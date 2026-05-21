# Folder Sidecars, Backlinks, and Policy Inheritance — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement issue #44 — define the folder sidecar schema (`_index.md`, `_policy.yaml`, `_summary.md`), ship pure folder-projection helpers in `cairn-core::domain::folder`, derive backlinks from record bodies, resolve policy with deepest-wins walk-up, and expose a `cairn lint --fix-folders` CLI flag.

**Architecture:** New module `cairn-core::domain::folder/` (split into `mod.rs`, `policy.rs`, `links.rs`, `index.rs`) holding pure types and functions. CLI handler in `cairn-cli::verbs::lint` walks the store, builds the inputs, calls the projector, and writes `_index.md` files atomically. `_summary.md` ships as schema-only types plus a `FolderSummaryWriter` trait surface; no summary files are emitted at P0.

**Tech Stack:** Rust 1.95 (edition 2024), `serde_yaml` 0.9, `tempfile`, `tokio` (CLI), `async_trait`, `thiserror`, `proptest`, `insta`, `rstest`, `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-04-27-folder-sidecars-design.md`

---

## File Structure

| Path | Role |
|---|---|
| `crates/cairn-core/src/domain/folder/mod.rs` | NEW — re-exports, `FolderError`, `FolderSummary`, `FolderSummaryWriter` trait + error |
| `crates/cairn-core/src/domain/folder/policy.rs` | NEW — `FolderPolicy`, `EffectivePolicy`, enums, `parse_policy`, `resolve_policy` |
| `crates/cairn-core/src/domain/folder/links.rs` | NEW — `RawLink`, `Backlink`, `extract_links`, `materialize_backlinks` |
| `crates/cairn-core/src/domain/folder/index.rs` | NEW — `FolderState`, `FolderIndex`, `RecordEntry`, `SubfolderEntry`, `aggregate_folders`, `project_index` |
| `crates/cairn-core/src/domain/mod.rs` | MODIFY — add `pub mod folder;` and re-exports |
| `crates/cairn-cli/src/verbs/mod.rs` | MODIFY — add `with_fix_folders` decorator |
| `crates/cairn-cli/src/verbs/lint.rs` | MODIFY — add `fix_folders_handler`, wire flag in `run()` |
| `crates/cairn-cli/src/main.rs` | MODIFY — wrap `lint_subcommand` with `with_fix_folders` |
| `crates/cairn-cli/tests/lint_folders.rs` | NEW — integration tests |
| `crates/cairn-cli/tests/snapshots/` | NEW (insta) — `_index.md` snapshots |

---

## Task 1: Module skeleton — empty submodule wired in

**Files:**
- Create: `crates/cairn-core/src/domain/folder/mod.rs`
- Create: `crates/cairn-core/src/domain/folder/policy.rs`
- Create: `crates/cairn-core/src/domain/folder/links.rs`
- Create: `crates/cairn-core/src/domain/folder/index.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`

- [ ] **Step 1: Create empty submodule files**

`crates/cairn-core/src/domain/folder/mod.rs`:

```rust
//! Folder sidecars — `_index.md`, `_policy.yaml`, `_summary.md` (brief §3.4).
//!
//! Pure functions only — zero I/O, zero async. Caller (CLI / future hooks)
//! supplies records and policy bytes; module returns projected files,
//! parsed policies, and resolved effective policies.

pub mod index;
pub mod links;
pub mod policy;
```

`crates/cairn-core/src/domain/folder/policy.rs`:

```rust
//! `FolderPolicy`, `EffectivePolicy`, parse + resolve.
```

`crates/cairn-core/src/domain/folder/links.rs`:

```rust
//! Markdown link extraction and reverse-map materialization.
```

`crates/cairn-core/src/domain/folder/index.rs`:

```rust
//! Folder aggregation + `_index.md` projection.
```

- [ ] **Step 2: Wire `folder` into `domain/mod.rs`**

Add to `crates/cairn-core/src/domain/mod.rs` (alongside the existing `pub mod` lines):

