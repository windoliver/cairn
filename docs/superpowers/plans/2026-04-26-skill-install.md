# `cairn skill install` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `cairn skill install --harness <harness>` — writes the Cairn skill bundle to `~/.cairn/skills/cairn/`, is idempotent and version-aware, and prints harness-specific registration hints.

**Architecture:** Extend `emit_skill.rs` with the kind cheat-sheet; create `skill.rs` in `cairn-cli` mirroring the `vault.rs` bootstrap pattern (opts → pure writer → receipt); wire the `skill install` subcommand in `main.rs`. Skill files are embedded via `include_str!` at compile time — no runtime path resolution.

**Tech Stack:** Rust 1.95, `clap 4.5` (derive + `ValueEnum`), `anyhow`, `serde_json`, `insta` (snapshots), `tempfile` (tests).

---

## File map

| Action | File | Responsibility |
|--------|------|---------------|
| Modify | `crates/cairn-idl/src/codegen/emit_skill.rs` | Add kind cheat-sheet to `emit_conventions()` |
| Modify | `skills/cairn/conventions.md` | Regenerated artifact — commit after codegen run |
| Modify | `crates/cairn-cli/src/vault.rs` | Make `write_once` `pub(crate)`; generalize signature |
| Create | `crates/cairn-cli/src/skill.rs` | `Harness`, `InstallOpts`, `InstallReceipt`, `install()`, `registration_hint()`, `render_human()`, embedded constants |
| Create | `skills/cairn/examples/01-remember-preference.md` | Static example stub |
| Create | `skills/cairn/examples/02-forget-something.md` | Static example stub |
| Create | `skills/cairn/examples/03-search-prior-decision.md` | Static example stub |
| Create | `skills/cairn/examples/04-skillify-this.md` | Static example stub |
| Modify | `crates/cairn-cli/src/lib.rs` | Expose `pub mod skill` |
| Modify | `crates/cairn-cli/src/main.rs` | Add `skill_subcommand()`, `run_skill()`, dispatch |

---

## Task 1: Extend `emit_conventions()` with kind cheat-sheet

**Files:**
- Modify: `crates/cairn-idl/src/codegen/emit_skill.rs` (lines 77-90)

- [ ] **Step 1: Open `emit_skill.rs` and replace the `emit_conventions` function**

Replace the entire `emit_conventions` function (currently lines 77–90) with:

```rust
fn emit_conventions(doc: &Document) -> GeneratedFile {
    let mut s = String::new();
    s.push_str(HEADER_MD);
    s.push_str("# Cairn skill conventions\n\n");
    s.push_str("Verb ids in the contract:\n\n");
    for verb in &doc.verbs {
        let _ = writeln!(s, "- `{}`", verb.id);
    }
    s.push_str("\n## Kind cheat-sheet (pick one — never invent new kinds)\n\n");
    // TODO(taxonomy-idl): generate from IDL when taxonomy slice lands
    let kinds: &[(&str, &str)] = &[
        ("user",             "preferences, working style, identity"),
        ("feedback",         "corrections the user gave you"),
        ("rule",             r#"invariants ("never X", "always Y")"#),
        ("fact",             "verifiable claims about the world"),
        ("entity",           "people, projects, systems you encountered"),
        ("playbook",         "reusable procedures with decision trees"),
        ("strategy_success", "an ad-hoc procedure that worked"),
        ("trace",            "reasoning trajectories (auto-captured; don't call directly)"),
    ];
    for (kind, desc) in kinds {
        let _ = writeln!(s, "- `{kind}` — {desc}");
    }
    s.push('\n');
    GeneratedFile {
        path: PathBuf::from("skills/cairn/conventions.md"),
        bytes: s.into_bytes(),
    }
}
```

- [ ] **Step 2: Regenerate the committed artifact**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked
```

Expected: exits 0, `skills/cairn/conventions.md` is updated.

- [ ] **Step 3: Verify idempotency (--check gate)**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: exits 0 (no diff on second run).

- [ ] **Step 4: Verify it compiles**

```bash
cargo check --workspace --all-targets --locked
```

Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-idl/src/codegen/emit_skill.rs skills/cairn/conventions.md
git commit -m "feat(idl): add kind cheat-sheet to conventions.md (§18.d, #68)"
```

---

## Task 2: Generalize `write_once` in `vault.rs` for reuse

**Files:**
- Modify: `crates/cairn-cli/src/vault.rs` (lines 178–285)

**Why:** `skill.rs` needs the same atomic + symlink-safe write logic. Making it `pub(crate)` with a generic signature avoids duplicating security-sensitive code.

- [ ] **Step 1: Change `write_once` signature in `vault.rs`**

Replace the existing `fn write_once` signature and its receipt-push calls. The new signature takes `created` and `skipped` vecs directly instead of `&mut BootstrapReceipt`:

```rust
pub(crate) fn write_once(
    path: &std::path::Path,
    content: &str,
    force: bool,
    created: &mut Vec<PathBuf>,
    skipped: &mut Vec<PathBuf>,
) -> Result<()> {
```

