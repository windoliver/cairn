# IDL

The canonical contract lives under `crates/cairn-idl/schema/`.

The IDL drives:

- Rust SDK and wire types in `crates/cairn-core/src/generated/`
- CLI clap builders in `crates/cairn-cli/src/generated/`
- MCP tool declarations and JSON schemas in `crates/cairn-mcp/src/generated/`
- The Cairn skill bundle in `skills/cairn/`
- Generated docs under `docs/site/src/reference/generated/`

Run the IDL drift gate:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Run the docs drift gate:

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
```
