# Configuration

Cairn reads `.cairn/config.yaml` from the vault root. Generate the default file
with:

```bash
cairn bootstrap --vault-path .
```

The loader supports environment interpolation for `${VAR}` placeholders and
`CAIRN_` environment overrides. Invalid config fails closed instead of falling
back to unknown defaults.

Commands that operate on a vault resolve it in this order: the global
`--vault <name|path>` flag or `CAIRN_VAULT`, then walking up from the current
directory to find `.cairn/`, then the default entry selected by
`cairn vault switch <name>`. The registry is stored at
`$XDG_CONFIG_HOME/cairn/vaults.toml` or `$HOME/.config/cairn/vaults.toml` on
Linux/macOS, and `%APPDATA%\cairn\vaults.toml` on Windows.

The generated [config defaults](../reference/generated/config-defaults.md)
are emitted from `CairnConfig::default()`. A new config field or changed default
changes that file and fails `cairn-docgen --check` until the generated docs are
updated.