The body of the function is unchanged except the two lines that push to `receipt`:

Replace:
```rust
receipt.files_skipped.push(path.to_owned());
```
with:
```rust
skipped.push(path.to_owned());
```

Replace (near the end):
```rust
receipt.files_created.push(path.to_owned());
```
with:
```rust
created.push(path.to_owned());
```

There are **three** `skipped.push` / `created.push` sites in the function — two for the `AlreadyExists` race path (both push to `skipped`) and one at the end (pushes to `created`). Update all of them.

- [ ] **Step 2: Update the four `write_once` call sites in `bootstrap()`**

Replace:
```rust
write_once(&config_path, &config_yaml, opts.force, &mut receipt)?;
write_once(
    &vault.join("purpose.md"),
    PURPOSE_MD,
    opts.force,
    &mut receipt,
)?;
write_once(&vault.join("index.md"), "", opts.force, &mut receipt)?;
write_once(&vault.join("log.md"), "", opts.force, &mut receipt)?;
```

With:
```rust
write_once(
    &config_path,
    &config_yaml,
    opts.force,
    &mut receipt.files_created,
    &mut receipt.files_skipped,
)?;
write_once(
    &vault.join("purpose.md"),
    PURPOSE_MD,
    opts.force,
    &mut receipt.files_created,
    &mut receipt.files_skipped,
)?;
write_once(
    &vault.join("index.md"),
    "",
    opts.force,
    &mut receipt.files_created,
    &mut receipt.files_skipped,
)?;
write_once(
    &vault.join("log.md"),
    "",
    opts.force,
    &mut receipt.files_created,
    &mut receipt.files_skipped,
)?;
```

- [ ] **Step 3: Run existing vault tests to confirm nothing broke**

```bash
cargo nextest run -p cairn-cli --locked
```

Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/src/vault.rs
git commit -m "refactor(cli): make write_once pub(crate) for reuse in skill install (#68)"
```

---

## Task 3: Create example stubs in `skills/cairn/examples/`

**Files:**
- Create: `skills/cairn/examples/01-remember-preference.md`
- Create: `skills/cairn/examples/02-forget-something.md`
- Create: `skills/cairn/examples/03-search-prior-decision.md`
- Create: `skills/cairn/examples/04-skillify-this.md`

These are static content (not IDL-generated). Users may edit them post-install — that's by design.

- [ ] **Step 1: Create `skills/cairn/examples/01-remember-preference.md`**

```markdown
# Example: remember a preference

**User says:** "Remember that I prefer snake_case for all variable names."

**Cairn call:**
```bash
cairn ingest --kind user --body "prefers snake_case for variable names"
```

**Why `kind: user`:** A preference about working style that should persist across sessions.
```

- [ ] **Step 2: Create `skills/cairn/examples/02-forget-something.md`**

```markdown
# Example: forget a stored fact

**User says:** "Forget what I said about preferring tabs."

**Cairn calls (two steps):**
```bash
# 1. Find the record id
cairn search "tabs preference" --limit 5 --json

# 2. Delete it (confirm with user before running forget)
cairn forget --record <id-from-search>
```

**Non-negotiable:** Always confirm with the user before running `cairn forget`.
```

- [ ] **Step 3: Create `skills/cairn/examples/03-search-prior-decision.md`**

```markdown
# Example: search for a prior decision

**User says:** "What did we decide about the database schema?"

**Cairn call:**
```bash
cairn search "database schema decision" --limit 10 --json
```

**Parse the JSON response:**
```json
{"hits":[
  {"id":"01HQZ...","kind":"fact","body":"decided to use sqlite-vec for ANN","score":0.91}
]}
```
```

- [ ] **Step 4: Create `skills/cairn/examples/04-skillify-this.md`**

```markdown
# Example: skillify a procedure

**User says:** "Skillify this — we just figured out how to run the benchmarks."

**Cairn call:**
```bash
cairn ingest \
  --kind strategy_success \
  --body "Run benchmarks: cargo criterion --bench <name>; results in target/criterion/" \
  --tag benchmark,procedure
```

**Why `kind: strategy_success`:** A procedure that worked — worth keeping for next time.
```

- [ ] **Step 5: Verify they render correctly (visual check)**

```bash
cat skills/cairn/examples/01-remember-preference.md
```

Expected: the markdown content appears.

- [ ] **Step 6: Commit**

```bash
git add skills/cairn/examples/
git commit -m "feat(skill): add example stubs for skill install (§18.d, #68)"
```

---

## Task 4: `Harness` enum + `registration_hint()` — TDD

**Files:**
- Create: `crates/cairn-cli/src/skill.rs`
- Modify: `crates/cairn-cli/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-cli/src/skill.rs` with just enough to make the test compile:

```rust
//! `cairn skill install` — writes the Cairn skill bundle to the harness skill
//! directory (§8.0.a-bis, §18.d).

use std::path::PathBuf;

use clap::ValueEnum;

