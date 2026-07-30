#!/usr/bin/env bash
# Validates the manifests/midnight/ ingestion set: schema + path safety via the
# real loader, required provenance on every manifest, and agreement between
# sources.tsv and the manifest files on disk.
#
# Scope: this runs WITHOUT repo checkouts, so `file:`-leaf existence cannot be
# verified here and `missing_file` issues are ignored (see rule 1). Existence is
# enforced by .github/workflows/manifest-smoke.yml, which re-runs
# `manifest check --base <clone>` against fresh clones every day.
set -uo pipefail

dir="manifests/midnight"
sources="$dir/sources.tsv"
fail=0

command -v jq >/dev/null || { echo "FAIL: jq is required"; exit 1; }

# Build once. Invoking `cargo run` per manifest re-resolves the whole workspace
# 100+ times and dominates the runtime.
cargo build -q -p midnight-manual --bin mnm || { echo "FAIL: cargo build"; exit 1; }
mnm="target/debug/mnm"

# Every manifest, including the categorised partner/ and community/ sets
# (-maxdepth 2). Slugs are unique across folders, so basenames identify them.
mapfile -t files < <(find "$dir" -maxdepth 2 -name '*.yaml' | sort)
count=${#files[@]}
[ "$count" -gt 0 ] || { echo "FAIL: no manifests found under $dir"; exit 1; }

# 1. Per-manifest: schema + path safety via the real loader, plus the provenance
#    fields the corpus's confidence scoring depends on. `missing_file` is
#    filtered out per the scope note; `parse_error`, `unsafe_path` and
#    `duplicate_file` are base-independent and still fail the run.
for f in "${files[@]}"; do
  json=$("$mnm" manifest check "$f" --json 2>/dev/null)
  if [ -z "$json" ]; then
    echo "FAIL: 'manifest check' produced no report for $f"; fail=1
  else
    blocking=$(printf '%s' "$json" | jq -r '
      [.issues[]? | select(.blocking and .kind != "missing_file")
                  | "\(.kind) (\(.path // "-"))"] | join("; ")')
    [ -n "$blocking" ] && { echo "FAIL: 'manifest check' rejected $f — $blocking"; fail=1; }
  fi
  grep -q 'attribution:' "$f" || { echo "FAIL: no attribution in $f"; fail=1; }
  grep -q 'trust:'       "$f" || { echo "FAIL: no trust tag in $f"; fail=1; }
done

# 2. sources.tsv <-> manifest files agree, the same invariant manifest-smoke.yml
#    enforces. This subsumes the old hardcoded manifest count: it catches a
#    missing row, a missing file, and a slug/filename mismatch alike.
slugs=$(awk 'NF>=4 && $1 !~ /^#/ {print $1}' "$sources" | sort)
basenames=$(printf '%s\n' "${files[@]}" | sed 's|.*/||; s|\.yaml$||' | sort)
if ! diff <(printf '%s\n' "$slugs") <(printf '%s\n' "$basenames") >/dev/null; then
  echo "FAIL: sources.tsv slugs and manifest files disagree (< sources.tsv, > *.yaml):"
  diff <(printf '%s\n' "$slugs") <(printf '%s\n' "$basenames")
  fail=1
fi

# 3. sources.tsv shape: every data row carries all four columns, no duplicate
#    slugs. A row short a column silently drops out of every awk-driven
#    consumer (this script, the smoke workflow, ingest-midnight.sh).
awk -F'\t' '
  $1 ~ /^#/ || NF == 0 { next }
  NF != 4 { printf "FAIL: sources.tsv line %d has %d field(s), want 4 (tab-separated): %s\n", NR, NF, $0; rc = 1 }
  END { exit rc }
' "$sources" || fail=1
dupes=$(printf '%s\n' "$slugs" | uniq -d)
[ -n "$dupes" ] && { echo "FAIL: duplicate slug(s) in sources.tsv: $(printf '%s' "$dupes" | tr '\n' ' ')"; fail=1; }

# 4. Repos deliberately kept out of the corpus must not reappear.
if [ -e "$dir/olanetsoft-midnight-mcp.yaml" ]; then
  echo "FAIL: excluded repo present"; fail=1
fi

echo "checked $count manifest(s) against $(printf '%s\n' "$slugs" | grep -c .) source row(s); fail=$fail"
exit "$fail"
