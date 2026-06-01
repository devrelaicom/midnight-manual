#!/usr/bin/env bash
#
# ingest-midnight.sh — manually ingest the default Midnight corpus.
#
# Builds `mnm` from source (release), then loops over the source list in
# manifests/midnight/sources.tsv: shallow-clones each repo into /tmp, registers
# the source, and runs `mnm ingest run` against a fresh checkout. Per-manifest
# failures are reported and skipped — the loop never aborts on them.
#
# Prerequisites (checked up front):
#   * a reachable midnight-manual server (its /readyz returns 200)
#   * an existing auth.toml (you have logged in at least once)
#   * a local keypair for --user-id so `mnm login` can refresh the admin token
#
# Usage:
#   scripts/ingest-midnight.sh [--user-id <id>] [options]
#
# Options:
#   --user-id <id>        User id for `mnm login`. If omitted, the [admin] user
#                         recorded in auth.toml is used (after a Y/n confirm).
#   --server <url>        Server base URL. If omitted, no --server is passed to
#                         mnm — it uses its own configured/built-in default
#                         (shown as "server: <url>" before ingestion starts).
#   --mnm-binary <path>   Use this prebuilt mnm and skip the cargo build.
#   --max-manifests <n>   Process only the first <n> manifests; skip the rest.
#   --sources <path>      Source list (default: manifests/midnight/sources.tsv).
#   --manifests-dir <dir> Manifest dir (default: manifests/midnight).
#   -h, --help            Show this help.

set -uo pipefail   # NOT -e: we deliberately continue past per-manifest errors.

# ── defaults ──────────────────────────────────────────────────────────────────
SERVER=""   # empty = don't pass --server; mnm uses its configured/built-in default
USER_ID=""
MNM_BIN=""
MAX_MANIFESTS=""
SOURCES="manifests/midnight/sources.tsv"
MANIFESTS_DIR="manifests/midnight"
CLONE_BASE="/tmp/mn-ingest"

usage() { sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; }

# ── args ────────────────────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
  case "$1" in
    --user-id)        USER_ID="${2:-}"; shift 2 ;;
    --server)         SERVER="${2:-}"; shift 2 ;;
    --mnm-binary)     MNM_BIN="${2:-}"; shift 2 ;;
    --max-manifests)  MAX_MANIFESTS="${2:-}"; shift 2 ;;
    --sources)        SOURCES="${2:-}"; shift 2 ;;
    --manifests-dir)  MANIFESTS_DIR="${2:-}"; shift 2 ;;
    -h|--help)        usage; exit 0 ;;
    *) echo "error: unknown argument '$1' (try --help)" >&2; exit 2 ;;
  esac
done
SERVER="${SERVER%/}"
# Only forward --server to mnm when the user set one; otherwise let mnm resolve.
if [ -n "$SERVER" ]; then SERVER_ARGS=(--server "$SERVER"); else SERVER_ARGS=(); fi

if [ -n "$MAX_MANIFESTS" ] && ! [[ "$MAX_MANIFESTS" =~ ^[0-9]+$ ]]; then
  echo "error: --max-manifests must be a non-negative integer" >&2; exit 2
fi
if [ ! -f "$SOURCES" ]; then
  echo "error: source list not found: $SOURCES" >&2; exit 2
fi

# ── colours / tty ─────────────────────────────────────────────────────────────
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  RED=$'\033[31m'; GREEN=$'\033[32m'; BLUE=$'\033[34m'; DIM=$'\033[2m'; NC=$'\033[0m'; TTY=1
else
  RED=""; GREEN=""; BLUE=""; DIM=""; NC=""; TTY=0
fi
CHECK="✓"; XMARK="x"; SKIP="–"

TMP="$(mktemp -d)"
cleanup() { rm -rf "$TMP"; [ "$TTY" = 1 ] && printf '\033[?25h'; }  # restore cursor
trap cleanup EXIT
[ "$TTY" = 1 ] && printf '\033[?25l'  # hide cursor during spinners