/// Supported harnesses for `cairn skill install`.
#[derive(Debug, Clone, PartialEq, Eq, ValueEnum)]
pub enum Harness {
    #[value(name = "claude-code")]
    ClaudeCode,
    Codex,
    Gemini,
    Opencode,
    Cursor,
    Custom,
}

/// Returns the harness-specific registration hint printed after install.
pub fn registration_hint(harness: &Harness) -> &'static str {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_hint_covers_all_harnesses() {
        let cases = [
            (Harness::ClaudeCode, "CLAUDE.md"),
            (Harness::Codex, "AGENTS.md"),
            (Harness::Gemini, "GEMINI.md"),
            (Harness::Opencode, "opencode"),
            (Harness::Cursor, ".cursorrules"),
            (Harness::Custom, "manually"),
        ];
        for (harness, expected_fragment) in &cases {
            let hint = registration_hint(harness);
            assert!(
                hint.contains(expected_fragment),
                "hint for {harness:?} should mention '{expected_fragment}' — got: {hint:?}"
            );
        }
    }
}
```

- [ ] **Step 2: Expose the module from `lib.rs`**

Add to `crates/cairn-cli/src/lib.rs`:

```rust
pub mod skill;
```

(After the existing `pub mod vault;` line.)

- [ ] **Step 3: Run the test to verify it fails**

```bash
cargo nextest run -p cairn-cli registration_hint_covers_all_harnesses --locked
```

Expected: FAIL — panics on `todo!()`.

- [ ] **Step 4: Implement `registration_hint()`**

Replace the `todo!()` with:

```rust
pub fn registration_hint(harness: &Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => {
            "# Add to your CLAUDE.md:\n@~/.cairn/skills/cairn/SKILL.md"
        }
        Harness::Codex => {
            "# Add to your AGENTS.md:\n@~/.cairn/skills/cairn/SKILL.md"
        }
        Harness::Gemini => {
            "# Add to your GEMINI.md:\n@~/.cairn/skills/cairn/SKILL.md"
        }
        Harness::Opencode => {
            "# Add to your opencode config skills path:\n~/.cairn/skills/cairn/SKILL.md"
        }
        Harness::Cursor => {
            "# Add to your .cursorrules:\n@~/.cairn/skills/cairn/SKILL.md"
        }
        Harness::Custom => {
            "# Skill bundle written. Register it with your harness manually."
        }
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

```bash
cargo nextest run -p cairn-cli registration_hint_covers_all_harnesses --locked
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/skill.rs crates/cairn-cli/src/lib.rs
git commit -m "feat(cli/skill): Harness enum + registration_hint() (§18.d, #68)"
```

---

## Task 5: `InstallOpts`, `InstallReceipt`, embedded constants

**Files:**
- Modify: `crates/cairn-cli/src/skill.rs`

- [ ] **Step 1: Add types and embedded constants to `skill.rs`**

Below the `Harness` enum and `registration_hint()`, add:

```rust
// Embedded at compile time from the committed generated artifacts.
// The CI `--check` gate catches drift between these and what cairn-codegen emits.
const SKILL_MD: &str = include_str!("../../../skills/cairn/SKILL.md");
const CONVENTIONS_MD: &str = include_str!("../../../skills/cairn/conventions.md");
const VERSION_FILE: &str = include_str!("../../../skills/cairn/.version");

// Static example stubs — written once on install; user may edit after.
const EXAMPLE_01: &str =
    include_str!("../../../skills/cairn/examples/01-remember-preference.md");
const EXAMPLE_02: &str =
    include_str!("../../../skills/cairn/examples/02-forget-something.md");
const EXAMPLE_03: &str =
    include_str!("../../../skills/cairn/examples/03-search-prior-decision.md");
const EXAMPLE_04: &str =
    include_str!("../../../skills/cairn/examples/04-skillify-this.md");

/// Options for [`install`].
#[derive(Debug, Clone)]
pub struct InstallOpts {
    /// Target directory. Default: `~/.cairn/skills/cairn/`.
    pub target_dir: PathBuf,
    /// Which harness to generate the registration hint for.
    pub harness: Harness,
    /// If `true`, overwrite generated files even if the version matches.
    pub force: bool,
}

/// Result of a skill install run.
#[derive(Debug, serde::Serialize)]
pub struct InstallReceipt {
    pub target_dir: PathBuf,
    pub contract_version: String,
    pub idl_version: String,
    pub files_created: Vec<PathBuf>,
    pub files_skipped: Vec<PathBuf>,
    pub registration_hint: String,
}

/// Resolves the default install directory (`~/.cairn/skills/cairn/`).
///
/// # Errors
/// Returns an error if `HOME` is not set in the environment.
pub fn default_target_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| anyhow::anyhow!("HOME environment variable is not set"))?;
    Ok(PathBuf::from(home).join(".cairn/skills/cairn"))
}
```

Also add at the top of the file (after `use clap::ValueEnum;`):

```rust
use anyhow::{Context, Result};
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo check -p cairn-cli --locked
```

Expected: no errors. The `include_str!` paths resolve at compile time — if any example file is missing, the build fails here.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-cli/src/skill.rs
git commit -m "feat(cli/skill): InstallOpts, InstallReceipt, embedded constants (§18.d, #68)"
```

---

## Task 6: `install()` — fresh install path — TDD

**Files:**
- Modify: `crates/cairn-cli/src/skill.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `skill.rs`:

```rust
#[test]
fn install_fresh_creates_expected_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("skills/cairn");
    let opts = InstallOpts {
        target_dir: target.clone(),
        harness: Harness::ClaudeCode,
        force: false,
    };

    let receipt = install(&opts).expect("fresh install");

    // Generated files must exist.
    assert!(target.join("SKILL.md").exists(), "SKILL.md missing");
    assert!(target.join("conventions.md").exists(), "conventions.md missing");
    assert!(target.join(".version").exists(), ".version missing");

    // Example stubs must exist.
    assert!(
        target.join("examples/01-remember-preference.md").exists(),
        "example 01 missing"
    );
    assert!(
        target.join("examples/04-skillify-this.md").exists(),
        "example 04 missing"
    );

    // Fresh install: nothing skipped.
    assert!(receipt.files_skipped.is_empty(), "expected no skips on fresh install");
    assert!(!receipt.files_created.is_empty(), "expected files to be created");

    // Receipt fields are populated.
    assert_eq!(receipt.contract_version, "cairn.mcp.v1");
    assert!(!receipt.idl_version.is_empty());
    assert!(!receipt.registration_hint.is_empty());
}
```

- [ ] **Step 2: Run to confirm it fails**

```bash
cargo nextest run -p cairn-cli install_fresh_creates_expected_files --locked
```

Expected: FAIL — `install` not defined.

- [ ] **Step 3: Add the `install()` stub and version helpers**

Add to `skill.rs` (above the `#[cfg(test)]` block):

```rust
/// Parses the `cairn-idl: X.Y.Z` line from a `.version` file.
fn parse_idl_version(version_file: &str) -> Option<String> {
    for line in version_file.lines() {
        if let Some(ver) = line.strip_prefix("cairn-idl: ") {
            return Some(ver.trim().to_owned());
        }
    }
    None
}

/// Compares two `X.Y.Z` version strings. Returns `Less` if `a < b`.
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    fn parse(v: &str) -> Option<(u64, u64, u64)> {
        let mut it = v.split('.');
        Some((
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
            it.next()?.parse().ok()?,
        ))
    }
    match (parse(a), parse(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        _ => std::cmp::Ordering::Equal,
    }
}

/// Installs the Cairn skill bundle at `opts.target_dir`.
///
/// Idempotent and version-aware. See the spec (§8.0.a-bis, §18.d) for the
/// full decision tree.
///
/// # Errors
/// Returns an error if a directory cannot be created or a file cannot be
/// written. Symlinked paths are rejected.
pub fn install(opts: &InstallOpts) -> Result<InstallReceipt> {
    let target = &opts.target_dir;
    let current_idl_version = env!("CARGO_PKG_VERSION");

    // Reject symlinked target root.
    if let Ok(meta) = std::fs::symlink_metadata(target) {
        if meta.file_type().is_symlink() {
            anyhow::bail!(
                "{} is a symlink — skill install will not write through it",
                target.display()
            );
        }
    }

    // Create target dir and examples/ subdir.
    std::fs::create_dir_all(target.join("examples"))
        .with_context(|| format!("creating {}", target.join("examples").display()))?;

    // Version check.
    let installed_version = std::fs::read_to_string(target.join(".version"))
        .ok()
        .and_then(|s| parse_idl_version(&s));

    let skip_generated = match &installed_version {
        Some(installed) if installed == current_idl_version && !opts.force => true,
        Some(installed)
            if compare_versions(installed, current_idl_version)
                == std::cmp::Ordering::Greater =>
        {
            eprintln!(
                "cairn skill install: warning — installed version ({installed}) is newer \
                 than this binary ({current_idl_version}); proceeding with downgrade"
            );
            false
        }
        _ => false,
    };

    let mut files_created: Vec<PathBuf> = Vec::new();
    let mut files_skipped: Vec<PathBuf> = Vec::new();

    // Generated files: overwrite unless skip_generated.
    let gen_force = opts.force || !skip_generated;
    crate::vault::write_once(
        &target.join("SKILL.md"),
        SKILL_MD,
        gen_force,
        &mut files_created,
        &mut files_skipped,
    )?;
    crate::vault::write_once(
        &target.join("conventions.md"),
        CONVENTIONS_MD,
        gen_force,
        &mut files_created,
        &mut files_skipped,
    )?;
    crate::vault::write_once(
        &target.join(".version"),
        VERSION_FILE,
        gen_force,
        &mut files_created,
        &mut files_skipped,
    )?;

    // Example stubs: write-once, never overwrite (user may have edited).
    let examples_dir = target.join("examples");
    for (name, content) in [
        ("01-remember-preference.md", EXAMPLE_01),
        ("02-forget-something.md", EXAMPLE_02),
        ("03-search-prior-decision.md", EXAMPLE_03),
        ("04-skillify-this.md", EXAMPLE_04),
    ] {
        crate::vault::write_once(
            &examples_dir.join(name),
            content,
            false, // never force-overwrite examples
            &mut files_created,
            &mut files_skipped,
        )?;
    }

    let hint = registration_hint(&opts.harness).to_owned();

    // Parse contract version from embedded .version file for the receipt.
    let contract_version = VERSION_FILE
        .lines()
        .find_map(|l| l.strip_prefix("contract: ").map(str::to_owned))
        .unwrap_or_else(|| "cairn.mcp.v1".to_owned());

    Ok(InstallReceipt {
        target_dir: target.clone(),
        contract_version,
        idl_version: current_idl_version.to_owned(),
        files_created,
        files_skipped,
        registration_hint: hint,
    })
}
```

- [ ] **Step 4: Run the test**

```bash
cargo nextest run -p cairn-cli install_fresh_creates_expected_files --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/skill.rs
git commit -m "feat(cli/skill): install() fresh-install path (§18.d, #68)"
```

---

## Task 7: `install()` — idempotency and version-same skip — TDD

**Files:**
- Modify: `crates/cairn-cli/src/skill.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module:

```rust
#[test]
fn install_idempotent_same_version_skips_generated() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("skills/cairn");
    let opts = InstallOpts {
        target_dir: target.clone(),
        harness: Harness::ClaudeCode,
        force: false,
    };

    // First install.
    install(&opts).expect("first install");

    // Second install — same version, no --force.
    let receipt2 = install(&opts).expect("second install");

    // Generated files should be in files_skipped on the second run.
    let skipped_names: Vec<_> = receipt2
        .files_skipped
        .iter()
        .filter_map(|p| p.file_name())
        .collect();
    assert!(
        skipped_names.contains(&std::ffi::OsStr::new("SKILL.md")),
        "SKILL.md should be skipped on same-version reinstall"
    );
    assert!(
        skipped_names.contains(&std::ffi::OsStr::new("conventions.md")),
        "conventions.md should be skipped"
    );
    assert!(
        skipped_names.contains(&std::ffi::OsStr::new(".version")),
        ".version should be skipped"
    );
}

