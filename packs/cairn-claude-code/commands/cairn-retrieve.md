---
description: Direct Cairn `retrieve` verb.
argument-hint: "<record-id> | --session <id> | --folder <path> | --scope <json> | --profile"
---

<!-- BEGIN CAIRN PACK -->
Run `cairn retrieve $ARGUMENTS`.

If the user passed a bare token, treat it as a record id (positional).
Otherwise pick the right target flag: `--session`, `--folder`, `--scope`,
or `--profile`. Optional refinements: `--turn`, `--tool-call`, `--limit`,
`--rehydrate`, `--user`, `--agent`.

Show the record body. For non-record targets, show the structured envelope.
<!-- END CAIRN PACK -->
