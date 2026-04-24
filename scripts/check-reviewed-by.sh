#!/usr/bin/env bash
# check-reviewed-by.sh
#
# Enforces GOVERNANCE.md §5 on load-bearing PRs during the single-maintainer
# period. A load-bearing PR must have:
#   1. An APPROVED GitHub review on the CURRENT HEAD by a non-author account.
#   2. A `Reviewed-by:` trailer (commit-message or PR body) that names the
#      same GitHub account.
#
# The script MUST be executed from the base-branch checkout (trusted code),
# not from a PR-supplied workspace — see .github/workflows/governance.yml
# which uses `pull_request_target` with the base ref checked out. It never
# executes PR-supplied code; it only inspects PR metadata via the GitHub
# REST API and diff via git.
#
# Inputs (env):
#   GH_TOKEN           — GitHub token with read access to pull_requests.
#   GITHUB_REPOSITORY  — "owner/repo", supplied by GitHub Actions.
#   PR_NUMBER          — pull request number.
#   BASE_SHA, HEAD_SHA — commit SHAs (base = merge-base ancestor, head = PR tip).
#   PR_AUTHOR          — login of the PR author.
#   PR_BODY            — pull request description (unverified; text only).

set -euo pipefail

require_env() {
    local name
    for name in "$@"; do
        if [[ -z "${!name:-}" ]]; then
            echo "check-reviewed-by: required env var ${name} is empty" >&2
            exit 2
        fi
    done
}

require_env GH_TOKEN GITHUB_REPOSITORY PR_NUMBER BASE_SHA HEAD_SHA PR_AUTHOR

# Load-bearing path globs — keep in sync with GOVERNANCE.md §5.2.
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
    '.github/freezes/'
    'scripts/check-reviewed-by.sh'
    'scripts/check-freeze.sh'
)

# Ensure we have both SHAs locally (pull_request_target starts from base).
git fetch --no-tags --depth=200 origin "${BASE_SHA}" "${HEAD_SHA}" >/dev/null 2>&1 || true

changed_files=$(git diff --name-only "${BASE_SHA}" "${HEAD_SHA}")

touches_load_bearing=0
while IFS= read -r file; do
    [[ -z "${file}" ]] && continue
    for pattern in "${load_bearing_patterns[@]}"; do
        case "${file}" in
            "${pattern}"*) touches_load_bearing=1; break ;;
        esac
    done
    [[ "${touches_load_bearing}" -eq 1 ]] && break
done <<<"${changed_files}"

if [[ "${touches_load_bearing}" -eq 0 ]]; then
    echo "check-reviewed-by: no load-bearing paths touched — skipping."
    exit 0
fi

echo "check-reviewed-by: load-bearing paths touched; verifying Approved review + Reviewed-by trailer."

# --- 1. Fetch reviews via GitHub REST API ---------------------------------
api_base="https://api.github.com/repos/${GITHUB_REPOSITORY}"
reviews_json=$(
    curl --silent --show-error --fail \
        --header "Authorization: Bearer ${GH_TOKEN}" \
        --header "Accept: application/vnd.github+json" \
        --header "X-GitHub-Api-Version: 2022-11-28" \
        "${api_base}/pulls/${PR_NUMBER}/reviews?per_page=100"
)

# Collect the LATEST review state per reviewer (GitHub returns reviews in
# chronological order). We want: login lowercased, state, commit_id.
latest_reviews=$(
    printf '%s' "${reviews_json}" | python3 -c '
import json, sys
reviews = json.load(sys.stdin)
latest = {}
for r in reviews:
    user = (r.get("user") or {}).get("login") or ""
    if not user:
        continue
    latest[user.lower()] = {
        "state": r.get("state") or "",
        "commit_id": r.get("commit_id") or "",
    }
for login, data in latest.items():
    print(f"{login}\t{data[\"state\"]}\t{data[\"commit_id\"]}")
'
)

author_lc=$(printf '%s' "${PR_AUTHOR}" | tr '[:upper:]' '[:lower:]')

approving_reviewers=$(
    printf '%s\n' "${latest_reviews}" \
        | awk -F'\t' -v author="${author_lc}" -v head="${HEAD_SHA}" '
            $2 == "APPROVED" && $1 != author && $3 == head { print $1 }
        '
)

if [[ -z "${approving_reviewers}" ]]; then
    cat >&2 <<EOF
check-reviewed-by: FAIL — no APPROVED GitHub review on the current HEAD
(${HEAD_SHA}) from a non-author account.

Latest review states (login state commit_id):
${latest_reviews:-(none)}

During the single-maintainer period (GOVERNANCE.md §5), load-bearing PRs
require an external reviewer to leave an Approved review on the PR AND a
matching 'Reviewed-by:' trailer. The trailer alone is not sufficient.
Request a review and have the reviewer click Approve on the current HEAD.
EOF
    exit 1
fi

# --- 2. Verify Reviewed-by trailer names an approving reviewer -----------
trailers=$(
    {
        git log "${BASE_SHA}..${HEAD_SHA}" --format=%B
        printf '\n%s\n' "${PR_BODY:-}"
    } | grep -iE '^[[:space:]]*Reviewed-by:' || true
)

if [[ -z "${trailers}" ]]; then
    echo "check-reviewed-by: FAIL — no 'Reviewed-by:' trailer found on commits or in PR body." >&2
    exit 1
fi

# Extract GitHub handles from the trailer lines (any token after '@').
trailer_handles=$(
    printf '%s\n' "${trailers}" \
        | grep -oE '@[A-Za-z0-9][A-Za-z0-9-]{0,38}' \
        | sed 's/^@//' \
        | tr '[:upper:]' '[:lower:]' \
        | sort -u
)

if [[ -z "${trailer_handles}" ]]; then
    echo "check-reviewed-by: FAIL — Reviewed-by: trailer present but no @handle recognised." >&2
    echo "${trailers}" >&2
    exit 1
fi

matched=""
while IFS= read -r handle; do
    [[ -z "${handle}" ]] && continue
    if printf '%s\n' "${approving_reviewers}" | grep -qxF "${handle}"; then
        matched="${handle}"
        break
    fi
done <<<"${trailer_handles}"

if [[ -z "${matched}" ]]; then
    cat >&2 <<EOF
check-reviewed-by: FAIL — Reviewed-by: trailer does not name any approving
reviewer of the current HEAD.

Reviewed-by handles:
$(printf '%s\n' "${trailer_handles}")

Approving reviewers on HEAD=${HEAD_SHA}:
$(printf '%s\n' "${approving_reviewers}")
EOF
    exit 1
fi

echo "check-reviewed-by: OK — Approved review by @${matched} matches Reviewed-by: trailer."