#[test]
fn install_force_overwrites_generated_even_on_same_version() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("skills/cairn");

    // First install.
    install(&InstallOpts {
        target_dir: target.clone(),
        harness: Harness::ClaudeCode,
        force: false,
    })
    .expect("first install");

    // Second install with --force.
    let receipt2 = install(&InstallOpts {
        target_dir: target.clone(),
        harness: Harness::ClaudeCode,
        force: true,
    })
    .expect("second install with force");

    let created_names: Vec<_> = receipt2
        .files_created
        .iter()
        .filter_map(|p| p.file_name())
        .collect();

    assert!(
        created_names.contains(&std::ffi::OsStr::new("SKILL.md")),
        "SKILL.md should be recreated with --force"
    );
    // Examples must still be skipped even with --force.
    let skipped_names: Vec<_> = receipt2
        .files_skipped
        .iter()
        .filter_map(|p| p.file_name())
        .collect();
    assert!(
        skipped_names.contains(&std::ffi::OsStr::new("01-remember-preference.md")),
        "example stubs must not be overwritten even with --force"
    );
}
```

- [ ] **Step 2: Run to confirm they fail**

```bash
cargo nextest run -p cairn-cli "install_idempotent|install_force_overwrites" --locked
```

Expected: Both tests FAIL (the idempotency logic is already in place from Task 6, so these may actually pass — run to check; fix if they fail).

- [ ] **Step 3: Run all skill tests to confirm everything passes**

```bash
cargo nextest run -p cairn-cli --locked
```

Expected: all tests pass (the logic from Task 6 already handles these cases).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/src/skill.rs
git commit -m "test(cli/skill): idempotency and --force tests (§18.d, #68)"
```

