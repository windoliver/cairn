#!/usr/bin/env bash
# check-freeze.sh
#
# Enforces path freezes declared under .github/freezes/*.yaml. Freezes are
# set by maintainers when a hard split trigger fires (ADR 0001
# "Immediate containment when a hard trigger fires"). Any PR whose diff
# touches a frozen path fails this required status check.
#
# This script must run from the BASE-branch checkout so PR-side edits to
# either the freeze files or this script cannot weaken the gate — see
# .github/workflows/governance.yml.
#
# Freeze file format (one file per active freeze):
#
#   # .github/freezes/2026-04-24-hard-trigger-h2-store.yaml
#   trigger: H2
#   issue: https://github.com/windoliver/cairn/issues/NNN
#   owner: "@windoliver"
#   opened: 2026-04-24
#   deadline: 2026-05-08
#   paths:
#     - crates/cairn-store-sqlite/
#     - crates/cairn-store-sqlite/Cargo.toml
#
# The `paths:` list is the only field this script parses; all other fields
# are human audit context. Freezes are removed by merging a PR that deletes
# the file (that PR itself may need the freeze override — see §5 of the
# ADR — and in that case must be labelled `governance:freeze-removal` which
# exempts it).

set -euo pipefail

if [[ -z "${BASE_SHA:-}" || -z "${HEAD_SHA:-}" ]]; then
    echo "check-freeze: BASE_SHA and HEAD_SHA must be set" >&2
    exit 2
fi

# Exempt freeze-removal PRs (they delete freeze files; blocking them would
# prevent the freeze from ever being lifted). Detection: PR carries label
# `governance:freeze-removal` OR the diff is purely deletions under
# .github/freezes/ with no other paths touched.
if [[ -n "${PR_LABELS:-}" ]]; then
    if printf '%s' "${PR_LABELS}" | tr '[:upper:]' '[:lower:]' \
            | grep -qw 'governance:freeze-removal'; then
        echo "check-freeze: governance:freeze-removal label detected — exempt."
        exit 0
    fi
fi

freeze_dir=".github/freezes"

if [[ ! -d "${freeze_dir}" ]]; then
    echo "check-freeze: no ${freeze_dir}/ directory — no active freezes."
    exit 0
fi

shopt -s nullglob
freeze_files=("${freeze_dir}"/*.yaml "${freeze_dir}"/*.yml)
shopt -u nullglob

# Ignore a placeholder README if present.
active_files=()
for f in "${freeze_files[@]}"; do
    [[ "$(basename "${f}")" == ".gitkeep"* ]] && continue
    active_files+=("${f}")
done

if [[ "${#active_files[@]}" -eq 0 ]]; then
    echo "check-freeze: no active freeze files."
    exit 0
fi

# Collect frozen paths from YAML `paths:` lists (simple parser: items under
# a `paths:` key formatted as `  - some/path/`).
frozen_paths=()
for f in "${active_files[@]}"; do
    while IFS= read -r path; do
        [[ -z "${path}" ]] && continue
        frozen_paths+=("${path}")
    done < <(
        python3 - "${f}" <<'PY'
import sys, re
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
in_paths = False
for line in text.splitlines():
    stripped = line.rstrip()
    if re.match(r'^paths\s*:\s*$', stripped):
        in_paths = True
        continue
    if in_paths:
        m = re.match(r'^\s*-\s*"?([^"\s]+)"?\s*$', stripped)
        if m:
            print(m.group(1))
            continue
        if stripped and not stripped.startswith(' '):
            in_paths = False
PY
    )
done

if [[ "${#frozen_paths[@]}" -eq 0 ]]; then
    echo "check-freeze: freeze files present but no paths declared — treating as inactive."
    exit 0
fi

git fetch --no-tags --depth=200 origin "${BASE_SHA}" "${HEAD_SHA}" >/dev/null 2>&1 || true
changed_files=$(git diff --name-only "${BASE_SHA}" "${HEAD_SHA}")

hit=""
while IFS= read -r file; do
    [[ -z "${file}" ]] && continue
    for frozen in "${frozen_paths[@]}"; do
        case "${file}" in
            "${frozen}"*) hit="${file} (frozen by pattern ${frozen})"; break 2 ;;
        esac
    done
done <<<"${changed_files}"

if [[ -n "${hit}" ]]; then
    cat >&2 <<EOF
check-freeze: FAIL — PR touches a frozen path.

Match: ${hit}

Active freezes (from ${freeze_dir}/):
$(printf '  %s\n' "${active_files[@]}")

Freezes are declared under ADR 0001 hard-trigger containment. Resolve the
underlying hard trigger and remove the freeze file via a PR labelled
governance:freeze-removal before merging changes that touch these paths.
EOF
    exit 1
fi

echo "check-freeze: OK — no frozen paths touched."
