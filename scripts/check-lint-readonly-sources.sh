#!/usr/bin/env bash
# Enforces the §3 brief invariant — sources are immutable from Cairn's
# side. The `lint` verb must never open a source file for write.
#
# Issue #257 / 2026-05-10-source-link-hygiene-design.md component 12.
set -euo pipefail

# Walk every file under crates/cairn-core/src/verbs/lint/ and refuse any
# write-side syscall against the filesystem. Matches:
#   - std::fs::write / std::fs::OpenOptions::new().write(true)
#   - tokio::fs::write
#   - File::create
#
# Skip lines below `#[cfg(test)]` in each file — by convention the test
# module is the final item in the file, so everything after that marker
# is test-only scaffolding (tempdir fixtures, etc.) and not production
# read/write behaviour of the lint verb itself.
hits=""
while IFS= read -r -d '' file; do
  prod_only=$(awk '/^[[:space:]]*#\[cfg\(test\)\]/{exit} {print}' "$file")
  file_hits=$(echo "$prod_only" | grep -En \
    '(^|[^[:alnum:]_])(fs::write|OpenOptions::new\(\)[^;]*\.write\(true\)|File::create)\b' \
    || true)
  if [ -n "$file_hits" ]; then
    while IFS= read -r line; do
      hits+="${file}:${line}"$'\n'
    done <<< "$file_hits"
  fi
done < <(find crates/cairn-core/src/verbs/lint/ -name '*.rs' -print0)

if [ -n "$hits" ]; then
  echo "lint must never open source files for write — §3 invariant:"
  echo "$hits"
  exit 1
fi
