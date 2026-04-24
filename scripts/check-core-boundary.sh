#!/usr/bin/env bash
#
# Fail if cairn-core's resolved dependency graph contains any cairn-* package.
# Dev-deps are ignored (fixtures can be consumed in tests); runtime and
# build-script deps are checked.

set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v jq >/dev/null 2>&1; then
  echo "check-core-boundary: jq is required but not installed" >&2
  exit 2
fi

# Emit every dep of cairn-core whose kind is normal (null) or build, filtered
# to names that start with `cairn-`. An empty result means clean.
violations=$(
  cargo metadata --format-version 1 --locked \
    | jq -r '
        .packages[]
        | select(.name == "cairn-core")
        | .dependencies[]
        | select((.kind // "normal") == "normal" or .kind == "build")
        | .name
        | select(startswith("cairn-"))
      '
)

if [[ -n "$violations" ]]; then
  echo "FAIL: cairn-core depends on forbidden workspace crates:" >&2
  echo "$violations" | sed 's/^/  - /' >&2
  exit 1
fi

echo "cairn-core boundary OK"
