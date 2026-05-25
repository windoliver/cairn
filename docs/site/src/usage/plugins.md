# Plugins

Cairn currently ships eight bundled plugin crates:

- `cairn-agent-core`
- `cairn-mcp`
- `cairn-sensors-local`
- `cairn-store-sqlite`
- `cairn-workflows`
- `cairn-frontend-obsidian`
- `cairn-frontend-vscode`
- `cairn-frontend-logseq`

List them:

```bash
cairn plugins list
cairn plugins list --json
```

Verify contract conformance:

```bash
cairn plugins verify
cairn plugins verify --strict
cairn plugins verify --json
```

Default verify mode exits 0 when tier-1 cases pass and tier-2 P0 cases are
pending. Strict mode treats pending tier-2 cases as failures and exits 69.

The generated [plugin reference](../reference/generated/plugins.md) is emitted
from the bundled registry, so adding or changing a bundled plugin updates docs
or fails CI.

See the [capability matrix](../reference/capability-matrix.md) for what ships in each release.