---

## Task 8: `install()` — version upgrade and downgrade warning — TDD

**Files:**
- Modify: `crates/cairn-cli/src/skill.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module:

```rust
#[test]
fn install_upgrades_when_older_version_present() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("skills/cairn");
    std::fs::create_dir_all(target.join("examples")).expect("create dir");

    // Write a stale .version file with an older idl version.
    std::fs::write(
        target.join(".version"),
        "contract: cairn.mcp.v1\ncairn-idl: 0.0.0\n",
    )
    .expect("write stale version");

    let opts = InstallOpts {
        target_dir: target.clone(),
        harness: Harness::ClaudeCode,
        force: false,
    };
    let receipt = install(&opts).expect("upgrade install");

    // Generated files must be in created (not skipped) because version differs.
    let created_names: Vec<_> = receipt
        .files_created
        .iter()
        .filter_map(|p| p.file_name())
        .collect();
    assert!(
        created_names.contains(&std::ffi::OsStr::new("SKILL.md")),
        "SKILL.md should be updated on version upgrade"
    );
}

#[test]
fn install_downgrade_proceeds_with_warning() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("skills/cairn");
    std::fs::create_dir_all(target.join("examples")).expect("create dir");

    // Write a .version file with a higher idl version (simulated downgrade).
    std::fs::write(
        target.join(".version"),
        "contract: cairn.mcp.v1\ncairn-idl: 999.0.0\n",
    )
    .expect("write future version");

    let opts = InstallOpts {
        target_dir: target.clone(),
        harness: Harness::ClaudeCode,
        force: false,
    };

    // Downgrade should succeed (not error).
    let result = install(&opts);
    assert!(
        result.is_ok(),
        "downgrade should proceed without error, got: {result:?}"
    );

    // Generated files should be updated (overwritten).
    let receipt = result.unwrap();
    let created_names: Vec<_> = receipt
        .files_created
        .iter()
        .filter_map(|p| p.file_name())
        .collect();
    assert!(
        created_names.contains(&std::ffi::OsStr::new("SKILL.md")),
        "SKILL.md should be overwritten on downgrade"
    );
}

