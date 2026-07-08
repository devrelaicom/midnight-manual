#!/usr/bin/env bash
# Two-sinks rule egress gate: fail if a raw query variable is interpolated into
# a `tracing` macro. Only the two sanctioned Sentry sinks (`search.topic`, and
# the admin-only `search.query`) may carry query text — a `tracing::*!` call is
# NOT one of them. Widen the identifier alternation below as new query-bearing
# names appear.
#
# Heuristic, line-based (no rg -U): catches single-line interpolation of the
# known query-bearing identifiers. It is a backstop, not a proof — the primary
# guarantees are the trust-boundary `sinks_for` gate and the fail-closed
# scrubbers.
set -euo pipefail

if ! command -v rg >/dev/null 2>&1; then
    echo "ERROR: ripgrep (rg) is required for this gate but was not found on PATH." >&2
    exit 2
fi

# rg exits 0 if it finds a match (a violation), 1 if it finds none (clean), and
# 2 on a real error (e.g. bad regex). Distinguish them explicitly so a scanner
# error can never masquerade as "clean".
set +e
matches=$(rg -n --pcre2 \
    'tracing::(info|warn|error|debug|trace)!\([^)]*(\bq\.text\b|\breq\.query\b|queries\[[^]]*\]\.text|\bquery\s*=\s*%)' \
    crates/*/src)
status=$?
set -e

case "$status" in
    0)
        echo "ERROR: a tracing macro interpolates raw query text (two-sinks rule violation):" >&2
        echo "$matches" >&2
        exit 1
        ;;
    1)
        echo "ok: no query text in tracing macros"
        ;;
    *)
        echo "ERROR: ripgrep failed (exit $status) while scanning tracing sites." >&2
        exit "$status"
        ;;
esac