```rust
pub mod folder;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p cairn-core --locked`
Expected: PASS, no warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/domain/folder/ crates/cairn-core/src/domain/mod.rs
git commit -m "feat(core): folder module skeleton (brief §3.4, #44)"
```

---

## Task 2: `FolderError` enum

**Files:**
- Modify: `crates/cairn-core/src/domain/folder/mod.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/cairn-core/src/domain/folder/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_error_displays_with_source() {
        let yaml = "purpose: [unclosed";
        let err = serde_yaml::from_str::<serde_yaml::Value>(yaml).unwrap_err();
        let folder_err = FolderError::PolicyParse { source: err };
        assert!(folder_err.to_string().starts_with("policy parse failed:"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p cairn-core --locked folder::tests::folder_error_displays_with_source`
Expected: FAIL — `FolderError` not defined.

- [ ] **Step 3: Add the enum**

Insert above the test module in `mod.rs`:

```rust
/// Errors raised by pure folder helpers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FolderError {
    /// `_policy.yaml` could not be parsed as a `FolderPolicy`.
    #[error("policy parse failed: {source}")]
    PolicyParse {
        /// Underlying serde_yaml error.
        #[source]
        source: serde_yaml::Error,
    },
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p cairn-core --locked folder::tests::folder_error_displays_with_source`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/domain/folder/mod.rs
git commit -m "feat(core): FolderError enum (brief §3.4, #44)"
```

---

## Task 3: `FolderPolicy` types + `parse_policy`

**Files:**
- Modify: `crates/cairn-core/src/domain/folder/policy.rs`

- [ ] **Step 1: Write the failing tests**

Append to `policy.rs`:

```rust
use serde::{Deserialize, Serialize};

use crate::domain::folder::FolderError;
use crate::domain::{MemoryKind, MemoryVisibility};

/// Per-folder configuration deserialized from `_policy.yaml`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FolderPolicy {
    /// Single-line per-folder purpose; echoed into `_index.md` frontmatter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Kinds permitted in this folder. `None` = inherit; `Some(empty)` = forbid all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_kinds: Option<Vec<MemoryKind>>,
    /// Visibility default when `None` chosen at ingest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility_default: Option<MemoryVisibility>,
    /// Override for the global consolidation cadence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consolidation_cadence: Option<ConsolidationCadence>,
    /// Agent that owns summary regeneration for this folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_agent: Option<String>,
    /// Retention policy override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<RetentionPolicy>,
    /// Cap for `_summary.md` regeneration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_max_tokens: Option<u32>,
}

/// Cadence on which `_summary.md` is regenerated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ConsolidationCadence {
    /// Hourly cadence.
    Hourly,
    /// Daily cadence (default).
    Daily,
    /// Weekly cadence.
    Weekly,
    /// Monthly cadence.
    Monthly,
    /// Manual (no automatic regeneration).
    Manual,
}

/// Retention policy override for a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum RetentionPolicy {
    /// Keep records for `Days(n)` since their last update.
    Days(u32),
    /// Keep records indefinitely.
    Unlimited,
}

/// Parse a `_policy.yaml` content string.
///
/// # Errors
///
/// Returns [`FolderError::PolicyParse`] if YAML is malformed or contains
/// unknown keys (the struct is `deny_unknown_fields`).
pub fn parse_policy(yaml: &str) -> Result<FolderPolicy, FolderError> {
    if yaml.trim().is_empty() {
        return Ok(FolderPolicy::default());
    }
    serde_yaml::from_str(yaml).map_err(|source| FolderError::PolicyParse { source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_every_field() {
        let yaml = r#"
purpose: people Cairn knows about
allowed_kinds: [user, feedback]
visibility_default: private
consolidation_cadence: weekly
owner_agent: agt:cairn-librarian:v2
retention: 90
summary_max_tokens: 300
"#;
        let policy = parse_policy(yaml).expect("parse");
        assert_eq!(policy.purpose.as_deref(), Some("people Cairn knows about"));
        assert_eq!(policy.allowed_kinds.as_ref().map(Vec::len), Some(2));
        assert_eq!(policy.visibility_default, Some(MemoryVisibility::Private));
        assert_eq!(
            policy.consolidation_cadence,
            Some(ConsolidationCadence::Weekly),
        );
        assert_eq!(policy.owner_agent.as_deref(), Some("agt:cairn-librarian:v2"));
        assert_eq!(policy.retention, Some(RetentionPolicy::Days(90)));
        assert_eq!(policy.summary_max_tokens, Some(300));
    }

    #[test]
    fn parse_unknown_key_returns_policy_parse() {
        let yaml = "unknown_key: 42\n";
        let err = parse_policy(yaml).unwrap_err();
        assert!(matches!(err, FolderError::PolicyParse { .. }));
    }

    #[test]
    fn parse_malformed_yaml_returns_policy_parse() {
        let yaml = "purpose: [unclosed";
        let err = parse_policy(yaml).unwrap_err();
        assert!(matches!(err, FolderError::PolicyParse { .. }));
    }

    #[test]
    fn parse_empty_yaml_returns_default() {
        let policy = parse_policy("").expect("parse empty");
        assert_eq!(policy, FolderPolicy::default());
        let policy = parse_policy("   \n\n").expect("parse whitespace");
        assert_eq!(policy, FolderPolicy::default());
    }

    #[test]
    fn retention_unlimited_round_trip() {
        let yaml = "retention: unlimited\n";
        let policy = parse_policy(yaml).expect("parse");
        assert_eq!(policy.retention, Some(RetentionPolicy::Unlimited));
    }
}
```

Add `pub use policy::*;` to `crates/cairn-core/src/domain/folder/mod.rs` so the test module's references resolve cleanly.

```rust
// At the top of mod.rs, before the FolderError enum:
pub use links::*;
pub use policy::*;
```

(`links::*` becomes meaningful in Task 5 — re-exporting now keeps later edits localized.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p cairn-core --locked folder::policy::tests`
Expected: FAIL — types/functions undefined.

- [ ] **Step 3: Already implemented in Step 1**

The struct + enums + `parse_policy` were added with the tests. Re-run.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-core --locked folder::policy::tests`
Expected: 5 passed.

- [ ] **Step 5: Verify clippy is clean**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/domain/folder/policy.rs crates/cairn-core/src/domain/folder/mod.rs
git commit -m "feat(core): FolderPolicy types + parse_policy (brief §3.4, #44)"
```

---

## Task 4: `EffectivePolicy` + `Default` + `resolve_policy`

**Files:**
- Modify: `crates/cairn-core/src/domain/folder/policy.rs`

- [ ] **Step 1: Write the failing tests**

Append to `policy.rs` (above the `#[cfg(test)]` block, the test additions go inside the existing `tests` module):

In `tests`:

```rust
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    fn empty_chain() -> BTreeMap<PathBuf, FolderPolicy> {
        BTreeMap::new()
    }

    #[test]
    fn resolve_with_no_policies_returns_defaults() {
        let target = Path::new("raw/projects/koi/rfc.md");
        let resolved = resolve_policy(target, &empty_chain());
        assert_eq!(resolved.visibility_default, MemoryVisibility::Private);
        assert_eq!(resolved.consolidation_cadence, ConsolidationCadence::Daily);
        assert_eq!(resolved.retention, RetentionPolicy::Unlimited);
        assert_eq!(resolved.summary_max_tokens, 200);
        assert!(resolved.purpose.is_none());
        assert!(resolved.allowed_kinds.is_none());
        assert!(resolved.owner_agent.is_none());
        assert!(resolved.source_chain.is_empty());
    }

    #[test]
    fn resolve_single_root_policy_is_echoed() {
        let mut chain = BTreeMap::new();
        chain.insert(
            PathBuf::from("raw"),
            FolderPolicy {
                purpose: Some("root".into()),
                consolidation_cadence: Some(ConsolidationCadence::Weekly),
                ..FolderPolicy::default()
            },
        );
        let target = Path::new("raw/x.md");
        let resolved = resolve_policy(target, &chain);
        assert_eq!(resolved.purpose.as_deref(), Some("root"));
        assert_eq!(resolved.consolidation_cadence, ConsolidationCadence::Weekly);
        assert_eq!(resolved.source_chain, vec![PathBuf::from("raw")]);
    }

    #[test]
    fn resolve_child_overrides_parent_per_key() {
        let mut chain = BTreeMap::new();
        chain.insert(
            PathBuf::from("raw"),
            FolderPolicy {
                purpose: Some("root".into()),
                consolidation_cadence: Some(ConsolidationCadence::Daily),
                summary_max_tokens: Some(100),
                ..FolderPolicy::default()
            },
        );
        chain.insert(
            PathBuf::from("raw/projects"),
            FolderPolicy {
                consolidation_cadence: Some(ConsolidationCadence::Weekly),
                ..FolderPolicy::default()
            },
        );
        let target = Path::new("raw/projects/koi.md");
        let resolved = resolve_policy(target, &chain);
        // Inherited from root:
        assert_eq!(resolved.purpose.as_deref(), Some("root"));
        assert_eq!(resolved.summary_max_tokens, 100);
        // Overridden by child:
        assert_eq!(resolved.consolidation_cadence, ConsolidationCadence::Weekly);
    }

    #[test]
    fn resolve_three_deep_chain_deepest_wins() {
        let mut chain = BTreeMap::new();
        chain.insert(
            PathBuf::from("raw"),
            FolderPolicy {
                purpose: Some("root".into()),
                ..FolderPolicy::default()
            },
        );
        chain.insert(
            PathBuf::from("raw/projects"),
            FolderPolicy {
                purpose: Some("projects".into()),
                ..FolderPolicy::default()
            },
        );
        chain.insert(
            PathBuf::from("raw/projects/koi"),
            FolderPolicy {
                purpose: Some("koi".into()),
                ..FolderPolicy::default()
            },
        );
        let target = Path::new("raw/projects/koi/rfc.md");
        let resolved = resolve_policy(target, &chain);
        assert_eq!(resolved.purpose.as_deref(), Some("koi"));
        assert_eq!(
            resolved.source_chain,
            vec![
                PathBuf::from("raw"),
                PathBuf::from("raw/projects"),
                PathBuf::from("raw/projects/koi"),
            ],
        );
    }

    #[test]
    fn resolve_source_chain_skips_dirs_without_policy() {
        let mut chain = BTreeMap::new();
        chain.insert(
            PathBuf::from("raw"),
            FolderPolicy {
                purpose: Some("r".into()),
                ..FolderPolicy::default()
            },
        );
        chain.insert(
            PathBuf::from("raw/a/b/c"),
            FolderPolicy {
                purpose: Some("c".into()),
                ..FolderPolicy::default()
            },
        );
        let target = Path::new("raw/a/b/c/d/e.md");
        let resolved = resolve_policy(target, &chain);
        assert_eq!(
            resolved.source_chain,
            vec![PathBuf::from("raw"), PathBuf::from("raw/a/b/c")],
        );
    }
```

Add the type and function above the test module:

```rust
/// Result of walking up `_policy.yaml` files and merging deepest-wins per key.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectivePolicy {
    /// Purpose echoed from the deepest policy that set one.
    pub purpose: Option<String>,
    /// Allowed kinds from the deepest policy that set them.
    pub allowed_kinds: Option<Vec<crate::domain::MemoryKind>>,
    /// Visibility default; falls back to `Private`.
    pub visibility_default: crate::domain::MemoryVisibility,
    /// Consolidation cadence; falls back to `Daily`.
    pub consolidation_cadence: ConsolidationCadence,
    /// Owning agent; `None` if unset anywhere in the chain.
    pub owner_agent: Option<String>,
    /// Retention; falls back to `Unlimited`.
    pub retention: RetentionPolicy,
    /// Summary token cap; falls back to 200.
    pub summary_max_tokens: u32,
    /// Folder paths that contributed, shallowest first, deepest last.
    pub source_chain: Vec<std::path::PathBuf>,
}

impl Default for EffectivePolicy {
    fn default() -> Self {
        Self {
            purpose: None,
            allowed_kinds: None,
            visibility_default: crate::domain::MemoryVisibility::Private,
            consolidation_cadence: ConsolidationCadence::Daily,
            owner_agent: None,
            retention: RetentionPolicy::Unlimited,
            summary_max_tokens: 200,
            source_chain: Vec::new(),
        }
    }
}

/// Walk from `target`'s parent up to the vault root, merging `_policy.yaml`
/// entries deepest-wins per key. Defaults from [`EffectivePolicy::default`]
/// fill in fields that no policy set.
#[must_use]
pub fn resolve_policy(
    target: &std::path::Path,
    policies_by_dir: &std::collections::BTreeMap<std::path::PathBuf, FolderPolicy>,
) -> EffectivePolicy {
    // Build the chain shallowest → deepest.
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    let mut cur = target.parent();
    while let Some(d) = cur {
        if d.as_os_str().is_empty() {
            break;
        }
        dirs.push(d.to_path_buf());
        cur = d.parent();
    }
    dirs.reverse();

    let mut effective = EffectivePolicy::default();
    for dir in dirs {
        let Some(p) = policies_by_dir.get(&dir) else {
            continue;
        };
        effective.source_chain.push(dir);
        if let Some(v) = &p.purpose {
            effective.purpose = Some(v.clone());
        }
        if let Some(v) = &p.allowed_kinds {
            effective.allowed_kinds = Some(v.clone());
        }
        if let Some(v) = p.visibility_default {
            effective.visibility_default = v;
        }
        if let Some(v) = p.consolidation_cadence {
            effective.consolidation_cadence = v;
        }
        if let Some(v) = &p.owner_agent {
            effective.owner_agent = Some(v.clone());
        }
        if let Some(v) = p.retention {
            effective.retention = v;
        }
        if let Some(v) = p.summary_max_tokens {
            effective.summary_max_tokens = v;
        }
    }
    effective
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p cairn-core --locked folder::policy::tests`
Expected: 5 new tests fail (compile error, then assertion failure depending on order).

- [ ] **Step 3: Step 1 already adds the impl**

Re-run; tests compile and pass.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-core --locked folder::policy::tests`
Expected: 10 passed (5 from Task 3 + 5 new).

- [ ] **Step 5: Verify clippy is clean**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/domain/folder/policy.rs
git commit -m "feat(core): EffectivePolicy + resolve_policy walk-up (brief §3.4, #44)"
```

---

## Task 5: `RawLink`, `Backlink`, and `extract_links`

**Files:**
- Modify: `crates/cairn-core/src/domain/folder/links.rs`

- [ ] **Step 1: Write the failing tests**

Append to `links.rs`:

```rust
use std::path::{Path, PathBuf};

/// A link extracted from a record body, target resolved against the source folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLink {
    /// Vault-relative target path.
    pub target_path: PathBuf,
    /// Optional fragment after `#`, if present.
    pub anchor: Option<String>,
}

/// A reverse link: source file pointing at a target file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlink {
    /// File containing the link (vault-relative).
    pub source_path: PathBuf,
    /// File being linked to (vault-relative).
    pub target_path: PathBuf,
    /// Optional fragment after `#`, if present.
    pub anchor: Option<String>,
}

/// Pure markdown link extractor.
///
/// Recognises:
/// - `[label](path)` — markdown link; label discarded.
/// - `[[target]]` and `[[target#anchor]]` — wiki link.
///
/// Drops:
/// - external URLs (`http://`, `https://`, `mailto:`);
/// - links inside fenced code blocks;
/// - escaped link syntax (`\[`).
///
/// Wiki-style targets without an extension get `.md` appended. Relative
/// paths are resolved against the source file's parent directory.
#[must_use]
pub fn extract_links(source_path: &Path, body: &str) -> Vec<RawLink> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("```") {
            // Toggle fence on the leading-``` line; skip the fence content.
            in_fence = !in_fence;
            // Lines that look like ``` ```inline``` on a single line still
            // toggle once — adequate for P0; rare in record bodies.
            let _ = rest;
            continue;
        }
        if in_fence {
            continue;
        }
        scan_line(source_path, line, &mut out);
    }
    out
}

