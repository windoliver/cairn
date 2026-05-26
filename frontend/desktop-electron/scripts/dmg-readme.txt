Cairn — local agent memory
==========================

To install: drag Cairn.app into Applications.

UNSIGNED BUILD?
  If macOS says "Cairn can't be opened because Apple cannot check
  it for malicious software", right-click Cairn.app and choose
  Open. You only need to do this once. Signed releases bypass this.

First launch will fetch the embedding model (~130 MB) into
  ~/Library/Application Support/cairn/models

Vaults default to ~/Documents/cairn (you can change this).

Uninstall: open Terminal and run:
  bash "/Applications/Cairn.app/Contents/Resources/scripts/uninstall.sh"
Vaults are NEVER deleted by the uninstaller.
