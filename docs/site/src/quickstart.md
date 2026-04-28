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