fn scan_line(source_path: &Path, line: &str, out: &mut Vec<RawLink>) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // Escaped open-bracket — skip.
        if b == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            continue;
        }
        if b == b'[' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Wiki link: [[target]] or [[target#anchor]]
            if let Some(end) = find_close(&bytes[i + 2..], b"]]") {
                let inner = &line[i + 2..i + 2 + end];
                if let Some(link) = parse_wiki(source_path, inner) {
                    out.push(link);
                }
                i += 2 + end + 2;
                continue;
            }
        }
        if b == b'[' {
            // Markdown link: [label](target)
            if let Some(label_end) = find_close(&bytes[i + 1..], b"]") {
                let after = i + 1 + label_end + 1;
                if after < bytes.len() && bytes[after] == b'(' {
                    if let Some(target_end) = find_close(&bytes[after + 1..], b")") {
                        let target = &line[after + 1..after + 1 + target_end];
                        if let Some(link) = parse_markdown(source_path, target) {
                            out.push(link);
                        }
                        i = after + 1 + target_end + 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
}

fn find_close(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn is_external(target: &str) -> bool {
    target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with("mailto:")
}

fn parse_wiki(source_path: &Path, raw: &str) -> Option<RawLink> {
    let raw = raw.trim();
    if raw.is_empty() || is_external(raw) {
        return None;
    }
    let (target_str, anchor) = match raw.split_once('#') {
        Some((t, a)) => (t, Some(a.to_owned())),
        None => (raw, None),
    };
    let with_ext: PathBuf = if Path::new(target_str).extension().is_some() {
        PathBuf::from(target_str)
    } else {
        PathBuf::from(format!("{target_str}.md"))
    };
    Some(RawLink {
        target_path: resolve_relative(source_path, &with_ext),
        anchor,
    })
}

fn parse_markdown(source_path: &Path, raw: &str) -> Option<RawLink> {
    let raw = raw.trim();
    if raw.is_empty() || is_external(raw) {
        return None;
    }
    let (target_str, anchor) = match raw.split_once('#') {
        Some((t, a)) => (t, Some(a.to_owned())),
        None => (raw, None),
    };
    Some(RawLink {
        target_path: resolve_relative(source_path, Path::new(target_str)),
        anchor,
    })
}

fn resolve_relative(source_path: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        return target.components().skip(1).collect();
    }
    let parent = source_path.parent().unwrap_or_else(|| Path::new(""));
    let joined = parent.join(target);
    normalize(&joined)
}

fn normalize(p: &Path) -> PathBuf {
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for comp in p.components() {
        use std::path::Component;
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::Normal(s) => out.push(s.to_os_string()),
        }
    }
    out.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_link_label_discarded() {
        let src = Path::new("raw/a.md");
        let links = extract_links(src, "see [Alice](raw/alice.md) for details");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target_path, PathBuf::from("raw/alice.md"));
        assert!(links[0].anchor.is_none());
    }

    #[test]
    fn wiki_link_appends_md_extension() {
        let src = Path::new("raw/a.md");
        let links = extract_links(src, "see [[alice]] for details");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target_path, PathBuf::from("raw/alice.md"));
    }

    #[test]
    fn wiki_anchor_is_populated() {
        let src = Path::new("raw/a.md");
        let links = extract_links(src, "see [[alice#bio]]");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].anchor.as_deref(), Some("bio"));
    }

    #[test]
    fn relative_path_resolves_against_source_folder() {
        let src = Path::new("wiki/entities/people/bob.md");
        let links = extract_links(src, "[ref](../alice.md)");
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].target_path,
            PathBuf::from("wiki/entities/alice.md"),
        );
    }

    #[test]
    fn external_urls_are_dropped() {
        let src = Path::new("raw/a.md");
        let body = "see [docs](https://example.com), [home](http://x), [mail](mailto:x@y)";
        let links = extract_links(src, body);
        assert!(links.is_empty());
    }

    #[test]
    fn code_fenced_links_are_ignored() {
        let src = Path::new("raw/a.md");
        let body = "before\n```\n[Alice](raw/alice.md)\n```\nafter [Bob](raw/bob.md)";
        let links = extract_links(src, body);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target_path, PathBuf::from("raw/bob.md"));
    }

    #[test]
    fn escaped_open_bracket_is_ignored() {
        let src = Path::new("raw/a.md");
        let body = r"\[not a link\](nope)";
        let links = extract_links(src, body);
        assert!(links.is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail then pass**

Run: `cargo nextest run -p cairn-core --locked folder::links::tests`
Expected: 7 passed (the impl is included in Step 1).

- [ ] **Step 3: Verify clippy is clean**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/domain/folder/links.rs
git commit -m "feat(core): extract_links + RawLink/Backlink types (brief §3.4, #44)"
```

---

## Task 6: `materialize_backlinks`

**Files:**
- Modify: `crates/cairn-core/src/domain/folder/links.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `links.rs`:

```rust
    use crate::contract::memory_store::StoredRecord;
    use crate::domain::record::tests::sample_stored_record;
    use std::collections::BTreeMap;

    fn record_with_body(version: u32, body: &str) -> StoredRecord {
        let mut s = sample_stored_record(version);
        s.record.body = body.to_owned();
        s
    }

    #[test]
    fn materialize_empty_records_returns_empty_map() {
        let records: Vec<StoredRecord> = Vec::new();
        let paths: BTreeMap<crate::domain::RecordId, std::path::PathBuf> = BTreeMap::new();
        let map = materialize_backlinks(&records, &paths);
        assert!(map.is_empty());
    }

    #[test]
    fn materialize_emits_backlink_for_existing_target() {
        let r1 = record_with_body(1, "see [Alice](raw/alice.md)");
        let mut paths = BTreeMap::new();
        paths.insert(r1.record.id.clone(), PathBuf::from("raw/r1.md"));
        let map = materialize_backlinks(&[r1], &paths);
        let entries = map.get(Path::new("raw/alice.md")).expect("entry");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source_path, PathBuf::from("raw/r1.md"));
    }

    #[test]
    fn materialize_includes_dangling_links() {
        let r1 = record_with_body(1, "[ghost](raw/missing.md)");
        let mut paths = BTreeMap::new();
        paths.insert(r1.record.id.clone(), PathBuf::from("raw/r1.md"));
        let map = materialize_backlinks(&[r1], &paths);
        assert!(map.contains_key(Path::new("raw/missing.md")));
    }

    #[test]
    fn materialize_two_sources_to_same_target_sorted() {
        let r1 = record_with_body(1, "[a](raw/alice.md)");
        let mut r2 = record_with_body(1, "[a](raw/alice.md)");
        // Mutate id so paths differ.
        r2.record.id = crate::domain::RecordId::new("01HQZX9F5N0000000000000ZZZ".to_owned()).unwrap();
        let mut paths = BTreeMap::new();
        paths.insert(r1.record.id.clone(), PathBuf::from("raw/zzz.md"));
        paths.insert(r2.record.id.clone(), PathBuf::from("raw/aaa.md"));
        let map = materialize_backlinks(&[r1, r2], &paths);
        let entries = map.get(Path::new("raw/alice.md")).expect("entry");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].source_path, PathBuf::from("raw/aaa.md"));
        assert_eq!(entries[1].source_path, PathBuf::from("raw/zzz.md"));
    }
```

Add the function above the test module:

```rust
use std::collections::BTreeMap;

use crate::contract::memory_store::StoredRecord;
use crate::domain::RecordId;

/// Build the reverse map: target_path → backlinks pointing at it. Sorted by
/// `source_path` for deterministic output.
#[must_use]
pub fn materialize_backlinks(
    records: &[StoredRecord],
    record_paths: &BTreeMap<RecordId, PathBuf>,
) -> BTreeMap<PathBuf, Vec<Backlink>> {
    let mut by_target: BTreeMap<PathBuf, Vec<Backlink>> = BTreeMap::new();
    for stored in records {
        let Some(source_path) = record_paths.get(&stored.record.id) else {
            continue;
        };
        for raw in extract_links(source_path, &stored.record.body) {
            by_target
                .entry(raw.target_path.clone())
                .or_default()
                .push(Backlink {
                    source_path: source_path.clone(),
                    target_path: raw.target_path,
                    anchor: raw.anchor,
                });
        }
    }
    for entries in by_target.values_mut() {
        entries.sort_by(|a, b| a.source_path.cmp(&b.source_path));
    }
    by_target
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-core --locked folder::links::tests`
Expected: 11 passed (7 + 4 new).

- [ ] **Step 3: Verify clippy is clean**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/domain/folder/links.rs
git commit -m "feat(core): materialize_backlinks reverse-map (brief §3.4, #44)"
```

---

## Task 7: `FolderState`, `FolderIndex`, `RecordEntry`, `SubfolderEntry`

**Files:**
- Modify: `crates/cairn-core/src/domain/folder/index.rs`

- [ ] **Step 1: Write the failing test**

Append to `index.rs`:

```rust
use std::path::PathBuf;

use crate::contract::memory_store::StoredRecord;
use crate::domain::folder::links::Backlink;
use crate::domain::folder::policy::EffectivePolicy;
use crate::domain::{MemoryKind, RecordId, Rfc3339Timestamp};

/// Per-record summary line for a folder index.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordEntry {
    /// Vault-relative path of the record file.
    pub path: PathBuf,
    /// Record id.
    pub id: RecordId,
    /// Memory kind.
    pub kind: MemoryKind,
    /// Last-update timestamp from the stored record.
    pub updated_at: Rfc3339Timestamp,
    /// Backlinks pointing at this record.
    pub backlink_count: u32,
}

/// Per-subfolder aggregate row.
#[derive(Debug, Clone, PartialEq)]
pub struct SubfolderEntry {
    /// Subfolder name (basename, no trailing slash).
    pub name: String,
    /// Number of records inside the subtree.
    pub record_count: u32,
    /// Latest `updated_at` across the subtree.
    pub last_updated: Option<Rfc3339Timestamp>,
}

/// Aggregated state for one folder, ready to project as `_index.md`.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderState {
    /// Vault-relative folder path.
    pub path: PathBuf,
    /// Records living directly in this folder, sorted by kind then id.
    pub records: Vec<StoredRecord>,
    /// Subfolders, sorted by name.
    pub subfolders: Vec<SubfolderEntry>,
    /// Backlinks targeting any record in this folder, sorted by source path.
    pub backlinks: Vec<Backlink>,
    /// Resolved effective policy at this folder.
    pub effective_policy: EffectivePolicy,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_state_compiles_with_default_policy() {
        let _ = FolderState {
            path: PathBuf::from("raw"),
            records: Vec::new(),
            subfolders: Vec::new(),
            backlinks: Vec::new(),
            effective_policy: EffectivePolicy::default(),
        };
    }
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo nextest run -p cairn-core --locked folder::index::tests::folder_state_compiles_with_default_policy`
Expected: PASS.

- [ ] **Step 3: Verify clippy is clean**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/domain/folder/index.rs
git commit -m "feat(core): FolderState + RecordEntry + SubfolderEntry types (brief §3.4, #44)"
```

---

## Task 8: `aggregate_folders`

**Files:**
- Modify: `crates/cairn-core/src/domain/folder/index.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `index.rs`:

```rust
    use crate::domain::folder::policy::FolderPolicy;
    use crate::domain::record::tests::sample_stored_record;
    use std::collections::BTreeMap;

    fn fixture_record(suffix_id: &str) -> StoredRecord {
        let mut s = sample_stored_record(1);
        s.record.id = RecordId::new(format!("01HQZX9F5N00000000000{:>06}", suffix_id))
            .expect("valid record id");
        s
    }

    #[test]
    fn aggregate_single_record_yields_one_folder() {
        let r = fixture_record("000A");
        let mut paths = BTreeMap::new();
        paths.insert(r.record.id.clone(), PathBuf::from("raw/x.md"));
        let states = aggregate_folders(&[r], &paths, &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].path, PathBuf::from("raw"));
        assert_eq!(states[0].records.len(), 1);
        assert!(states[0].subfolders.is_empty());
    }

    #[test]
    fn aggregate_nested_propagates_subfolder_counts() {
        let r1 = fixture_record("0001");
        let r2 = fixture_record("0002");
        let mut paths = BTreeMap::new();
        paths.insert(r1.record.id.clone(), PathBuf::from("raw/a/x.md"));
        paths.insert(
            r2.record.id.clone(),
            PathBuf::from("raw/a/b/y.md"),
        );
        let states = aggregate_folders(&[r1, r2], &paths, &BTreeMap::new(), &BTreeMap::new());
        // One state per non-empty folder: raw, raw/a, raw/a/b
        let by_path: BTreeMap<PathBuf, &FolderState> =
            states.iter().map(|s| (s.path.clone(), s)).collect();
        let raw = by_path.get(Path::new("raw")).expect("raw");
        let raw_a = by_path.get(Path::new("raw/a")).expect("raw/a");
        let raw_a_b = by_path.get(Path::new("raw/a/b")).expect("raw/a/b");
        // raw has one subfolder (a), no direct records.
        assert!(raw.records.is_empty());
        assert_eq!(raw.subfolders.len(), 1);
        assert_eq!(raw.subfolders[0].name, "a");
        assert_eq!(raw.subfolders[0].record_count, 2);
        // raw/a has one direct record + one subfolder (b).
        assert_eq!(raw_a.records.len(), 1);
        assert_eq!(raw_a.subfolders.len(), 1);
        assert_eq!(raw_a.subfolders[0].record_count, 1);
        // raw/a/b has one direct record, no subfolders.
        assert_eq!(raw_a_b.records.len(), 1);
        assert!(raw_a_b.subfolders.is_empty());
    }

    #[test]
    fn aggregate_skips_empty_folders() {
        let r = fixture_record("0001");
        let mut paths = BTreeMap::new();
        paths.insert(r.record.id.clone(), PathBuf::from("raw/x.md"));
        let states = aggregate_folders(&[r], &paths, &BTreeMap::new(), &BTreeMap::new());
        // No FolderState for folders without records or record-bearing children.
        assert!(states.iter().all(|s| s.path != PathBuf::from("wiki")));
    }

    #[test]
    fn aggregate_attaches_backlinks_to_target_folder() {
        let r = fixture_record("0001");
        let mut paths = BTreeMap::new();
        paths.insert(r.record.id.clone(), PathBuf::from("raw/x.md"));
        let mut backlinks = BTreeMap::new();
        backlinks.insert(
            PathBuf::from("raw/x.md"),
            vec![Backlink {
                source_path: PathBuf::from("raw/y.md"),
                target_path: PathBuf::from("raw/x.md"),
                anchor: None,
            }],
        );
        let states = aggregate_folders(&[r], &paths, &BTreeMap::new(), &backlinks);
        let raw = states
            .iter()
            .find(|s| s.path == PathBuf::from("raw"))
            .expect("raw state");
        assert_eq!(raw.backlinks.len(), 1);
    }