#[test]
fn parse_idl_version_extracts_version() {
    let version_file = "contract: cairn.mcp.v1\ncairn-idl: 1.2.3\n";
    assert_eq!(
        parse_idl_version(version_file),
        Some("1.2.3".to_owned())
    );
}

#[test]
fn compare_versions_ordering() {
    use std::cmp::Ordering;
    assert_eq!(compare_versions("0.0.1", "0.0.2"), Ordering::Less);
    assert_eq!(compare_versions("1.0.0", "0.9.9"), Ordering::Greater);
    assert_eq!(compare_versions("0.0.1", "0.0.1"), Ordering::Equal);
}
```

- [ ] **Step 2: Run to confirm they fail**

```bash
cargo nextest run -p cairn-cli "install_upgrades|install_downgrade|parse_idl_version|compare_versions" --locked
```

Expected: tests for upgrade/downgrade should pass (logic already in Task 6). `parse_idl_version` and `compare_versions` tests fail because those are private — add `pub(crate)` to them or move the tests adjacent to their declarations.

- [ ] **Step 3: Make `parse_idl_version` and `compare_versions` visible to tests**

Both are already in the same module as `tests`, so `#[cfg(test)]` tests can call them directly. If tests still fail, check that the function names match exactly.

- [ ] **Step 4: Run all skill tests**

```bash
cargo nextest run -p cairn-cli --locked
```

Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/skill.rs
git commit -m "test(cli/skill): version upgrade/downgrade tests (§18.d, #68)"
```

---

## Task 9: `install()` — symlink rejection — TDD

**Files:**
- Modify: `crates/cairn-cli/src/skill.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
#[test]
#[cfg(unix)]
fn install_rejects_symlinked_target() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().expect("tempdir");
    let real_dir = tmp.path().join("real");
    std::fs::create_dir_all(&real_dir).expect("create real dir");

    let link = tmp.path().join("link");
    symlink(&real_dir, &link).expect("create symlink");

    let opts = InstallOpts {
        target_dir: link.clone(),
        harness: Harness::ClaudeCode,
        force: false,
    };
    let result = install(&opts);
    assert!(result.is_err(), "install into a symlinked dir must fail");
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("symlink"),
        "error message should mention symlink — got: {msg}"
    );
}
```

- [ ] **Step 2: Run to confirm it fails**

```bash
cargo nextest run -p cairn-cli install_rejects_symlinked_target --locked
```

Expected: FAIL — install doesn't check for symlinks yet... Actually the check IS already in Task 6's `install()`. Run to confirm it passes.

- [ ] **Step 3: Run all skill tests**

```bash
cargo nextest run -p cairn-cli --locked
```

Expected: all pass. If the symlink test was already passing from the Task 6 implementation, this task is confirmation only.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/src/skill.rs
git commit -m "test(cli/skill): symlink rejection test (§18.d, #68)"
```

---

## Task 10: `render_human()` + snapshot tests

**Files:**
- Modify: `crates/cairn-cli/src/skill.rs`

- [ ] **Step 1: Write the snapshot tests first**

Add to the `tests` module:

