#!/usr/bin/env bash
# Cairn desktop uninstall helper.
#
# Removes regenerable state from ~/Library/Application Support/cairn
# (models, logs). Preserves vault_registry.json by default so a
# reinstall remembers the user's vaults; --full removes that too.
#
# Vault directories are NEVER touched. The registry is read only to
# print a "your vaults remain at: ..." message for the user.
#
# Usage:
#   uninstall.sh [--yes] [--full] [--app-support PATH]

set -euo pipefail

YES=0
FULL=0
APP_SUPPORT="${HOME}/Library/Application Support/cairn"

while [ $# -gt 0 ]; do
    case "$1" in
        --yes) YES=1 ;;
        --full) FULL=1 ;;
        --app-support) APP_SUPPORT="$2"; shift ;;
        *) echo "unknown arg: $1" >&2; exit 64 ;;
    esac
    shift
done

REGISTRY="${APP_SUPPORT}/vault_registry.json"

if [ ! -d "$APP_SUPPORT" ]; then
    echo "nothing to uninstall: $APP_SUPPORT does not exist"
    exit 0
fi

# Read vault paths from registry (informational only — printed, never deleted).
if [ -f "$REGISTRY" ]; then
    # Use python3 (ships with macOS) for robust JSON parsing.
    VAULT_PATHS=$(python3 -c "
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    for v in d.get('vaults', []):
        print(v.get('path', ''))
except Exception:
    pass
" "$REGISTRY")
else
    VAULT_PATHS=""
fi

echo "Cairn uninstall"
echo "==============="
echo "App support : $APP_SUPPORT"
if [ -n "$VAULT_PATHS" ]; then
    echo "Your vaults (NOT touched by this script):"
    echo "$VAULT_PATHS" | sed 's/^/  /'
fi
echo
echo "Will remove: models/, logs/, desktop.log"
if [ "$FULL" -eq 1 ]; then
    echo "Will remove: vault_registry.json (--full)"
fi

if [ "$YES" -ne 1 ]; then
    printf "Proceed? [y/N] "
    read -r ans
    case "$ans" in
        y|Y|yes|YES) ;;
        *) echo "aborted"; exit 1 ;;
    esac
fi

# Delete regenerable state. Note: every rm target is a literal path
# under $APP_SUPPORT — no $VAULT_PATHS reference in any rm command.
rm -rf -- "${APP_SUPPORT}/models"
rm -rf -- "${APP_SUPPORT}/logs"
rm -f  -- "${APP_SUPPORT}/desktop.log"

if [ "$FULL" -eq 1 ]; then
    rm -f -- "${APP_SUPPORT}/vault_registry.json"
fi

echo "done."
echo
echo "To finish: drag /Applications/Cairn.app to the Trash."
