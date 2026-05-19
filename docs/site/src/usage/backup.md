# Backup Registry

Cairn tracks backups it created or that an operator explicitly registers under
`.cairn/backups/*.json`. Each entry records the artifact path, creation time,
artifact digest, backup kind, and target ids included in that artifact.

Registered backups participate in forget replay. After `cairn forget --record`
or `cairn forget --session` commits its live-store purge, Cairn scans the
registry and rewrites every registered backup that still contains a forgotten
target. The old artifact is copied under `.cairn/backups/shredded/`, and
`.cairn/backups/shredded.log` records the forget operation that invalidated it.

```bash
cairn admin snapshot --backup /path/to/backup
cairn backup register /path/to/imported-backup --kind export
cairn backup list
cairn backup forget sha256:...
```

If multiple registered artifacts have the same digest, `backup forget` removes
all matching registry entries. This avoids leaving an indistinguishable
same-content artifact registered after the operator asked to forget that digest.

The guarantee boundary is intentionally narrow: Cairn can replay tombstones into
its own registered backup artifacts and into restore operations that use
`cairn admin restore`. It cannot redact or discover third-party backups that
were copied outside this registry. Register imported backups before relying on
forget-me guarantees across restore.
