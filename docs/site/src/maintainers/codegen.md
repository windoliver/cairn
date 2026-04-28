# Codegen

Cairn has two generated-output gates.

IDL/code generation:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Docs generation:

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
```

To refresh generated docs after changing a user-facing surface:

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --write
```

Commit generated Markdown under `docs/site/src/reference/generated/`.
