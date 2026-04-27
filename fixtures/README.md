# Cairn test fixtures

Golden records, config, envelopes, search filters, and plugin manifests for CI schema validation.

## Layout

```
fixtures/
└── v0/                    ← canonical P0 schema snapshot (never mutated)
    ├── records/           ← one JSON per memory-class × visibility-tier combination
    ├── config/            ← CairnConfig JSON examples
    ├── envelopes/         ← SignedIntent + Response wire fixtures
    ├── search-filters/    ← SearchArgsFilters + SearchArgs examples
    └── manifests/         ← plugin.toml manifests for each ContractKind
```

Consumed through the `cairn-test-fixtures` crate's `fixtures_dir()` and `fixture_v0_dir()` helpers.

## Loading in tests

```rust
let path = cairn_test_fixtures::fixture_v0_dir().join("records/semantic_private_user.json");
let raw = std::fs::read_to_string(&path).expect("fixture must exist");
let record: MemoryRecord = serde_json::from_str(&raw).expect("must parse");
record.validate().expect("must be valid");
```

## Migration workflow

When a schema change is intentional:

1. Copy `v0/` → `v1/`, update the changed fixtures in `v1/`.
2. Keep `v0/` intact — it proves old vaults still load.
3. Add a migration test in `schema_fixtures.rs` that loads `v0/` fixtures with the new schema.
4. Run `cargo insta review` to accept new snapshots.
5. Include the snapshot files in the same commit as the schema change.

## CI

`cargo nextest run --workspace` runs the `schema_fixtures` test binary automatically. The `insta` snapshots act as a schema-drift gate: if a schema change breaks deserialization or shifts the wire form, the test fails and the snapshot diff is shown.
