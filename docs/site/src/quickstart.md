# Quickstart

Build the CLI from the workspace root:

```bash
cargo build -p cairn-cli --locked
```

Run the implemented prelude commands:

```bash
cargo run -p cairn-cli --locked -- status --json
cargo run -p cairn-cli --locked -- handshake --json
```

Create a default vault config:

```bash
cargo run -p cairn-cli --locked -- bootstrap --vault-path .
```

Register and select the vault:

```bash
cargo run -p cairn-cli --locked -- vault add . --name default
cargo run -p cairn-cli --locked -- vault switch default
```

Inspect bundled plugins:

```bash
cargo run -p cairn-cli --locked -- plugins list
cargo run -p cairn-cli --locked -- plugins verify
```

`plugins verify` exits 0 in default mode when tier-2 P0 cases are pending. Add
`--strict` when you want pending tier-2 cases to fail with exit code 69.

The memory verbs are present for interface stability, but they are not storage
backed yet:

```bash
cargo run -p cairn-cli --locked -- search --json
```

Today those verbs return an `Internal`/aborted response rather than silently
pretending memory work succeeded.

Install the agent skill bundle when you want a shell-oriented harness to learn
the Cairn contract and conventions:

```bash
cargo run -p cairn-cli --locked -- skill install --harness codex
```

For the v0.1 reference consumer, verify the Claude Code wiring end to end:

```bash
cargo run -p cairn-cli --locked -- doctor claude-code --json
```

Successful doctor output is the reproducibility checklist: it proves the Cairn
binary is discoverable, Claude Code can find the MCP registration, the
configured server starts, `status` is callable through the MCP surface, and the
five expected hook entries are present.

Use the [Claude Code reference consumer guide](usage/claude-code-reference.md)
for the full hook-loop smoke test, P0 acceptance checklist, and daily dogfood
workflow.

For what each release supports, see the [capability matrix](reference/capability-matrix.md).
