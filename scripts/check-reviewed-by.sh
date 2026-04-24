#!/usr/bin/env bash
# check-reviewed-by.sh
#
# Enforces GOVERNANCE.md §5 on load-bearing PRs during the single-maintainer
# period. A load-bearing PR must carry a `Reviewed-by:` trailer (either on
# the tip commit or in the PR body) naming a non-author GitHub handle.
#
# Inputs (env):
#   BASE_SHA, HEAD_SHA, PR_BODY, PR_AUTHOR — supplied by the workflow.
#
# The script is conservative: if inputs are missing, it fails closed.

set -euo pipefail

if [[ -z "${BASE_SHA:-}" || -z "${HEAD_SHA:-}" ]]; then
    echo "check-reviewed-by: BASE_SHA and HEAD_SHA must be set" >&2
    exit 2
fi

# Load-bearing path globs — keep in sync with GOVERNANCE.md §5 and
# CLAUDE.md §9.
load_bearing_patterns=(
    'crates/cairn-core/src/'
    'crates/cairn-core/Cargo.toml'
    'crates/cairn-idl/'
    'crates/cairn-store-sqlite/migrations/'
    'docs/design/design-brief.md'
    'docs/design/decisions/'
    'GOVERNANCE.md'
    'MAINTAINERS.md'
    '.github/CODEOWNERS'
    '.github/workflows/governance.yml'
    'scripts/check-reviewed-by.sh'
)

changed_files=$(git diff --name-only "${BASE_SHA}" "${HEAD_SHA}")

touches_load_bearing=0
while IFS= read -r file; do
    for pattern in "${load_bearing_patterns[@]}"; do
        case "${file}" in
            "${pattern}"*) touches_load_bearing=1; break ;;
        esac
    done
done <<<"${changed_files}"

if [[ "${touches_load_bearing}" -eq 0 ]]; then
    echo "check-reviewed-by: no load-bearing paths touched — skipping."
    exit 0
fi

echo "check-reviewed-by: load-bearing paths touched; requiring Reviewed-by: trailer."

# Collect Reviewed-by trailers from commits AND the PR body.
trailers=$(
    {
        git log "${BASE_SHA}..${HEAD_SHA}" --format=%B
        printf '\n%s\n' "${PR_BODY:-}"
    } | grep -iE '^[[:space:]]*Reviewed-by:' || true
)

if [[ -z "${trailers}" ]]; then
    cat >&2 <<EOF
check-reviewed-by: FAIL

This PR touches load-bearing paths (GOVERNANCE.md §5 / CLAUDE.md §9) but
carries no 'Reviewed-by:' trailer. During the single-maintainer period,
required-reviewer branch protection is disabled because GitHub disallows
self-approval. Load-bearing PRs must instead be reviewed by an external
reviewer who leaves an approving review on the PR and is recorded via:

    Reviewed-by: <Full Name> <@github-handle or email>

Add the trailer to either the merge commit body or the PR description.
EOF
    exit 1
fi

# Reject trailers whose GitHub handle matches the PR author (case-insensitive).
if [[ -n "${PR_AUTHOR:-}" ]]; then
    lowered_author=$(printf '%s' "${PR_AUTHOR}" | tr '[:upper:]' '[:lower:]')
    self_review=$(
        printf '%s\n' "${trailers}" \
            | tr '[:upper:]' '[:lower:]' \
            | grep -E "@?${lowered_author}([^[:alnum:]_-]|$)" || true
    )
    if [[ -n "${self_review}" ]]; then
        echo "check-reviewed-by: FAIL — Reviewed-by: trailer names the PR author (${PR_AUTHOR}); external reviewer required." >&2
        exit 1
    fi
fi

echo "check-reviewed-by: OK"
printf '%s\n' "${trailers}"
