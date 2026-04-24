#!/usr/bin/env bash
# check-freeze.sh
#
# Enforces path freezes declared under .github/freezes/*.yaml. Freezes are
# set by maintainers when a hard split trigger fires (ADR 0001
# "Immediate containment when a hard trigger fires"). Any PR whose diff
# touches a frozen path fails this required status check.
#
# Executed from the BASE-branch checkout so PR-side edits to either the
# freeze files or this script cannot weaken the gate — see
# .github/workflows/governance.yml.
#
# Freeze file format:
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
# PyYAML is required and parses the file strictly; any malformed active
# freeze file fails the check (fail-closed).

set -euo pipefail

if [[ -z "${BASE_SHA:-}" || -z "${HEAD_SHA:-}" ]]; then
    echo "check-freeze: BASE_SHA and HEAD_SHA must be set" >&2
    exit 2
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "check-freeze: python3 is required" >&2
    exit 2
fi

if ! python3 -c 'import yaml' >/dev/null 2>&1; then
    echo "check-freeze: PyYAML is required (install via workflow step)" >&2
    exit 2
fi

git fetch --no-tags --depth=200 origin "${BASE_SHA}" "${HEAD_SHA}" >/dev/null 2>&1 || true

# --- 1. Diff once, reused below. -----------------------------------------
changed_files=$(git diff --name-only "${BASE_SHA}" "${HEAD_SHA}")
changed_name_status=$(git diff --name-status "${BASE_SHA}" "${HEAD_SHA}")

# --- 2. Freeze-removal exemption: strict JSON label match + diff scope. --
# The exemption applies only when:
#   - PR labels (parsed as strict JSON array) contain the EXACT string
#     'governance:freeze-removal', AND
#   - every changed path is under .github/freezes/, AND
#   - every changed path is a deletion (status 'D').
# Otherwise the freeze check runs normally.
exempt=0
if [[ -n "${PR_LABELS:-}" ]]; then
    set +e
    PR_LABELS="${PR_LABELS}" python3 -c '
import json, os, sys
raw = os.environ.get("PR_LABELS", "")
try:
    labels = json.loads(raw)
except Exception:
    sys.exit(2)
if not isinstance(labels, list):
    sys.exit(2)
sys.exit(0 if "governance:freeze-removal" in labels else 3)
'
    label_rc=$?
    set -e

    if [[ "${label_rc}" -eq 2 ]]; then
        echo "check-freeze: PR_LABELS is not a valid JSON array — fail-closed." >&2
        exit 1
    fi

    if [[ "${label_rc}" -eq 0 ]]; then
        diff_only_freeze_deletes=1
        while IFS=$'\t' read -r status path rest; do
            [[ -z "${status}" ]] && continue
            # Rename/copy statuses are prefixed 'R'/'C' with a similarity score.
            case "${status}" in
                D) ;;
                *) diff_only_freeze_deletes=0 ;;
            esac
            case "${path}" in
                .github/freezes/*) ;;
                *) diff_only_freeze_deletes=0 ;;
            esac
            [[ "${diff_only_freeze_deletes}" -eq 0 ]] && break
        done <<<"${changed_name_status}"

        if [[ "${diff_only_freeze_deletes}" -eq 1 ]]; then
            exempt=1
        else
            echo "check-freeze: governance:freeze-removal label present but diff is not purely freeze-file deletions — exemption denied." >&2
            echo "Changed files:" >&2
            printf '%s\n' "${changed_name_status}" >&2
            exit 1
        fi
    fi
fi

if [[ "${exempt}" -eq 1 ]]; then
    echo "check-freeze: governance:freeze-removal label + diff matches freeze-only deletions — exempt."
    exit 0
fi

# --- 3. Load active freezes; fail closed on any malformed file. ----------
freeze_dir=".github/freezes"

if [[ ! -d "${freeze_dir}" ]]; then
    echo "check-freeze: no ${freeze_dir}/ directory — no active freezes."
    exit 0
fi

shopt -s nullglob
freeze_files=("${freeze_dir}"/*.yaml "${freeze_dir}"/*.yml)
shopt -u nullglob

if [[ "${#freeze_files[@]}" -eq 0 ]]; then
    echo "check-freeze: no freeze YAML files present."
    exit 0
fi

parsed=$(
    python3 - "${freeze_files[@]}" <<'PY'
import sys, yaml, pathlib
status = 0
for p in sys.argv[1:]:
    text = pathlib.Path(p).read_text()
    try:
        doc = yaml.safe_load(text)
    except yaml.YAMLError as exc:
        print(f"ERROR\t{p}\tYAML parse error: {exc}", file=sys.stderr)
        status = 2
        continue
    if doc is None:
        print(f"ERROR\t{p}\tempty document", file=sys.stderr)
        status = 2
        continue
    if not isinstance(doc, dict):
        print(f"ERROR\t{p}\ttop-level must be a mapping", file=sys.stderr)
        status = 2
        continue
    paths = doc.get("paths")
    if not isinstance(paths, list) or not paths:
        print(f"ERROR\t{p}\t'paths:' must be a non-empty list", file=sys.stderr)
        status = 2
        continue
    for item in paths:
        if not isinstance(item, str) or not item.strip():
            print(f"ERROR\t{p}\tpath entries must be non-empty strings", file=sys.stderr)
            status = 2
            continue
        print(f"PATH\t{p}\t{item.strip()}")
sys.exit(status)
PY
) || {
    echo "check-freeze: FAIL — one or more freeze files failed strict parse (see above). Fail-closed." >&2
    exit 1
}

# --- 4. Check PR diff against frozen paths. ------------------------------
hit=""
while IFS=$'\t' read -r tag src pattern; do
    [[ "${tag}" != "PATH" ]] && continue
    while IFS= read -r file; do
        [[ -z "${file}" ]] && continue
        case "${file}" in
            "${pattern}"*)
                hit="${file} (frozen by ${pattern} in ${src})"
                break 2
                ;;
        esac
    done <<<"${changed_files}"
done <<<"${parsed}"

if [[ -n "${hit}" ]]; then
    cat >&2 <<EOF
check-freeze: FAIL — PR touches a frozen path.

Match: ${hit}

Active freeze files:
$(printf '  %s\n' "${freeze_files[@]}")

Freezes are declared under ADR 0001 hard-trigger containment. Resolve the
underlying hard trigger and remove the freeze file via a PR whose diff is
exclusively freeze-file deletions, labelled governance:freeze-removal.
EOF
    exit 1
fi

echo "check-freeze: OK — no frozen paths touched."