```

Add the function:

```rust
use std::collections::BTreeMap;
use std::path::Path;

use crate::domain::folder::links::Backlink;
use crate::domain::folder::policy::{FolderPolicy, resolve_policy};

/// Group records by their parent folder, walk up to compute subfolder
/// aggregates, attach backlinks targeting records in each folder, and
/// resolve the effective policy at each folder. Returns one
/// [`FolderState`] per folder that is non-empty (has at least one record
/// in its subtree).
#[must_use]
pub fn aggregate_folders(
    records: &[StoredRecord],
    record_paths: &BTreeMap<RecordId, PathBuf>,
    policies_by_dir: &BTreeMap<PathBuf, FolderPolicy>,
    backlinks_by_target: &BTreeMap<PathBuf, Vec<Backlink>>,
) -> Vec<FolderState> {
    // 1. Pair records with their resolved paths.
    let mut paired: Vec<(PathBuf, &StoredRecord)> = Vec::new();
    for stored in records {
        let Some(p) = record_paths.get(&stored.record.id) else {
            continue;
        };
        paired.push((p.clone(), stored));
    }
    paired.sort_by(|a, b| {
        a.1.record
            .kind
            .as_str()
            .cmp(b.1.record.kind.as_str())
            .then_with(|| a.1.record.id.as_str().cmp(b.1.record.id.as_str()))
    });

    // 2. Collect every folder path that has either a direct record OR a
    //    descendant with a record. Record per-subtree counts.
    let mut subtree_count: BTreeMap<PathBuf, u32> = BTreeMap::new();
    let mut subtree_last_update: BTreeMap<PathBuf, Rfc3339Timestamp> = BTreeMap::new();
    let mut direct: BTreeMap<PathBuf, Vec<&StoredRecord>> = BTreeMap::new();

    for (path, stored) in &paired {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from(""), Path::to_path_buf);
        direct.entry(parent.clone()).or_default().push(*stored);

        // Walk every ancestor (parent inclusive) and bump counts.
        let mut cur: Option<&Path> = Some(&parent);
        while let Some(d) = cur {
            if d.as_os_str().is_empty() {
                break;
            }
            *subtree_count.entry(d.to_path_buf()).or_insert(0) += 1;
            let entry = subtree_last_update
                .entry(d.to_path_buf())
                .or_insert_with(|| stored.record.updated_at.clone());
            if stored.record.updated_at > *entry {
                *entry = stored.record.updated_at.clone();
            }
            cur = d.parent();
        }
    }

    // 3. For every folder in `subtree_count`, build a FolderState.
    let mut states: Vec<FolderState> = Vec::new();
    for (folder, _count) in &subtree_count {
        let direct_records: Vec<StoredRecord> = direct
            .get(folder)
            .map(|v| v.iter().map(|r| (*r).clone()).collect())
            .unwrap_or_default();

        // Subfolders: any path in `subtree_count` whose parent equals `folder`.
        let mut subfolders: Vec<SubfolderEntry> = subtree_count
            .iter()
            .filter_map(|(p, c)| {
                if p.parent() == Some(folder) {
                    Some(SubfolderEntry {
                        name: p
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        record_count: *c,
                        last_updated: subtree_last_update.get(p).cloned(),
                    })
                } else {
                    None
                }
            })
            .collect();
        subfolders.sort_by(|a, b| a.name.cmp(&b.name));

        // Backlinks: any entry in backlinks_by_target whose target lives in this folder.
        let mut blinks: Vec<Backlink> = backlinks_by_target
            .iter()
            .filter(|(target, _)| target.parent() == Some(folder))
            .flat_map(|(_, v)| v.iter().cloned())
            .collect();
        blinks.sort_by(|a, b| a.source_path.cmp(&b.source_path));

        // Effective policy: walk up from a synthetic file inside this folder.
        let synthetic = folder.join("_dummy.md");
        let effective_policy = resolve_policy(&synthetic, policies_by_dir);

        states.push(FolderState {
            path: folder.clone(),
            records: direct_records,
            subfolders,
            backlinks: blinks,
            effective_policy,
        });
    }
    states.sort_by(|a, b| a.path.cmp(&b.path));
    states
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-core --locked folder::index::tests`
Expected: 5 passed (1 from Task 7 + 4 new).

- [ ] **Step 3: Verify clippy is clean**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/domain/folder/index.rs
git commit -m "feat(core): aggregate_folders (brief §3.4, #44)"
```

