#!/usr/bin/env bash
#
# Fail if cairn-core declares any cairn-* package as a dependency of any kind
# (normal, build, or dev). Core must stay a leaf: adapter crates never reach
# back into core, and core's own tests stay pure to keep this invariant
# trivially checkable.

set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v jq >/dev/null 2>&1; then
  echo "check-core-boundary: jq is required but not installed" >&2
  exit 2
fi

# Emit every dep declared by cairn-core whose name starts with `cairn-`,
# regardless of kind. An empty result means clean.
violations=$(
  cargo metadata --format-version 1 --locked \
    | jq -r '
        .packages[]
        | select(.name == "cairn-core")
        | .dependencies[]
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