```rust
#[test]
fn render_human_snapshot_fresh_install() {
    let receipt = InstallReceipt {
        target_dir: PathBuf::from("/home/user/.cairn/skills/cairn"),
        contract_version: "cairn.mcp.v1".to_owned(),
        idl_version: "0.0.1".to_owned(),
        files_created: vec![
            PathBuf::from("/home/user/.cairn/skills/cairn/SKILL.md"),
            PathBuf::from("/home/user/.cairn/skills/cairn/conventions.md"),
            PathBuf::from("/home/user/.cairn/skills/cairn/.version"),
            PathBuf::from("/home/user/.cairn/skills/cairn/examples/01-remember-preference.md"),
        ],
        files_skipped: vec![],
        registration_hint: "# Add to your CLAUDE.md:\n@~/.cairn/skills/cairn/SKILL.md"
            .to_owned(),
    };
    insta::assert_snapshot!(render_human(&receipt));
}

#[test]
fn render_human_snapshot_already_installed() {
    let receipt = InstallReceipt {
        target_dir: PathBuf::from("/home/user/.cairn/skills/cairn"),
        contract_version: "cairn.mcp.v1".to_owned(),
        idl_version: "0.0.1".to_owned(),
        files_created: vec![],
        files_skipped: vec![
            PathBuf::from("/home/user/.cairn/skills/cairn/SKILL.md"),
            PathBuf::from("/home/user/.cairn/skills/cairn/conventions.md"),
            PathBuf::from("/home/user/.cairn/skills/cairn/.version"),
        ],
        registration_hint: "# Add to your CLAUDE.md:\n@~/.cairn/skills/cairn/SKILL.md"
            .to_owned(),
    };
    insta::assert_snapshot!(render_human(&receipt));
}

#[test]
fn receipt_json_snapshot() {
    let receipt = InstallReceipt {
        target_dir: PathBuf::from("/home/user/.cairn/skills/cairn"),
        contract_version: "cairn.mcp.v1".to_owned(),
        idl_version: "0.0.1".to_owned(),
        files_created: vec![PathBuf::from(
            "/home/user/.cairn/skills/cairn/SKILL.md",
        )],
        files_skipped: vec![],
        registration_hint: "# Add to your CLAUDE.md:\n@~/.cairn/skills/cairn/SKILL.md"
            .to_owned(),
    };
    insta::assert_json_snapshot!(receipt);
}
```

Also add `use insta;` at the top of the `tests` module if needed (it's already a dev-dep).

- [ ] **Step 2: Run to confirm tests fail (no snapshot yet)**

```bash
cargo nextest run -p cairn-cli "render_human|receipt_json" --locked
```

Expected: FAIL — `render_human` not defined + no snapshot files exist.

- [ ] **Step 3: Implement `render_human()`**

Add to `skill.rs` (above the `#[cfg(test)]` block):

```rust
/// Renders a human-readable summary of an install receipt.
#[must_use]
pub fn render_human(receipt: &InstallReceipt) -> String {
    let header = if receipt.files_created.is_empty() && receipt.files_skipped.is_empty() {
        format!(
            "cairn skill install: nothing to do at {}",
            receipt.target_dir.display()
        )
    } else if receipt.files_created.is_empty() {
        format!(
            "cairn skill install: already up to date at {} (v{})\n  (pass --force to overwrite generated files)",
            receipt.target_dir.display(),
            receipt.idl_version,
        )
    } else {
        format!(
            "cairn skill install: skill bundle installed at {}",
            receipt.target_dir.display()
        )
    };

    let mut lines = vec![header];
    for path in &receipt.files_created {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        lines.push(format!("  {name}  [created]"));
    }
    for path in &receipt.files_skipped {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        lines.push(format!("  {name}  [skipped]"));
    }

    if !receipt.registration_hint.is_empty() {
        lines.push(String::new());
        lines.push("Registration hint:".to_owned());
        for hint_line in receipt.registration_hint.lines() {
            lines.push(format!("  {hint_line}"));
        }
    }

    lines.join("\n")
}
```

- [ ] **Step 4: Run tests to generate initial snapshots**

```bash
cargo nextest run -p cairn-cli "render_human|receipt_json" --locked
```

Expected: FAIL with "snapshot value not found" — insta captures the output for review.

- [ ] **Step 5: Review and accept snapshots**

```bash
cargo insta review
```

Inspect the proposed snapshots. Accept them if the output looks correct (files listed, registration hint present).

- [ ] **Step 6: Run tests to confirm they pass**

```bash
cargo nextest run -p cairn-cli "render_human|receipt_json" --locked
```

Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-cli/src/skill.rs crates/cairn-cli/src/snapshots/
git commit -m "feat(cli/skill): render_human() + snapshot tests (§18.d, #68)"
```

---

## Task 11: Wire `skill install` into `main.rs`

**Files:**
- Modify: `crates/cairn-cli/src/main.rs`

- [ ] **Step 1: Add the skill subcommand builder**

Add this function to `main.rs` (alongside `bootstrap_subcommand()`):

```rust
fn skill_subcommand() -> clap::Command {
    clap::Command::new("skill")
        .about("Manage the Cairn skill bundle")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("install")
                .about("Install the Cairn skill bundle into the harness skill directory (§18.d)")
                .arg(
                    clap::Arg::new("harness")
                        .long("harness")
                        .required(true)
                        .value_name("HARNESS")
                        .value_parser(clap::builder::EnumValueParser::<cairn_cli::skill::Harness>::new())
                        .help("Target harness (claude-code, codex, gemini, opencode, cursor, custom)"),
                )
                .arg(
                    clap::Arg::new("target-dir")
                        .long("target-dir")
                        .value_name("PATH")
                        .help("Override the default install path (~/.cairn/skills/cairn/)"),
                )
                .arg(
                    clap::Arg::new("force")
                        .long("force")
                        .action(clap::ArgAction::SetTrue)
                        .help("Overwrite generated files even if the version matches"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit JSON receipt instead of human-readable output"),
                ),
        )
}
```

- [ ] **Step 2: Register it in `build_command()`**

In `build_command()`, after `.subcommand(bootstrap_subcommand())`, add:

```rust
.subcommand(skill_subcommand())
```

- [ ] **Step 3: Add the dispatch function**

Add `run_skill()` to `main.rs`:

```rust
fn run_skill(matches: &ArgMatches) -> ExitCode {
    match matches.subcommand() {
        Some(("install", sub)) => run_skill_install(sub),
        _ => unreachable!(
            "clap subcommand_required(true) on skill ensures a subcommand is always present"
        ),
    }
}

fn run_skill_install(matches: &ArgMatches) -> ExitCode {
    let harness = matches
        .get_one::<cairn_cli::skill::Harness>("harness")
        .expect("invariant: --harness is required by clap")
        .clone();

    let target_dir = if let Some(path) = matches.get_one::<String>("target-dir") {
        std::path::PathBuf::from(path)
    } else {
        match cairn_cli::skill::default_target_dir() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("cairn skill install: {e:#}");
                return ExitCode::from(69); // EX_UNAVAILABLE
            }
        }
    };

    let force = matches.get_flag("force");
    let json = matches.get_flag("json");

    let opts = cairn_cli::skill::InstallOpts {
        target_dir,
        harness,
        force,
    };

    match cairn_cli::skill::install(&opts) {
        Ok(receipt) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&receipt)
                        .expect("invariant: InstallReceipt is always serializable")
                );
            } else {
                println!("{}", cairn_cli::skill::render_human(&receipt));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cairn skill install: {e:#}");
            ExitCode::from(74) // EX_IOERR
        }
    }
}
```

- [ ] **Step 4: Add `skill` to the match in `main()`**

In the `match matches.subcommand()` block, add after `Some(("bootstrap", sub)) => run_bootstrap(sub),`:

```rust
Some(("skill", sub)) => run_skill(sub),
```

- [ ] **Step 5: Smoke test — help output**

```bash
cargo run -p cairn-cli --locked -- skill --help
```

Expected output includes:
```
Manage the Cairn skill bundle