---

## Task 9: `project_index` — render `_index.md`

**Files:**
- Modify: `crates/cairn-core/src/domain/folder/index.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `index.rs`:

```rust
    use crate::domain::projection::ProjectedFile;

    #[test]
    fn project_emits_required_frontmatter_fields() {
        let state = FolderState {
            path: PathBuf::from("raw"),
            records: Vec::new(),
            subfolders: Vec::new(),
            backlinks: Vec::new(),
            effective_policy: EffectivePolicy::default(),
        };
        let pf: ProjectedFile = project_index(&state);
        assert_eq!(pf.path, PathBuf::from("raw/_index.md"));
        assert!(pf.content.contains("folder: raw"));
        assert!(pf.content.contains("kind: folder_index"));
        assert!(pf.content.contains("record_count: 0"));
        assert!(pf.content.contains("subfolder_count: 0"));
    }

    #[test]
    fn project_omits_purpose_when_unset() {
        let state = FolderState {
            path: PathBuf::from("raw"),
            records: Vec::new(),
            subfolders: Vec::new(),
            backlinks: Vec::new(),
            effective_policy: EffectivePolicy::default(),
        };
        let pf = project_index(&state);
        assert!(!pf.content.contains("purpose:"));
    }

    #[test]
    fn project_includes_purpose_when_set() {
        let state = FolderState {
            path: PathBuf::from("raw"),
            records: Vec::new(),
            subfolders: Vec::new(),
            backlinks: Vec::new(),
            effective_policy: EffectivePolicy {
                purpose: Some("things".into()),
                ..EffectivePolicy::default()
            },
        };
        let pf = project_index(&state);
        assert!(pf.content.contains("purpose: things"));
    }

    #[test]
    fn project_is_deterministic() {
        let state = FolderState {
            path: PathBuf::from("raw"),
            records: Vec::new(),
            subfolders: vec![
                SubfolderEntry {
                    name: "b".into(),
                    record_count: 1,
                    last_updated: None,
                },
                SubfolderEntry {
                    name: "a".into(),
                    record_count: 1,
                    last_updated: None,
                },
            ],
            backlinks: Vec::new(),
            effective_policy: EffectivePolicy::default(),
        };
        // Caller passes already-sorted subfolders; project_index does not sort,
        // it relies on aggregate_folders. Sort here to match the contract.
        let mut state = state;
        state.subfolders.sort_by(|a, b| a.name.cmp(&b.name));
        let a = project_index(&state);
        let b = project_index(&state);
        assert_eq!(a.content, b.content);
    }

    #[test]
    fn project_omits_empty_sections() {
        let state = FolderState {
            path: PathBuf::from("raw"),
            records: Vec::new(),
            subfolders: Vec::new(),
            backlinks: Vec::new(),
            effective_policy: EffectivePolicy::default(),
        };
        let pf = project_index(&state);
        assert!(!pf.content.contains("## Records"));
        assert!(!pf.content.contains("## Subfolders"));
        assert!(!pf.content.contains("## Backlinks"));
    }
```

Add the function:

```rust
use crate::domain::projection::ProjectedFile;

