# cairn-idl

Canonical IDL for the `cairn.mcp.v1` contract plus the `cairn-codegen`
binary that lowers the IDL into the four surface bundles (SDK, CLI, MCP,
skill). Schema sources live under `schema/`; generated outputs live in the
consumer crates and `skills/cairn/`.

When the schema changes, run `cargo run -p cairn-idl --bin cairn-codegen`
and commit the regenerated tree. CI (`codegen-drift`) gates on no-diff.

See `docs/dev/codegen.md` for the full maintainer guide.
