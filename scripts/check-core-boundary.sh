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
# excluding the self-referential dev-dep used to activate the
# `test-fixtures` feature for integration tests under `tests/`. An empty
# result means clean. The self-dep does not introduce adapter-crate
# coupling — it only flips a feature flag — so the invariant is preserved.
violations=$(
  cargo metadata --format-version 1 --locked \
    | jq -r '
        .packages[]
        | select(.name == "cairn-core")
        | .dependencies[]
        | select(.name | startswith("cairn-"))
        | select(.name != "cairn-core")
        | .name
      '
)

if [[ -n "$violations" ]]; then
  echo "FAIL: cairn-core depends on forbidden workspace crates:" >&2
  echo "$violations" | sed 's/^/  - /' >&2
  exit 1
fi

echo "cairn-core boundary OK"