Usage: cairn skill <COMMAND>

Commands:
  install  Install the Cairn skill bundle into the harness skill directory (§18.d)
```

```bash
cargo run -p cairn-cli --locked -- skill install --help
```

Expected output includes `--harness`, `--target-dir`, `--force`, `--json`.

- [ ] **Step 6: Smoke test — actual install into a temp path**

```bash
TESTDIR=$(mktemp -d) && cargo run -p cairn-cli --locked -- skill install \
  --harness claude-code \
  --target-dir "$TESTDIR/cairn-skills"
```

Expected: human-readable output showing files created and the CLAUDE.md registration hint.

```bash
cargo run -p cairn-cli --locked -- skill install \
  --harness claude-code \
  --target-dir "$TESTDIR/cairn-skills" \
  --json
```

Expected: JSON receipt with `files_skipped` populated (second run is idempotent).

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-cli/src/main.rs
git commit -m "feat(cli): wire cairn skill install subcommand (§18.d, #68)"
```

---

## Task 12: Run full verification checklist

**Files:** none (verification only)

- [ ] **Step 1: Format check**

```bash
cargo fmt --all --check
```

Expected: exits 0. If not, run `cargo fmt --all` and commit.

- [ ] **Step 2: Clippy**

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Expected: exits 0 with no warnings. Fix any pedantic warnings (common ones: `needless_pass_by_value`, `must_use`, `missing_errors_doc`). Add a one-line reason comment if you must locally suppress.

- [ ] **Step 3: Check + build**

```bash
cargo check --workspace --all-targets --locked
```

Expected: exits 0.

- [ ] **Step 4: Full test suite**

```bash
cargo nextest run --workspace --locked --no-fail-fast
```

Expected: all tests pass.

- [ ] **Step 5: Doctests**

```bash
cargo test --doc --workspace --locked
```

Expected: exits 0.

- [ ] **Step 6: Core boundary check**

```bash
./scripts/check-core-boundary.sh
```

Expected: exits 0. (`cairn-core` must have no deps on other workspace crates.)

- [ ] **Step 7: Codegen check**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: exits 0 (no drift in generated files).

- [ ] **Step 8: Commit any formatting fixes**

If `cargo fmt` produced changes:

```bash
git add -p   # stage only formatting changes
git commit -m "style: cargo fmt after skill install wiring (#68)"
```

---

## Acceptance criteria verification

After Task 12 passes, confirm each acceptance criterion from issue #68:

- [ ] **"The installed skill names when to call Cairn and how to format requests."**
  Run `cat ~/.cairn/skills/cairn/SKILL.md` (or the `--target-dir` you used in the smoke test). Should show verb trigger phrases and output format rules.

- [ ] **"The kind cheat-sheet is generated from the canonical taxonomy."**
  Run `cat ~/.cairn/skills/cairn/conventions.md`. Should list all 8 kinds with descriptions.

- [ ] **"Reinstall updates generated content while preserving user-local notes if supported."**
  Edit `examples/01-remember-preference.md` manually in the install dir. Re-run `cairn skill install --harness claude-code`. Confirm the file is listed in `files_skipped`, not `files_created`.
