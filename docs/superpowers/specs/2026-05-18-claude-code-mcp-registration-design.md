# Claude Code MCP Registration Design

**Issue:** [#101](https://github.com/windoliver/cairn/issues/101) — `[P0] Implement Claude Code MCP registration and local config writer`
**Parent:** [#19](https://github.com/windoliver/cairn/issues/19) — v0.1 reference consumer
**Design source:** §3.3 Active vault selection · §8 Contract surfaces · §19 Reference consumer
**External reference:** [Claude Code MCP docs](https://code.claude.com/docs/en/mcp)
**Date:** 2026-05-18
**Status:** Draft for review

---

## 1. Goal

Add a first-party setup command that registers Cairn as a Claude Code stdio
MCP server for one active vault. The command writes Claude Code config directly,
is idempotent, and provides a reversible remove path. It must not store secrets
or mutate unrelated Claude Code settings.

This closes the issue #101 acceptance path:

- Claude Code can discover and start `cairn mcp` after setup.
- Re-running setup does not duplicate MCP server entries.
- Safe uninstall/remove guidance and command behavior are present.
- Tests exercise config writer fixtures, idempotency, and startup smoke where
  local constraints allow.

## 2. Scope

### 2.1 In Scope

- New command family: `cairn setup claude-code`.
- Default Claude Code MCP scope: `local`.
- Explicit project scope support: `cairn setup claude-code --scope project`.
- A remove path: `cairn setup claude-code remove`.
- JSON and human receipts.
- Fixture-driven config writer tests.
- Doctor integration guidance: after setup, `cairn doctor claude-code` verifies
  discovery and startup.

### 2.2 Out of Scope

- Codex, Gemini, Cursor, and other harness registration.
- Cloud, remote, SSE, or HTTP MCP transports.
- Secrets, API keys, OAuth material, or any provider credentials.
- Automatically editing `CLAUDE.md` or installing the Cairn skill. That remains
  the separate `cairn skill install` surface.
- Hook configuration beyond preserving existing Claude hook settings. The
  doctor command already verifies hook presence; this issue registers MCP.

## 3. User-Facing Command

Recommended shape:

```text
cairn setup claude-code [OPTIONS]
cairn setup claude-code remove [OPTIONS]
```

Setup options:

| Option | Default | Meaning |
|---|---|---|
| `--scope <local|project>` | `local` | Claude Code config location. |
| `--project-dir <PATH>` | current directory | Project used for local-scope `~/.claude.json` project key or project-scope `.mcp.json`. |
| `--home-dir <PATH>` | `$HOME` | Testable override for `~/.claude.json`. |
| `--server-name <NAME>` | `cairn` | MCP server key in Claude config. |
| `--vault <NAME_OR_PATH>` | top-level global option | Active Cairn vault bound into `cairn mcp`. |
| `--binary <PATH>` | current executable | Absolute `cairn` binary path to register. |
| `--json` | false | Emit machine-readable receipt. |

Remove options mirror setup: `--scope`, `--project-dir`, `--home-dir`,
`--server-name`, and `--json`.

### 3.1 Default Local Scope

Defaulting to Claude Code local scope keeps registration private to the current
project and user. It writes a project-specific entry under `~/.claude.json`:

```json
{
  "projects": {
    "/abs/project": {
      "mcpServers": {
        "cairn": {
          "type": "stdio",
          "command": "/abs/path/to/cairn",
          "args": ["--vault", "/abs/path/to/vault", "mcp"],
          "env": {}
        }
      }
    }
  }
}
```

This matches the brief's one-invocation-one-vault rule (§3.3): each Claude Code
server entry is bound to exactly one active Cairn vault through `--vault`.

### 3.2 Project Scope

Project scope is explicit because `.mcp.json` is designed to be shared with a
team. It writes:

```json
{
  "mcpServers": {
    "cairn": {
      "type": "stdio",
      "command": "/abs/path/to/cairn",
      "args": ["--vault", "/abs/path/to/vault", "mcp"],
      "env": {}
    }
  }
}
```

Project scope intentionally uses concrete absolute paths by default. Operators
who want a checked-in, machine-independent `.mcp.json` can pass an explicit
`--binary` or edit the file after generation. Cairn itself does not write
environment placeholders that could hide wrong-path or secret behavior.

## 4. Architecture

### 4.1 Modules

Add:

```text
crates/cairn-cli/src/setup.rs
crates/cairn-cli/src/setup/claude_code.rs
crates/cairn-cli/tests/claude_code_setup.rs
docs/site/src/usage/claude-code.md
```

Modify:

```text
crates/cairn-cli/src/command.rs
crates/cairn-cli/src/main.rs
crates/cairn-cli/src/lib.rs
crates/cairn-cli/src/doctor.rs
docs/site/src/usage/mcp.md
```

`setup::claude_code` owns pure data transformations plus controlled filesystem
I/O for the two Claude Code config locations. `main.rs` only parses CLI options
and maps errors to exit codes, following existing CLI patterns.

### 4.2 Data Types

```rust
pub enum ClaudeCodeScope {
    Local,
    Project,
}

pub struct ClaudeCodeSetupOpts {
    pub scope: ClaudeCodeScope,
    pub project_dir: PathBuf,
    pub home_dir: PathBuf,
    pub server_name: String,
    pub vault: PathBuf,
    pub binary: PathBuf,
}

pub struct ClaudeCodeRemoveOpts {
    pub scope: ClaudeCodeScope,
    pub project_dir: PathBuf,
    pub home_dir: PathBuf,
    pub server_name: String,
}

pub struct ClaudeCodeSetupReceipt {
    pub scope: ClaudeCodeScope,
    pub config_path: PathBuf,
    pub server_name: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub status: SetupStatus,
}

pub enum SetupStatus {
    Created,
    Updated,
    Unchanged,
    Removed,
    NotFound,
}
```

### 4.3 JSON Handling

Use `serde_json::Value` and object-level merges rather than string editing.
This preserves unrelated Claude Code settings and lets tests assert exact
fixture transforms.

Setup behavior:

1. Resolve `project_dir`, `home_dir`, `binary`, and `vault` to absolute paths.
2. Read existing JSON config if present.
3. Create missing parent objects.
4. Insert or replace only `mcpServers[server_name]`.
5. Preserve all unrelated keys.
6. Write pretty JSON with a trailing newline.

Remove behavior:

1. Read existing JSON config if present.
2. Remove only `mcpServers[server_name]` at the selected scope.
3. Preserve unrelated servers and settings.
4. Leave empty parent objects alone unless removal can be done safely without
   deleting user-owned structure.
5. Return `NotFound` when no matching entry exists.

## 5. Vault and Binary Resolution

The setup command must register a `cairn mcp` invocation that starts the same
runtime a user would start manually:

```text
/abs/path/to/cairn --vault /abs/path/to/vault mcp
```

Vault resolution follows existing CLI precedence:

1. `--vault` global flag.
2. `CAIRN_VAULT`.
3. Walk-up from `--project-dir`.
4. Registry default.

If the result is a bound vault, setup writes the resolved path. If no vault can
be resolved, setup fails with `EX_CONFIG` and tells the operator to run
`cairn bootstrap` or pass `--vault`.

Binary resolution defaults to `std::env::current_exe()`. A `--binary` override
exists for packaging tests and operators who want to register a stable symlink
such as `/opt/homebrew/bin/cairn`.

## 6. Idempotency and Safety

Idempotency is structural:

- If the selected config already contains an identical server entry, return
  `Unchanged` and leave the file byte-stable where possible.
- If the selected server name exists but differs, replace only that server
  entry and return `Updated`.
- If no entry exists, insert it and return `Created`.
- Re-running setup never appends duplicate entries.

Safety rules:

- Do not write secret-bearing env keys.
- Default `env` is `{}`.
- Reject empty `server_name`.
- Reject non-object JSON roots with a clear config error.
- Never mutate a different scope than the one requested.
- Removal only touches the selected server name.

## 7. Error Handling

| Condition | Exit Code | Behavior |
|---|---:|---|
| Invalid CLI args | 64 | Clap usage error. |
| Malformed existing Claude config | 78 | Explain path and parse failure. |
| No active vault can be resolved | 78 | Ask operator to pass `--vault` or bootstrap/register a vault. |
| Config write failure | 74 | Preserve source error and path. |
| MCP startup failure in doctor smoke | 69 | Report through `cairn doctor claude-code`. |

Setup itself should not spawn Claude Code. It writes deterministic config.
Startup verification belongs to `cairn doctor claude-code`, which already
checks registration discovery and MCP startup.

## 8. Documentation

Add a Claude Code usage page that documents:

```bash
cairn setup claude-code --vault work
cairn doctor claude-code
cairn setup claude-code remove
```

The docs should state:

- Local scope is the default and private to the user/current project.
- Project scope writes `.mcp.json` and may be committed intentionally.
- Cairn does not write secrets to Claude Code config.
- To remove the registration, use the remove command or delete only the
  `mcpServers.cairn` entry from the selected scope.

`docs/site/src/usage/mcp.md` should link to the setup page as the recommended
Claude Code path while keeping the lower-level `cairn mcp` explanation.

## 9. Testing

### 9.1 Unit and Fixture Tests

Use temp directories and checked-in fixture JSON values:

- `setup_local_creates_project_entry_in_claude_json`
- `setup_local_is_idempotent`
- `setup_local_replaces_stale_cairn_entry_only`
- `setup_project_creates_mcp_json`
- `setup_project_preserves_unrelated_servers`
- `remove_local_deletes_only_selected_server`
- `remove_project_returns_not_found_when_absent`
- `setup_rejects_non_object_config_root`
- `setup_receipt_serializes_to_json`

### 9.2 CLI Tests

Run the built `cairn` binary with `--home-dir`, `--project-dir`, `--binary`,
and `--vault` pointing at temp fixtures:

- `cairn setup claude-code --json` writes the expected local-scope receipt.
- Re-running setup returns `Unchanged`.
- `cairn setup claude-code --scope project` writes `.mcp.json`.
- `cairn setup claude-code remove --json` removes the entry.
- `cairn doctor claude-code --project-dir <dir> --home-dir <dir>` succeeds
  after setup when the registered binary is the test binary and the vault
  fixture is bound.

### 9.3 Verification Commands

Targeted:

```bash
cargo nextest run -p cairn-cli --test claude_code_setup
cargo nextest run -p cairn-cli --test doctor_cli
cargo nextest run -p cairn-cli --test mcp_subcommand
```

Pre-PR:

```bash
cargo nextest run --workspace
cargo test --doc --workspace
scripts/check-core-boundary.sh
```

## 10. Migration and Compatibility

Existing users who manually registered Cairn under server name `cairn` get a
safe update: the selected entry is replaced with the normalized `cairn --vault
... mcp` command. Users who registered a different server name are untouched.

The command does not require the `claude` CLI. This keeps install verification
deterministic in CI and works on machines where Claude Code config files exist
but the executable is not on `PATH`.