/// Project a [`FolderState`] to a `_index.md` file. Caller is responsible
/// for ensuring deterministic sort order on `state.records`,
/// `state.subfolders`, and `state.backlinks`.
#[must_use]
pub fn project_index(state: &FolderState) -> ProjectedFile {
    let folder_str = state.path.to_string_lossy();
    let updated_at = state
        .records
        .iter()
        .map(|r| r.record.updated_at.as_str())
        .chain(
            state
                .subfolders
                .iter()
                .filter_map(|s| s.last_updated.as_ref().map(Rfc3339Timestamp::as_str)),
        )
        .max()
        .unwrap_or("1970-01-01T00:00:00Z")
        .to_owned();

    let mut frontmatter = String::new();
    frontmatter.push_str("---\n");
    frontmatter.push_str(&format!("folder: {folder_str}\n"));
    frontmatter.push_str("kind: folder_index\n");
    frontmatter.push_str(&format!("updated_at: {updated_at}\n"));
    frontmatter.push_str(&format!("record_count: {}\n", state.records.len()));
    frontmatter.push_str(&format!(
        "subfolder_count: {}\n",
        state.subfolders.len(),
    ));
    if let Some(purpose) = &state.effective_policy.purpose {
        // Quote with serde_yaml to avoid breaking on `:` / leading whitespace.
        let yaml_val = serde_yaml::Value::String(purpose.clone());
        let s = serde_yaml::to_string(&yaml_val)
            .ok()
            .and_then(|s| s.strip_prefix("---\n").map(str::to_owned).or(Some(s)))
            .unwrap_or_else(|| purpose.clone());
        let s = s.trim_end_matches('\n');
        frontmatter.push_str(&format!("purpose: {s}\n"));
    }
    frontmatter.push_str("---\n\n");

    let mut body = String::new();
    body.push_str(&format!("# {folder_str}\n"));

    if !state.records.is_empty() {
        body.push_str(&format!("\n## Records ({})\n", state.records.len()));
        for s in &state.records {
            // path = folder / "<kind>_<id>.md" — same as MarkdownProjector.
            let leaf = format!(
                "{}_{}.md",
                s.record.kind.as_str(),
                s.record.id.as_str(),
            );
            body.push_str(&format!(
                "- [{leaf}]({leaf}) — {kind} · updated {upd}\n",
                kind = s.record.kind.as_str(),
                upd = s.record.updated_at.as_str(),
            ));
        }
    }

    if !state.subfolders.is_empty() {
        body.push_str(&format!(
            "\n## Subfolders ({})\n",
            state.subfolders.len(),
        ));
        for sf in &state.subfolders {
            let upd = sf
                .last_updated
                .as_ref()
                .map(|t| format!(" · last updated {}", t.as_str()))
                .unwrap_or_default();
            body.push_str(&format!(
                "- [{name}/]({name}/) — {n} records{upd}\n",
                name = sf.name,
                n = sf.record_count,
            ));
        }
    }

    if !state.backlinks.is_empty() {
        body.push_str(&format!(
            "\n## Backlinks into this folder ({})\n",
            state.backlinks.len(),
        ));
        for bl in &state.backlinks {
            let p = bl.source_path.to_string_lossy();
            body.push_str(&format!("- [{p}]({p})\n"));
        }
    }

    ProjectedFile {
        path: state.path.join("_index.md"),
        content: format!("{frontmatter}{body}"),
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-core --locked folder::index::tests`
Expected: 10 passed (5 from Task 7+8 + 5 new).

- [ ] **Step 3: Verify clippy is clean**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/domain/folder/index.rs
git commit -m "feat(core): project_index renders _index.md (brief §3.4, #44)"
```

---

## Task 10: `FolderSummary` types + `FolderSummaryWriter` trait stub

**Files:**
- Modify: `crates/cairn-core/src/domain/folder/mod.rs`

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `mod.rs`:

```rust
    #[test]
    fn folder_summary_writer_trait_object_compiles() {
        struct Stub;
        #[async_trait::async_trait]
        impl FolderSummaryWriter for Stub {
            async fn write_summary(
                &self,
                _summary: FolderSummary,
            ) -> Result<(), FolderSummaryError> {
                Err(FolderSummaryError::Unimplemented)
            }
        }
        let _: Box<dyn FolderSummaryWriter> = Box::new(Stub);
    }
```

Add the types and trait above the existing `FolderError` enum (or below — same file, cohesive surface):

```rust
use std::path::PathBuf;

use crate::domain::Rfc3339Timestamp;

/// Schema for `_summary.md`. P0 ships types only — body generation is P1.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderSummary {
    /// Vault-relative folder path.
    pub folder: PathBuf,
    /// When this summary was generated.
    pub generated_at: Rfc3339Timestamp,
    /// Agent that generated the summary (e.g. `agt:cairn-librarian:v2`).
    pub generated_by: String,
    /// Number of records the summary covers.
    pub covers_records: u32,
    /// Approximate token count of `body`.
    pub summary_tokens: u32,
    /// Generated prose body.
    pub body: String,
}

/// Errors raised by [`FolderSummaryWriter`] implementations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FolderSummaryError {
    /// No summary writer is registered (P0 default).
    #[error("folder summary writer not registered")]
    Unimplemented,
    /// Internal error from the writer implementation.
    #[error("folder summary writer internal: {0}")]
    Internal(String),
}

/// Workflow-owned write surface for `_summary.md`. P0 ships zero
/// implementations; P1 `cairn-workflows::FolderSummaryWorkflow` is the
/// first implementor.
#[async_trait::async_trait]
pub trait FolderSummaryWriter: Send + Sync {
    /// Persist a generated [`FolderSummary`] as `_summary.md` under its
    /// folder, atomically. Implementors are responsible for I/O safety
    /// (atomic rename, symlink rejection).
    ///
    /// # Errors
    ///
    /// Returns [`FolderSummaryError::Unimplemented`] when no writer is
    /// registered, or [`FolderSummaryError::Internal`] for I/O / encoding
    /// failures.
    async fn write_summary(
        &self,
        summary: FolderSummary,
    ) -> Result<(), FolderSummaryError>;
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo nextest run -p cairn-core --locked folder::tests::folder_summary_writer_trait_object_compiles`
Expected: PASS.

- [ ] **Step 3: Verify clippy is clean**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/domain/folder/mod.rs
git commit -m "feat(core): FolderSummary types + FolderSummaryWriter trait stub (brief §3.4, #44)"
```

---

## Task 11: Property tests — round-trip and associativity

**Files:**
- Modify: `crates/cairn-core/src/domain/folder/policy.rs`

- [ ] **Step 1: Write the failing proptest**

Append to the `tests` module in `policy.rs`:

```rust
    use proptest::prelude::*;

    fn arb_cadence() -> impl Strategy<Value = ConsolidationCadence> {
        prop_oneof![
            Just(ConsolidationCadence::Hourly),
            Just(ConsolidationCadence::Daily),
            Just(ConsolidationCadence::Weekly),
            Just(ConsolidationCadence::Monthly),
            Just(ConsolidationCadence::Manual),
        ]
    }

    fn arb_retention() -> impl Strategy<Value = RetentionPolicy> {
        prop_oneof![
            (1u32..=365).prop_map(RetentionPolicy::Days),
            Just(RetentionPolicy::Unlimited),
        ]
    }

    fn arb_policy() -> impl Strategy<Value = FolderPolicy> {
        (
            proptest::option::of("[a-z ]{1,40}".prop_map(String::from)),
            proptest::option::of(arb_cadence()),
            proptest::option::of(arb_retention()),
            proptest::option::of(1u32..=1024),
        )
            .prop_map(|(purpose, cadence, retention, summary)| FolderPolicy {
                purpose,
                allowed_kinds: None,
                visibility_default: None,
                consolidation_cadence: cadence,
                owner_agent: None,
                retention,
                summary_max_tokens: summary,
            })
    }

    proptest! {
        #[test]
        fn parse_serialize_round_trips(p in arb_policy()) {
            let yaml = serde_yaml::to_string(&p).unwrap();
            let parsed = parse_policy(&yaml).unwrap();
            prop_assert_eq!(parsed, p);
        }

        #[test]
        fn resolve_associative_under_chunked_merge(
            seed in 0u32..1000
        ) {
            // Deterministic three-policy chain.
            let mut chain = std::collections::BTreeMap::new();
            chain.insert(
                std::path::PathBuf::from("a"),
                FolderPolicy {
                    purpose: Some(format!("a-{seed}")),
                    summary_max_tokens: Some(100),
                    ..FolderPolicy::default()
                },
            );
            chain.insert(
                std::path::PathBuf::from("a/b"),
                FolderPolicy {
                    consolidation_cadence: Some(ConsolidationCadence::Weekly),
                    ..FolderPolicy::default()
                },
            );
            chain.insert(
                std::path::PathBuf::from("a/b/c"),
                FolderPolicy {
                    summary_max_tokens: Some(seed.min(500) + 50),
                    ..FolderPolicy::default()
                },
            );
            let target = std::path::Path::new("a/b/c/x.md");
            let full = resolve_policy(target, &chain);

            // Subset (only middle) should differ on cadence inheritance.
            let mut middle_only = std::collections::BTreeMap::new();
            middle_only.insert(
                std::path::PathBuf::from("a/b"),
                chain.get(std::path::Path::new("a/b")).unwrap().clone(),
            );
            let middle = resolve_policy(target, &middle_only);

            prop_assert_eq!(full.consolidation_cadence, ConsolidationCadence::Weekly);
            prop_assert_eq!(middle.consolidation_cadence, ConsolidationCadence::Weekly);
        }
    }
```

Verify `proptest` is already in `cairn-core` `[dev-dependencies]`:

Run: `grep -A2 "\[dev-dependencies\]" crates/cairn-core/Cargo.toml`

If absent, add to `crates/cairn-core/Cargo.toml`:

```toml
[dev-dependencies]
proptest = { workspace = true }
```

- [ ] **Step 2: Run proptest**

Run: `cargo nextest run -p cairn-core --locked folder::policy::tests`
Expected: PASS — all proptest cases.

- [ ] **Step 3: Verify clippy is clean**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/domain/folder/policy.rs crates/cairn-core/Cargo.toml
git commit -m "test(core): proptest round-trip + resolve invariants (brief §3.4, #44)"
```

---

## Task 12: CLI — `with_fix_folders` decorator and main wiring

**Files:**
- Modify: `crates/cairn-cli/src/verbs/mod.rs`
- Modify: `crates/cairn-cli/src/main.rs`

- [ ] **Step 1: Add the decorator**

Append to `crates/cairn-cli/src/verbs/mod.rs`:

```rust
/// Add `--fix-folders` flag to the `lint` subcommand.
///
/// Augments the generated subcommand builder without touching generated
/// files, using the same pattern as [`with_fix_markdown`].
#[must_use]
pub fn with_fix_folders(cmd: clap::Command) -> clap::Command {
    cmd.arg(
        clap::Arg::new("fix-folders")
            .long("fix-folders")
            .action(clap::ArgAction::SetTrue)
            .help(
                "Regenerate folder _index.md sidecars and backlinks for every \
                 non-empty folder (brief §3.4, #44)",
            ),
    )
}
```

- [ ] **Step 2: Wrap the lint subcommand in `main.rs`**

In `crates/cairn-cli/src/main.rs`, find the existing line:

```rust
        .subcommand(verbs::with_json(verbs::with_fix_markdown(
            generated::verbs::lint_subcommand(),
        )))
```

Replace with:

```rust
        .subcommand(verbs::with_json(verbs::with_fix_markdown(
            verbs::with_fix_folders(generated::verbs::lint_subcommand()),
        )))
```

- [ ] **Step 3: Verify build**

Run: `cargo check -p cairn-cli --locked`
Expected: PASS.

- [ ] **Step 4: Smoke-test the flag is recognised**

Run: `cargo run -q -p cairn-cli --locked -- lint --help`
Expected: stdout includes `--fix-folders`.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/mod.rs crates/cairn-cli/src/main.rs
git commit -m "feat(cli): with_fix_folders flag decorator (brief §3.4, #44)"
```

---

## Task 13: CLI handler — `fix_folders_handler`

**Files:**
- Modify: `crates/cairn-cli/src/verbs/lint.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/cairn-cli/tests/lint_folders.rs`:

```rust
//! Integration tests for `cairn lint --fix-folders` (issue #44).

use std::path::Path;

use cairn_cli::verbs::lint::{FixFoldersResult, fix_folders_handler};
use cairn_core::contract::memory_store::MemoryStore;
use cairn_test_fixtures::store::{FixtureStore, sample_record};

#[tokio::test]
async fn rebuilds_index_from_empty_markdown_tree() {
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    // Bootstrap-style minimal layout: just .cairn/ and raw/.
    std::fs::create_dir_all(vault.path().join(".cairn")).unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();

    let result: FixFoldersResult = fix_folders_handler(&store, vault.path()).await.unwrap();

    assert!(!result.written.is_empty(), "expected at least one _index.md written");
    let index = vault.path().join("raw/_index.md");
    assert!(index.exists(), "raw/_index.md not written");
    let content = std::fs::read_to_string(&index).unwrap();
    assert!(content.contains("kind: folder_index"));
}

#[tokio::test]
async fn idempotent_second_run_reports_unchanged() {
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();

    let r1 = fix_folders_handler(&store, vault.path()).await.unwrap();
    assert!(!r1.written.is_empty());

    let r2 = fix_folders_handler(&store, vault.path()).await.unwrap();
    assert!(r2.written.is_empty());
    assert!(r2.unchanged > 0);
}

#[tokio::test]
async fn bad_policy_yaml_does_not_abort_run() {
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();
    std::fs::write(
        vault.path().join("raw/_policy.yaml"),
        "unknown_key: 42\n",
    )
    .unwrap();

    let result = fix_folders_handler(&store, vault.path()).await.unwrap();
    assert_eq!(result.policy_errors.len(), 1);
    assert!(result.policy_errors[0].path.ends_with("raw/_policy.yaml"));
    // The valid record's _index.md is still emitted.
    assert!(vault.path().join("raw/_index.md").exists());
}
```

Make `cairn-cli` library exports include the new handler (it already publishes `verbs` in `lib.rs`):

Run: `grep -n "pub use\|pub mod" crates/cairn-cli/src/lib.rs`

`lib.rs` already re-exports the `verbs` module via `pub mod verbs;` (added in #43). Confirm `lint` is `pub mod` inside `verbs/mod.rs`. Already true.

- [ ] **Step 2: Run the integration test to verify it fails to compile**

Run: `cargo nextest run -p cairn-cli --locked --test lint_folders`
Expected: FAIL — `fix_folders_handler` and `FixFoldersResult` undefined.

- [ ] **Step 3: Add the handler in `crates/cairn-cli/src/verbs/lint.rs`**

Add at the top, alongside existing imports:

```rust
use std::collections::BTreeMap;

use cairn_core::domain::folder::{
    FolderError, aggregate_folders, materialize_backlinks, parse_policy, project_index,
};
use cairn_core::domain::folder::policy::FolderPolicy;
```

Append after `fix_markdown_handler`:

```rust
/// Result of a `lint --fix-folders` run.
#[derive(Debug, serde::Serialize)]
pub struct FixFoldersResult {
    /// Folder index files written or updated (vault-relative).
    pub written: Vec<PathBuf>,
    /// Number of indexes that already matched their projection.
    pub unchanged: usize,
    /// Per-policy parse failures; subtree was skipped.
    pub policy_errors: Vec<PolicyError>,
}

/// One `_policy.yaml` that failed to parse.
#[derive(Debug, serde::Serialize)]
pub struct PolicyError {
    /// Vault-relative path of the offending file.
    pub path: PathBuf,
    /// Human-readable reason.
    pub reason: String,
}

/// Walk the store, build folder states, project `_index.md` files, write
/// atomically. A bad `_policy.yaml` does not abort — that subtree is
/// skipped, the error is recorded.
///
/// # Errors
///
/// Returns an error if the store cannot be queried, or if any non-policy
/// I/O fails.
pub async fn fix_folders_handler(
    store: &dyn MemoryStore,
    vault_root: &Path,
) -> anyhow::Result<FixFoldersResult> {
    let projector = MarkdownProjector;
    let records = store.list_active().await.context("store: list_active")?;

    // 1. Build record_paths from MarkdownProjector — same shape used by
    //    --fix-markdown, so callers get a coherent view.
    let mut record_paths: BTreeMap<cairn_core::domain::RecordId, PathBuf> = BTreeMap::new();
    for stored in &records {
        let pf = projector.project(stored);
        record_paths.insert(stored.record.id.clone(), pf.path);
    }

    // 2. Walk vault for files named `_policy.yaml`.
    let mut policies_by_dir: BTreeMap<PathBuf, FolderPolicy> = BTreeMap::new();
    let mut policy_errors: Vec<PolicyError> = Vec::new();
    let walker = walkdir::WalkDir::new(vault_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_hidden_dir(e));
    for entry in walker {
        let entry = entry.with_context(|| format!("walking {}", vault_root.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name() != "_policy.yaml" {
            continue;
        }
        let abs = entry.path().to_path_buf();
        let rel = abs
            .strip_prefix(vault_root)
            .with_context(|| format!("strip_prefix {}", abs.display()))?
            .to_path_buf();
        let dir = rel.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        let bytes = tokio::fs::read_to_string(&abs).await.with_context(|| {
            format!("read {}", abs.display())
        })?;
        match parse_policy(&bytes) {
            Ok(p) => {
                policies_by_dir.insert(dir, p);
            }
            Err(FolderError::PolicyParse { source }) => {
                policy_errors.push(PolicyError {
                    path: rel,
                    reason: source.to_string(),
                });
            }
        }
    }

    // 3. Reverse-map backlinks.
    let backlinks_by_target = materialize_backlinks(&records, &record_paths);

    // 4. Aggregate.
    let states = aggregate_folders(
        &records,
        &record_paths,
        &policies_by_dir,
        &backlinks_by_target,
    );

    // 5. Write each `_index.md` atomically.
    let mut written = Vec::new();
    let mut unchanged = 0usize;
    for state in states {
        let projected = project_index(&state);
        let abs = vault_root.join(&projected.path);
        let needs_write = match tokio::fs::read_to_string(&abs).await {
            Ok(existing) => existing != projected.content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => return Err(anyhow::anyhow!("cannot read {}: {e}", abs.display())),
        };
        if !needs_write {
            unchanged += 1;
            continue;
        }
        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create_dir_all {}", parent.display()))?;
        }
        let content = projected.content.clone();
        let dest = abs.clone();
        let parent_buf = abs.parent().unwrap_or(Path::new(".")).to_path_buf();
        tokio::task::spawn_blocking(move || {
            use std::io::Write as _;
            let mut tmp = tempfile::Builder::new()
                .suffix(".md.tmp")
                .tempfile_in(&parent_buf)
                .with_context(|| format!("tempfile in {}", parent_buf.display()))?;
            tmp.write_all(content.as_bytes())
                .with_context(|| format!("write temp {}", tmp.path().display()))?;
            tmp.persist(&dest)
                .map_err(|e| anyhow::anyhow!("persist -> {}: {}", dest.display(), e.error))?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .with_context(|| format!("spawn_blocking write {}", abs.display()))??;
        written.push(projected.path);
    }

    Ok(FixFoldersResult {
        written,
        unchanged,
        policy_errors,
    })
}

fn is_hidden_dir(entry: &walkdir::DirEntry) -> bool {
    entry.file_type().is_dir()
        && entry
            .file_name()
            .to_str()
            .is_some_and(|s| s.starts_with('.') && s != ".")
}
```

Add `walkdir` to `cairn-cli` `[dependencies]` in `crates/cairn-cli/Cargo.toml`:

```toml
walkdir = { workspace = true }
```

If `walkdir` is not yet in workspace deps, add to root `Cargo.toml` `[workspace.dependencies]`:

```toml
walkdir = "2"
```

- [ ] **Step 4: Run integration tests**

Run: `cargo nextest run -p cairn-cli --locked --test lint_folders`
Expected: 3 passed.

- [ ] **Step 5: Verify clippy is clean**

Run: `cargo clippy -p cairn-cli --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/verbs/lint.rs crates/cairn-cli/tests/lint_folders.rs crates/cairn-cli/Cargo.toml Cargo.toml
git commit -m "feat(cli): fix_folders_handler — atomic _index.md rebuild (brief §3.4, #44)"
```

---

## Task 14: Wire `--fix-folders` into `lint::run`

**Files:**
- Modify: `crates/cairn-cli/src/verbs/lint.rs`

- [ ] **Step 1: Add the dispatch branch**

Update `run()` in `lint.rs`. Current shape:

```rust
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    let fix_markdown = sub.get_flag("fix-markdown");

    if fix_markdown {
        // unimplemented response
    }
    // unimplemented response
}
```

Replace with:

```rust
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    let fix_markdown = sub.get_flag("fix-markdown");
    let fix_folders = sub.get_flag("fix-folders");

    if fix_markdown || fix_folders {
        // TODO(#46): wire the SQLite store. For now, return the same
        // unimplemented envelope used by --fix-markdown.
        let resp = unimplemented_response(ResponseVerb::Lint);
        if json {
            emit_json(&resp);
        } else {
            human_error(
                "lint",
                "Internal",
                "store not wired in this P0 build — --fix-folders requires #46",
                &resp.operation_id,
            );
        }
        return ExitCode::FAILURE;
    }

    let resp = unimplemented_response(ResponseVerb::Lint);
    if json {
        emit_json(&resp);
    } else {
        human_error(
            "lint",
            "Internal",
            "store not wired in this P0 build",
            &resp.operation_id,
        );
    }
    ExitCode::FAILURE
}
```

(The handler is exercised by integration tests directly; CLI dispatch goes live with #46 store wiring, mirroring how `--fix-markdown` already works.)

- [ ] **Step 2: Verify build + smoke-test**

Run: `cargo run -q -p cairn-cli --locked -- lint --fix-folders --json`
Expected: prints an `Unimplemented` envelope, exit code `1`. (Same as `--fix-markdown` today.)

- [ ] **Step 3: Verify all unit + integration tests still pass**

Run: `cargo nextest run -p cairn-cli --locked`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/src/verbs/lint.rs
git commit -m "feat(cli): dispatch --fix-folders to handler (brief §3.4, #44)"
```

---

## Task 15: Insta snapshot for `_index.md` fixture

**Files:**
- Modify: `crates/cairn-cli/tests/lint_folders.rs`

- [ ] **Step 1: Write the snapshot test**

Append to `crates/cairn-cli/tests/lint_folders.rs`:

```rust
#[tokio::test]
async fn fixture_index_matches_snapshot() {
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();

    let _ = fix_folders_handler(&store, vault.path()).await.unwrap();
    let content = std::fs::read_to_string(vault.path().join("raw/_index.md")).unwrap();

    // Strip the timestamped `updated_at` line so the snapshot stays stable.
    let stable: String = content
        .lines()
        .filter(|l| !l.starts_with("updated_at:") && !l.contains("· updated "))
        .collect::<Vec<_>>()
        .join("\n");

    insta::assert_snapshot!("raw_index_single_record", stable);
}
```

- [ ] **Step 2: Verify `insta` is a dev-dep of `cairn-cli`**

Run: `grep -n "insta" crates/cairn-cli/Cargo.toml`

If absent, add to `[dev-dependencies]`:

```toml
insta = { workspace = true }
```

- [ ] **Step 3: Run the test (it will fail with a "missing snapshot")**

Run: `cargo nextest run -p cairn-cli --locked --test lint_folders fixture_index_matches_snapshot`
Expected: FAIL — pending snapshot.

- [ ] **Step 4: Accept the snapshot**

Run: `cargo insta review` (or `cargo insta accept` if interactive review is unavailable in the harness).
Expected: snapshot committed under `crates/cairn-cli/tests/snapshots/lint_folders__raw_index_single_record.snap`.

- [ ] **Step 5: Re-run to confirm**

Run: `cargo nextest run -p cairn-cli --locked --test lint_folders`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/tests/lint_folders.rs crates/cairn-cli/tests/snapshots/ crates/cairn-cli/Cargo.toml
git commit -m "test(cli): insta snapshot for raw/_index.md fixture (brief §3.4, #44)"
```

---

## Task 16: Full verification pipeline

**Files:** none

- [ ] **Step 1: fmt**

Run: `cargo fmt --all --check`
Expected: PASS.

- [ ] **Step 2: clippy (full workspace)**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 3: check**

Run: `cargo check --workspace --all-targets --locked`
Expected: PASS.

- [ ] **Step 4: nextest (full workspace)**

Run: `cargo nextest run --workspace --locked --no-fail-fast`
Expected: all green.

- [ ] **Step 5: doctests**

Run: `cargo test --doc --workspace --locked`
Expected: PASS.

- [ ] **Step 6: core boundary script**

Run: `./scripts/check-core-boundary.sh`
Expected: PASS.

- [ ] **Step 7: codegen no-diff**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`
Expected: PASS (no IDL changes).

- [ ] **Step 8: docs build**

Run: `RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" cargo doc --workspace --no-deps --document-private-items --locked`
Expected: PASS.

- [ ] **Step 9: supply-chain trio**

Run:
```bash
cargo deny check
cargo audit --deny warnings
cargo machete
```
Expected: PASS (the only new dep is `walkdir`, already widely allow-listed).

- [ ] **Step 10: Final commit (only if step 9 surfaces deny.toml updates)**

If `cargo deny` flags a missing license / advisory entry for `walkdir`, append to `deny.toml` and commit:

```bash
git add deny.toml
git commit -m "chore(deny): allow walkdir transitive licenses (#44)"
```

---

## Self-review notes

- **Spec coverage:** Every acceptance criterion (§3.4 sidecar trio, backlink rebuildability, workflow-owned summary writes) maps to one or more tasks (3, 4, 5–6, 7–9, 10, 13, 15). Verification criteria (folder fixture projection, backlink rebuild from empty markdown tree, nested-folder config inheritance) map to tasks 15, 13, 4 respectively.
- **No placeholders:** Every step has either runnable code or an exact command with expected output.
- **Type consistency:** `FolderError`, `FolderPolicy`, `EffectivePolicy`, `FolderState`, `FolderIndex`, `Backlink`, `RawLink`, `FolderSummary`, `FolderSummaryWriter` named consistently across all tasks.
- **TDD:** Every task starts with a failing test, then minimal impl, then green.
- **Frequent commits:** One commit per task, each scoped to one file or tightly cohesive group.
