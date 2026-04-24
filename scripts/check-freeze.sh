#!/usr/bin/env bash
# check-freeze.sh
#
# Enforces path freezes declared under .github/freezes/*.yaml. Freezes are
# set by maintainers when a hard split trigger fires (ADR 0002
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
# Parse PR_LABELS as a strict JSON array once. Any malformed input
# fail-closes the check — do not fall through treating PR_LABELS as
# absent, since that would skip the mandatory label exemption logic.
parsed_labels=""
if [[ -n "${PR_LABELS:-}" ]]; then
    set +e
    parsed_labels=$(
        PR_LABELS="${PR_LABELS}" python3 -c '
import json, os, sys
raw = os.environ.get("PR_LABELS", "")
try:
    labels = json.loads(raw)
except Exception:
    sys.exit(2)
if not isinstance(labels, list) or not all(isinstance(x, str) for x in labels):
    sys.exit(2)
for label in labels:
    print(label)
'
    )
    label_rc=$?
    set -e
    if [[ "${label_rc}" -ne 0 ]]; then
        echo "check-freeze: PR_LABELS is not a valid JSON array of strings — fail-closed." >&2
        exit 1
    fi
fi

has_label() {
    local needle="$1"
    [[ -z "${parsed_labels}" ]] && return 1
    printf '%s\n' "${parsed_labels}" | grep -qxF "${needle}"
}

# --- 2a. Freeze-removal exemption ----------------------------------------
# Applies iff:
#   - PR is labelled exactly 'governance:freeze-removal', AND
#   - every changed path is a pure deletion under .github/freezes/.
if has_label "governance:freeze-removal"; then
    diff_only_freeze_deletes=1
    while IFS=$'\t' read -r status path rest; do
        [[ -z "${status}" ]] && continue
        # Strip similarity score from rename/copy statuses (e.g. R100).
        short_status="${status:0:1}"
        case "${short_status}" in
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
        echo "check-freeze: governance:freeze-removal label + diff matches freeze-only deletions — exempt."
        exit 0
    fi

    echo "check-freeze: governance:freeze-removal label present but diff is not purely freeze-file deletions — exemption denied." >&2
    echo "Changed files:" >&2
    printf '%s\n' "${changed_name_status}" >&2
    exit 1
fi

# --- 2b. Transition exemption (GOVERNANCE §5.4) --------------------------
# The second-maintainer transition PR needs to touch load-bearing
# governance + doc paths while an all-paths transition freeze is active.
# Applies iff:
#   - PR is labelled exactly 'governance:transition', AND
#   - the diff touches ONLY the explicitly-permitted transition scope:
#       MAINTAINERS.md
#       GOVERNANCE.md
#       .github/CODEOWNERS
#       docs/design/decisions/
#       docs/design/design-brief.md
#   - and does NOT modify any freeze file (the tamper guard below
#     still runs after this block).
if has_label "governance:transition"; then
    allowed_prefixes=(
        'MAINTAINERS.md'
        'GOVERNANCE.md'
        '.github/CODEOWNERS'
        'docs/design/decisions/'
        'docs/design/design-brief.md'
    )
    in_scope=1
    while IFS= read -r file; do
        [[ -z "${file}" ]] && continue
        ok=0
        for allowed in "${allowed_prefixes[@]}"; do
            case "${file}" in
                "${allowed}"*) ok=1; break ;;
            esac
        done
        if [[ "${ok}" -eq 0 ]]; then
            in_scope=0
            echo "check-freeze: governance:transition exemption denied — path '${file}' is outside the permitted transition scope." >&2
            break
        fi
    done <<<"${changed_files}"

    if [[ "${in_scope}" -eq 1 ]]; then
        echo "check-freeze: governance:transition label + diff within permitted transition scope — exempt."
        exit 0
    fi
    echo "Changed files:" >&2
    printf '%s\n' "${changed_files}" >&2
    exit 1
fi

# --- 2a. Freeze-file tamper guard. ---------------------------------------
# A PR that is NOT the exempt freeze-removal path must not edit, rename,
# or delete existing `.github/freezes/*.y?ml` files. New-file additions
# (status 'A') are allowed — that is how a maintainer opens a new freeze.
# Everything else (M, D, R, C, T) on those paths fails closed, because
# the fallthrough freeze-path check can only detect diffs touching the
# TARGET of a freeze, not the freeze file itself.
#
# This is the final gate before normal freeze evaluation, so an un-
# labelled PR that silently deletes an active freeze file is caught
# here even if it doesn't also touch the (now-unfrozen) target path.
tamper_hit=""
while IFS=$'\t' read -r status path rest; do
    [[ -z "${status}" ]] && continue
    case "${path}" in
        .github/freezes/*.yaml|.github/freezes/*.yml) ;;
        *) continue ;;
    esac
    # Strip similarity score from rename/copy statuses (e.g. R100).
    short_status="${status:0:1}"
    case "${short_status}" in
        A) ;;
        *) tamper_hit="${status}	${path}"; break ;;
    esac
done <<<"${changed_name_status}"

if [[ -n "${tamper_hit}" ]]; then
    cat >&2 <<EOF
check-freeze: FAIL — PR modifies or deletes an existing freeze file
without the exempt freeze-removal protocol.

Offending change: ${tamper_hit}

Freeze files under .github/freezes/*.yaml may only be:
  - added (status A) — to declare a new freeze, OR
  - deleted (status D) as the ENTIRE diff, with the PR labelled
    'governance:freeze-removal'.

Editing, renaming, or partially deleting an existing freeze file is not
permitted; open a freeze-removal PR then a separate freeze-addition PR
if the freeze needs to change.
EOF
    exit 1
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
# Path semantics: a `paths:` entry is a prefix match on the repository-
# relative path. The single entry `**` is a sentinel meaning "match
# every path" (used by the transition freeze in GOVERNANCE.md §5.4).
hit=""
while IFS=$'\t' read -r tag src pattern; do
    [[ "${tag}" != "PATH" ]] && continue
    if [[ "${pattern}" == "**" ]]; then
        # Any changed file is a hit.
        first_file=$(printf '%s\n' "${changed_files}" | sed -n '1p')
        if [[ -n "${first_file}" ]]; then
            hit="${first_file} (frozen by ** sentinel in ${src})"
            break
        fi
        continue
    fi
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

Freezes are declared under ADR 0002 hard-trigger containment. Resolve the
underlying hard trigger and remove the freeze file via a PR whose diff is
exclusively freeze-file deletions, labelled governance:freeze-removal.
EOF
    exit 1
fi

echo "check-freeze: OK — no frozen paths touched."
