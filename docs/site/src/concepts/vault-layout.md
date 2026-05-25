# Vault Layout

`cairn bootstrap` writes the default config file to:

```text
.cairn/config.yaml
```

The config defaults describe a local vault using the bundled SQLite store
contract, local sensor ingress, local workflow orchestration, and no configured
LLM provider. The generated [config defaults](../reference/generated/config-defaults.md)
show the exact YAML shape emitted from `CairnConfig::default()`, including
store layout, sensor policy, and pipeline options.

See the [capability matrix](../reference/capability-matrix.md) for which capabilities ship in which release.
