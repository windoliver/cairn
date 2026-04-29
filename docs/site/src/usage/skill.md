# Cairn Skill

The `skills/cairn` directory is generated from the same IDL as the CLI and MCP
surfaces. It gives shell-oriented agents a stable way to learn the Cairn
contract, command names, and usage conventions.

Regenerate it with the normal IDL codegen gate:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Install the committed skill bundle with:

```bash
cairn skill install --harness codex
```

Supported harness values are `claude-code`, `codex`, `gemini`, `opencode`,
`cursor`, and `custom`. The default install directory is
`~/.cairn/skills/cairn/`; use `--target-dir <path>` to write elsewhere and
`--force` to overwrite generated files.

When the IDL changes, `cairn-codegen --check` catches generated skill drift and
`cairn-docgen --check` catches the corresponding docs drift.
