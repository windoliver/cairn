# Vault Layout

`cairn bootstrap` writes the default config file to:

```text
.cairn/config.yaml
```

The config defaults describe a local vault using the bundled SQLite store
contract, local sensor ingress, local workflow orchestration, and no configured
LLM provider. The generated [config defaults](../reference/generated/config-defaults.md)
show the exact YAML shape emitted from `CairnConfig::default()`.

Durable record storage is not wired in the current P0 build. The layout page
therefore documents the config location and contract intent rather than a stable
on-disk record schema.
