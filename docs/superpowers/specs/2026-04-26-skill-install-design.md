# Design: `cairn skill install` — directory writer and registration hints

**Issue:** [#68](https://github.com/windoliver/cairn/issues/68)  
**Design source:** §8.0.a-bis, §18.d  
**Date:** 2026-04-26  
**Status:** approved

---

## Summary

`cairn skill install --harness <harness>` writes the Cairn skill bundle into
`~/.cairn/skills/cairn/` and prints a harness-specific registration hint.
Install is idempotent and version-aware. No harness config is mutated.

---

## Decisions

| # | Question | Decision | Rationale |
|---|----------|----------|-----------|
| 1 | How are skill files bundled in the binary? | `include_str!` at compile time | Single-binary, offline, no runtime path resolution. Mirrors the bootstrap pattern. CI `--check` gate catches drift. |
| 2 | Does install mutate harness configs? | No — print-only hints | Issue explicitly excludes "Full Claude Code hook integration". Print-only is zero-blast-radius. |
| 3 | Kind cheat-sheet source | Hardcoded 8 kinds from §18.d | Taxonomy IDL slice not yet landed. Kinds are stable in the brief. `TODO(taxonomy-idl)` comment marks the future hook-up point. |

---

## Architecture

### Components changed

**`crates/cairn-idl/src/codegen/emit_skill.rs`**

Extend `emit_conventions()` to render the kind cheat-sheet. The 8 kinds
(`user`, `feedback`, `rule`, `fact`, `entity`, `playbook`, `strategy_success`,
`trace`) are hardcoded strings with a `TODO(taxonomy-idl)` comment. No other
changes to the IDL crate.

**`crates/cairn-cli/src/skill.rs`** (new file)

Owns all install logic:

```rust
pub enum Harness {
    ClaudeCode,
    Codex,
    Gemini,
    Opencode,
    Cursor,
    Custom(PathBuf),
}

pub struct InstallOpts {
    pub target_dir: PathBuf,   // default: ~/.cairn/skills/cairn/
    pub harness: Harness,
    pub force: bool,
}

pub struct InstallReceipt {
    pub target_dir: PathBuf,
    pub contract_version: String,
    pub idl_version: String,
    pub files_created: Vec<PathBuf>,
    pub files_skipped: Vec<PathBuf>,
    pub registration_hint: String,
}

pub fn install(opts: &InstallOpts) -> Result<InstallReceipt>;
pub fn registration_hint(harness: &Harness) -> &'static str;

// Embedded at compile time from the committed generated artifacts:
const SKILL_MD: &str = include_str!("../../../skills/cairn/SKILL.md");
const CONVENTIONS_MD: &str = include_str!("../../../skills/cairn/conventions.md");
const VERSION_FILE: &str = include_str!("../../../skills/cairn/.version");
```

**`crates/cairn-cli/src/main.rs`**

Add `skill_subcommand()` returning a `clap::Command` with one sub-subcommand
`install`. Args: `--harness <harness>` (required, `ValueEnum`), `--target-dir
<path>` (optional, default `~/.cairn/skills/cairn/`), `--force` (flag),
`--json` (flag). Dispatch in `run_skill()`.

---

## Data flow

```
cairn skill install --harness claude-code
  │
  ├─ resolve target_dir  (~/.cairn/skills/cairn/ or --target-dir)
  │
  ├─ version check
  │    read target_dir/.version (if exists)
  │    same version + !force  →  skip all writes, return receipt (files_skipped)
  │    newer version           →  warn to stderr, proceed (downgrade is user's call)
  │    older / absent          →  proceed (upgrade or fresh install)
  │
  ├─ write generated files  (atomic, symlink-safe — same write_once as vault.rs)
  │    SKILL.md        →  always overwrite on version change or fresh install
  │    conventions.md  →  always overwrite
  │    .version        →  always overwrite
  │
  ├─ write example stubs  (write_once, force=false always — user may have edited)
  │    examples/01-remember-preference.md
  │    examples/02-forget-something.md
  │    examples/03-search-prior-decision.md
  │    examples/04-skillify-this.md
  │
  ├─ print registration_hint(harness) to stdout (or embed in JSON receipt)
  │
  └─ exit 0
```

### User-local note preservation

`examples/` files are written once and never overwritten on reinstall (even
with a version upgrade). `SKILL.md`, `conventions.md`, and `.version` are
always regenerated — they are IDL artifacts, not user content.

### JSON receipt

```json
{
  "target_dir": "/home/user/.cairn/skills/cairn",
  "contract_version": "cairn.mcp.v1",
  "idl_version": "0.0.1",
  "files_created": ["SKILL.md", "conventions.md", ".version"],
  "files_skipped": [],
  "registration_hint": "# Add to your CLAUDE.md:\n@~/.cairn/skills/cairn/SKILL.md"
}
```

---

## Registration hints (per harness)

All hints are printed to stdout only — no harness config is mutated.

| Harness | Hint |
|---------|------|
| `claude-code` | `# Add to your CLAUDE.md:\n@~/.cairn/skills/cairn/SKILL.md` |
| `codex` | `# Add to your AGENTS.md:\n@~/.cairn/skills/cairn/SKILL.md` |
| `gemini` | `# Add to your GEMINI.md:\n@~/.cairn/skills/cairn/SKILL.md` |
| `opencode` | `# Add to your opencode config skills path:\n~/.cairn/skills/cairn/SKILL.md` |
| `cursor` | `# Add to your .cursorrules:\n@~/.cairn/skills/cairn/SKILL.md` |
| `custom` | `# Skill bundle written to <path>. Register it with your harness manually.` |

---

## Error handling

Library code (`skill.rs`) uses `thiserror`. Binary (`main.rs`) wraps in `anyhow`.

| Condition | Exit code |
|-----------|-----------|
| I/O error (can't create dir / write file) | `74` (EX_IOERR) |
| Target dir is a symlink | `73` (EX_CANTCREAT) |
| Downgrade detected | `0` — warn to stderr, proceed |

---

## Testing

| Layer | What | Mechanism |
|-------|------|-----------|
| Unit | `registration_hint()` correct per harness | `#[test]` in `skill.rs` |
| Unit | Idempotency — second install, all files in `files_skipped` | `tempfile::tempdir()` |
| Unit | Version-same skip — `.version` matches, `!force` → no writes | `tempfile::tempdir()` |
| Unit | Version-upgrade — older `.version` → generated files overwritten, examples skipped | `tempfile::tempdir()` |
| Unit | Symlink rejection — symlinked target dir → `Err` | `tempfile::tempdir()` + `std::os::unix::fs::symlink` |
| Snapshot | `render_human(receipt)` | `insta` |
| Snapshot | JSON receipt | `insta` |

All tests use real `tempdir()` — no filesystem mocking.

**CI gate additions:**
- `include_str!` paths resolve at compile time → build fails if `skills/cairn/SKILL.md` missing
- `cargo nextest run --workspace` covers new tests
- `cargo run -p cairn-idl --bin cairn-codegen -- --check` already gates skill file drift

---

## Out of scope (v0.1)

- Full Claude Code hook integration (explicit issue exclusion)
- Mutating any harness config file
- Harness auto-detection (user must pass `--harness`)
- Taxonomy IDL-driven kind cheat-sheet (blocked on separate PR; hardcoded for now)