# ── helpers ───────────────────────────────────────────────────────────────────

# Group an integer with thousands separators: 1456 -> 1,456.
# awk-based (portable: BSD/macOS sed lacks the GNU `:a;…;ta` branch form).
commafy() {
  printf '%s' "$1" | awk '{
    x=$0; o="";
    while (length(x) > 3) { o = "," substr(x, length(x)-2) o; x = substr(x, 1, length(x)-3); }
    printf "%s%s", x, o;
  }'
}

# Derive a human status from the streamed JSONL events file (no jq dependency).
derive_status() {
  local last; last="$(tail -n 40 "$1" 2>/dev/null)"
  case "$last" in
    *'"phase":"finalize"'*) printf 'finalizing'; return ;;
  esac
  local up; up="$(printf '%s\n' "$last" | grep '"phase":"uploading documents"' | tail -1)"
  if [ -n "$up" ]; then
    local c o
    c="$(printf '%s' "$up" | grep -oE '"current":[0-9]+' | grep -oE '[0-9]+')"
    o="$(printf '%s' "$up" | grep -oE '"of":[0-9]+' | grep -oE '[0-9]+')"
    printf 'uploading %s/%s' "${c:-?}" "${o:-?}"; return
  fi
  if printf '%s\n' "$last" | grep -q '"phase":"upload_documents"'; then printf 'uploading'; return; fi
  if printf '%s\n' "$last" | grep -qE '"phase":"chunk".*"chunks"'; then printf 'preparing upload'; return; fi
  if printf '%s\n' "$last" | grep -q '"phase":"chunk"'; then printf 'chunking'; return; fi
  if printf '%s\n' "$last" | grep -q 'source_creating'; then printf 'creating source'; return; fi
  if printf '%s\n' "$last" | grep -qE '"phase":"walk".*"files"'; then printf 'chunking'; return; fi
  if printf '%s\n' "$last" | grep -q '"phase":"walk"'; then printf 'walking source'; return; fi
  printf 'starting'
}

