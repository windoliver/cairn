# Configuration

Cairn reads `.cairn/config.yaml` from the vault root. Generate the default file
with:

```bash
cairn bootstrap --vault-path .
```

The loader supports environment interpolation for `${VAR}` placeholders and
`CAIRN_` environment overrides. Invalid config fails closed instead of falling
back to unknown defaults.

The generated [config defaults](../reference/generated/config-defaults.md)
are emitted from `CairnConfig::default()`. A new config field or changed default
changes that file and fails `cairn-docgen --check` until the generated docs are
updated.
