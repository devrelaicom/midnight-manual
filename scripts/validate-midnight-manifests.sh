#!/usr/bin/env bash
# Validates the manifests/midnight/ ingestion set.
set -uo pipefail
dir="manifests/midnight"
expected=44
fail=0

mapfile -t files < <(find "$dir" -maxdepth 1 -name '*.yaml' | sort)
count=${#files[@]}

# 1. Directory-level rule: no `file:` leaves anywhere.
if grep -rnE '^[[:space:]]*file:[[:space:]]' "$dir"/*.yaml 2>/dev/null; then
  echo "FAIL: a 'file:' leaf was found — manifests must be directory-level"; fail=1
fi

# 2. Each file: schema + path-safety via the real loader, plus required provenance.
for f in "${files[@]}"; do
  if ! cargo run -q -p mn-cli --bin mnm -- manifest check "$f" >/dev/null 2>&1; then
    echo "FAIL: 'manifest check' rejected $f"; fail=1
  fi
  grep -q 'attribution:' "$f" || { echo "FAIL: no attribution in $f"; fail=1; }
  grep -q 'trust:'       "$f" || { echo "FAIL: no trust tag in $f"; fail=1; }
done

# 3. Exactly the expected number of manifests, and no excluded repo present.
[ "$count" -eq "$expected" ] || { echo "FAIL: expected $expected manifests, found $count"; fail=1; }
[ -e "$dir/olanetsoft-midnight-mcp.yaml" ] && { echo "FAIL: excluded repo present"; fail=1; }

echo "checked $count manifest(s); fail=$fail"
exit "$fail"