# Extract the single [admin] user_id from an auth.toml (no TOML parser needed).
user_from_toml() {
  awk '
    /^\[admin\]/      { in_admin = 1; next }
    /^\[/             { in_admin = 0 }
    in_admin && /^[[:space:]]*user_id[[:space:]]*=/ {
      sub(/^[^=]*=[[:space:]]*"?/, ""); sub(/".*$/, ""); sub(/[[:space:]]*$/, "");
      print; exit
    }
  ' "$1"
}

# Resolve the server URL mnm will actually use (config file / env / compiled
# default) by asking the built binary — so the readyz preflight targets the same
# address mnm will, without the script hardcoding any default.
server_from_mnm() {
  "$MNM" config show 2>/dev/null | awk '
    /^\[server\]/ { in_s = 1; next }
    /^\[/         { in_s = 0 }
    in_s && /^[[:space:]]*url[[:space:]]*=/ {
      sub(/^[^=]*=[[:space:]]*"?/, ""); sub(/".*$/, ""); sub(/[[:space:]]*$/, ""); print; exit
    }
  '
}

# readyz preflight against a base URL; exit 1 if not ready.
readyz_or_exit() {
  if ! curl -fsS --max-time 10 "$1/readyz" >/dev/null 2>&1; then
    echo "${RED}preflight failed:${NC} server not ready at $1/readyz" >&2
    echo "  (is the server up and the database reachable? pass --server <url> if needed)" >&2
    exit 1
  fi
}

# ── 1. preflight ──────────────────────────────────────────────────────────────
# auth.toml lives at ${XDG_CONFIG_HOME:-$HOME/.config}/midnight-manual/auth.toml
AUTH_TOML="${XDG_CONFIG_HOME:-$HOME/.config}/midnight-manual/auth.toml"
if [ ! -f "$AUTH_TOML" ]; then
  echo "${RED}preflight failed:${NC} auth.toml not found at $AUTH_TOML" >&2
  echo "  (run 'mnm login --user-id <id>' once to create it)" >&2
  exit 1
fi

# Resolve the user: explicit --user-id wins; otherwise use the single
# [admin].user_id from auth.toml and confirm (auth.toml is not multi-user).
if [ -z "$USER_ID" ]; then
  derived="$(user_from_toml "$AUTH_TOML")"
  if [ -z "$derived" ]; then
    echo "${RED}error:${NC} no [admin] user_id in $AUTH_TOML — pass --user-id <id>" >&2
    exit 2
  fi
  if [ -t 0 ]; then
    printf 'Do you want to ingest files as %s%s%s user? (Y/n) ' "$BLUE" "$derived" "$NC"
    read -r reply || reply=""
    case "${reply:-Y}" in
      [Nn]*) echo "aborted — pass --user-id <id> to choose a different user"; exit 0 ;;
    esac
    USER_ID="$derived"
  else
    echo "${RED}error:${NC} no --user-id given and not an interactive terminal; pass --user-id <id>" >&2
    exit 2
  fi
fi

# ── 2. build (or use prebuilt) mnm ────────────────────────────────────────────
if [ -n "$MNM_BIN" ]; then
  MNM="$MNM_BIN"
  [ -x "$MNM" ] || { echo "error: --mnm-binary '$MNM' is not an executable" >&2; exit 1; }
  printf 'using: %s\n' "$MNM"
else
  printf '%sbuilding mnm (cargo build --release -p mn-cli --bin mnm)…%s\n' "$DIM" "$NC"
  if ! cargo build --release -p mn-cli --bin mnm; then
    echo "error: cargo build failed" >&2; exit 1
  fi
  MNM="./target/release/mnm"
  printf 'built: %s\n' "$MNM"
fi
VER="$("$MNM" --version 2>/dev/null | awk '{print $NF}')"
printf 'version: %s\n' "${VER:-unknown}"

# ── 2b. resolve effective server + readyz preflight (now that mnm is available) ─
if [ -n "$SERVER" ]; then
  SERVER_URL="$SERVER"
else
  SERVER_URL="$(server_from_mnm)"
  if [ -z "$SERVER_URL" ]; then
    echo "${RED}preflight failed:${NC} could not resolve server URL from 'mnm config show' (pass --server <url>)" >&2
    exit 1
  fi
fi
printf 'server: %s\n\n' "$SERVER_URL"
readyz_or_exit "$SERVER_URL"

# ── 3. login (refresh admin token) ────────────────────────────────────────────
if ! "$MNM" "${SERVER_ARGS[@]+"${SERVER_ARGS[@]}"}" login --user-id "$USER_ID" >/dev/null 2>"$TMP/login.err"; then
  echo "${RED}login failed:${NC} $(tail -n 5 "$TMP/login.err" | grep -v '^[[:space:]]*$' | tail -1)" >&2
  echo "  (need a local keypair for '$USER_ID' and a matching user on the server)" >&2
  exit 1
fi

# ── 4. load source list ───────────────────────────────────────────────────────
rows=()
while IFS=$'\t' read -r slug repo branch kind; do
  [ -z "${slug:-}" ] && continue
  rows+=("$slug"$'\t'"$repo"$'\t'"$branch"$'\t'"$kind")
done < <(awk 'NF>=4 && $1 !~ /^#/ {print $1"\t"$2"\t"$3"\t"$4}' "$SOURCES")
total=${#rows[@]}
limit=$total
if [ -n "$MAX_MANIFESTS" ] && [ "$MAX_MANIFESTS" -lt "$total" ]; then limit=$MAX_MANIFESTS; fi

INGESTED=0; FAILED=0; SKIPPED=$(( total - limit ))
mkdir -p "$CLONE_BASE"

# ── 5. ingest each manifest ───────────────────────────────────────────────────
idx=0
for row in "${rows[@]}"; do
  idx=$((idx+1))
  if [ "$idx" -gt "$limit" ]; then break; fi

  slug="$(printf '%s' "$row" | cut -f1)"
  repo="$(printf '%s' "$row" | cut -f2)"
  branch="$(printf '%s' "$row" | cut -f3)"
  kind="$(printf '%s' "$row" | cut -f4)"
  url="https://github.com/$repo"
  man="$MANIFESTS_DIR/$slug.yaml"
  clone="$CLONE_BASE/$slug"

  if [ ! -f "$man" ]; then
    printf '%s%s %s%s failed • manifest %s not found\n' "$RED" "$XMARK" "$repo" "$NC" "$man"
    FAILED=$((FAILED+1)); continue
  fi

  # fresh shallow clone
  rm -rf "$clone"
  if ! git clone --depth=1 -b "$branch" "$url" "$clone" >/dev/null 2>"$TMP/clone.err"; then
    printf '%s%s %s%s failed • repo not found or branch '"'"'%s'"'"' missing\n' "$RED" "$XMARK" "$repo" "$NC" "$branch"
    FAILED=$((FAILED+1)); continue
  fi

  # best-effort source registration with the correct kind (tolerate "exists")
  "$MNM" "${SERVER_ARGS[@]+"${SERVER_ARGS[@]}"}" sources create \
      --slug "$slug" --kind "$kind" --origin-url "$url" >/dev/null 2>&1 || true

  # run the ingest, streaming JSONL events to a temp file
  ev="$TMP/$slug.jsonl"; er="$TMP/$slug.err"; : >"$ev"; : >"$er"
  start=$SECONDS
  "$MNM" "${SERVER_ARGS[@]+"${SERVER_ARGS[@]}"}" ingest run "$man" \
      --source-slug "$slug" --source-root "$clone" --yes --json >"$ev" 2>"$er" &
  pid=$!

  if [ "$TTY" = 1 ]; then
    frames=(⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏); fi=0   # array, not substring: char-safe on bash 3.2
    while kill -0 "$pid" 2>/dev/null; do
      st="$(derive_status "$ev")"
      printf '\r\033[K%s%s%s ingesting %s %s→ %s%s' \
        "$BLUE" "${frames[fi%10]}" "$NC" "$repo" "$DIM" "$st" "$NC"
      fi=$((fi+1)); sleep 0.1
    done
  fi
  wait "$pid"; rc=$?
  elapsed=$((SECONDS - start))

  files="$(grep '"phase":"walk"' "$ev" 2>/dev/null | grep -oE '"files":[0-9]+' | grep -oE '[0-9]+' | tail -1)"; files="${files:-0}"
  chunks="$(grep '"phase":"chunk"' "$ev" 2>/dev/null | grep -oE '"chunks":[0-9]+' | grep -oE '[0-9]+' | tail -1)"; chunks="${chunks:-0}"

  [ "$TTY" = 1 ] && printf '\r\033[K'
  if [ "$rc" -eq 0 ]; then
    printf '%s%s %s%s ingested • %s files • %s chunks • %ss\n' \
      "$GREEN" "$CHECK" "$repo" "$NC" "$(commafy "$files")" "$(commafy "$chunks")" "$elapsed"
    INGESTED=$((INGESTED+1))
  else
    why="$(tail -n 20 "$er" | grep -v '^[[:space:]]*$' | tail -1)"; why="${why:-"ingest exited with code $rc"}"
    printf '%s%s %s%s failed • %s\n' "$RED" "$XMARK" "$repo" "$NC" "$why"
    FAILED=$((FAILED+1))
  fi
done

# ── 6. summary ────────────────────────────────────────────────────────────────
if [ "$SKIPPED" -gt 0 ]; then
  printf '%s%s skipped %d manifest(s) (--max-manifests=%s)%s\n' "$DIM" "$SKIP" "$SKIPPED" "$MAX_MANIFESTS" "$NC"
fi
printf '\n%s%d ingested%s · %s%d failed%s · %s%d skipped%s\n' \
  "$GREEN" "$INGESTED" "$NC" "$RED" "$FAILED" "$NC" "$DIM" "$SKIPPED" "$NC"

[ "$FAILED" -gt 0 ] && exit 1 || exit 0
