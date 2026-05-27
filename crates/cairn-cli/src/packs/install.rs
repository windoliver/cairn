//! Pack installer: write a validated `PackManifest`'s files into a target
//! project directory.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::packs::manifest::{Harness, PackError, PackManifest};

/// Install options.
#[derive(Debug, Clone)]
pub struct PackInstallOpts {
    /// Harness to install for.
    pub harness: Harness,
    /// Target project directory (will be created if needed).
    pub project_dir: PathBuf,
    /// Overwrite existing files even when they differ.
    pub force: bool,
}

/// Receipt returned by [`install_pack`].
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PackInstallReceipt {
    /// Pack id installed.
    pub pack_id: String,
    /// Pack version installed.
    pub version: String,
    /// Files created (did not exist before).
    pub files_created: Vec<PathBuf>,
    /// Files merged with existing content.
    pub files_merged: Vec<PathBuf>,
    /// Files skipped (content matched, or skip-due-to-existing without force).
    pub files_skipped: Vec<PathBuf>,
    /// Non-fatal warnings (e.g. missing capabilities).
    pub warnings: Vec<String>,
    /// True if any `requires_capabilities` entry is not advertised locally.
    pub degraded: bool,
}

/// Install the pack for `opts.harness` into `opts.project_dir`.
///
/// # Errors
/// Returns [`PackError`] on validation failure, IO failure, or
/// non-recoverable merge conflict.
pub fn install_pack(opts: &PackInstallOpts) -> Result<PackInstallReceipt, PackError> {
    let dir = crate::packs::bundled_pack_for(opts.harness);

    let manifest_bytes = dir
        .get_file("pack.json")
        .ok_or_else(|| PackError::ManifestInvalid {
            reason: "embedded pack missing pack.json".to_owned(),
        })?
        .contents();
    let manifest: PackManifest = serde_json::from_slice(manifest_bytes)?;

    manifest.validate_pass_a()?;
    manifest.validate_pass_b()?;
    manifest.assert_all_paths_present(dir)?;
    manifest.assert_subagent_frontmatter_matches_manifest(dir)?;

    if manifest.harness != opts.harness {
        // Both sides typed as Harness enum; this branch is unreachable in v1.
        return Err(PackError::HarnessMismatch {
            want: format!("{:?}", manifest.harness),
            got: format!("{:?}", opts.harness),
        });
    }

    let mut receipt = PackInstallReceipt {
        pack_id: manifest.pack_id.clone(),
        version: manifest.version.clone(),
        ..Default::default()
    };

    // 0. Migrate legacy pre-pack slash commands (issue #182).
    //    Prior cairn-cli versions inlined four commands —
    //    remember.md / forget.md / recall.md / graph.md — wrapped with
    //    `<!-- BEGIN CAIRN AGENT SKILL -->`. The pack supersedes them
    //    with `cairn-*` verb-direct commands; the legacy ones (notably
    //    `forget.md`, which committed without --dry-run) MUST be
    //    removed on upgrade so the safer new path is canonical.
    remove_legacy_commands(&opts.project_dir, &mut receipt)?;

    // 1. Subagents → .claude/agents/<id>.md
    for s in &manifest.subagents {
        let bytes = dir.get_file(&s.path).unwrap().contents();
        let target = opts
            .project_dir
            .join(".claude/agents")
            .join(format!("{}.md", s.id));
        write_pack_file(&opts.project_dir, &target, bytes, opts.force, &mut receipt)?;
    }

    // 2. Commands → .claude/commands/<id>.md
    for c in &manifest.commands {
        let bytes = dir.get_file(&c.path).unwrap().contents();
        let target = opts
            .project_dir
            .join(".claude/commands")
            .join(format!("{}.md", c.id));
        write_pack_file(&opts.project_dir, &target, bytes, opts.force, &mut receipt)?;
    }

    // 3. hooks/settings.json → .claude/settings.json (merged).
    let pack_id_at_version = format!("{}@{}", manifest.pack_id, manifest.version);
    let pack_settings_bytes = dir
        .get_file("hooks/settings.json")
        .ok_or_else(|| PackError::ManifestInvalid {
            reason: "embedded pack missing hooks/settings.json".to_owned(),
        })?
        .contents();
    let pack_settings: Value = serde_json::from_slice(pack_settings_bytes)?;
    let settings_target = opts.project_dir.join(".claude/settings.json");
    let existing_settings = read_optional_json(&settings_target)?;
    let merged_settings = crate::packs::merge::merge_settings_json(
        existing_settings,
        &pack_settings,
        &pack_id_at_version,
    )?;
    write_json_pretty(
        &opts.project_dir,
        &settings_target,
        &merged_settings,
        &mut receipt,
    )?;

    // 4. hooks/.mcp.json → project .mcp.json (deep merge of mcpServers).
    if let Some(mcp_file) = dir.get_file("hooks/.mcp.json") {
        let pack_mcp: Value = serde_json::from_slice(mcp_file.contents())?;
        let mcp_target = opts.project_dir.join(".mcp.json");
        let existing_mcp = read_optional_json(&mcp_target)?;
        let merged_mcp = merge_mcp_json(
            existing_mcp,
            &pack_mcp,
            &pack_id_at_version,
            opts.force,
            &mut receipt.warnings,
        )?;
        write_json_pretty(&opts.project_dir, &mcp_target, &merged_mcp, &mut receipt)?;
    }

    // 5. manual.md → CLAUDE.md (block-injected).
    let manual_bytes = dir.get_file(&manifest.manual_fragment).unwrap().contents();
    let manual_text =
        std::str::from_utf8(manual_bytes).map_err(|e| PackError::ManifestInvalid {
            reason: format!("manual_fragment is not UTF-8: {e}"),
        })?;
    let claude_md_target = opts.project_dir.join("CLAUDE.md");
    let existing_claude = read_optional_text(&claude_md_target)?;
    let injected = crate::packs::merge::inject_block(existing_claude, manual_text)?;
    write_text(
        &opts.project_dir,
        &claude_md_target,
        &injected,
        &mut receipt,
    )?;

    // 6. Capability advertise — soft check via canonical Capabilities enum.
    //    A pack capability is "known" iff serde can deserialize it as the
    //    canonical enum. Pass B already enforced this; here we use the
    //    same predicate to surface a `degraded` flag if any required
    //    capability is somehow unrecognised at install time.
    for cap in &manifest.requires_capabilities {
        let json = format!("\"{cap}\"");
        if serde_json::from_str::<cairn_core::generated::common::Capabilities>(&json).is_err() {
            receipt.warnings.push(format!(
                "capability `{cap}` not advertised — install proceeds, runtime will fail closed"
            ));
            receipt.degraded = true;
        }
    }

    Ok(receipt)
}

/// Legacy slash command file names emitted by the pre-pack inline
/// installer. Removed on upgrade if the file body carries the old
/// `BEGIN CAIRN AGENT SKILL` marker.
const LEGACY_COMMAND_NAMES: &[&str] = &["remember.md", "forget.md", "recall.md", "graph.md"];

/// Marker that identifies a file as written by the pre-pack inline
/// installer (see `cairn-cli/src/skill.rs` on main prior to #182).
const LEGACY_PACK_MARKER: &str = "BEGIN CAIRN AGENT SKILL";

fn remove_legacy_commands(
    project_dir: &Path,
    receipt: &mut PackInstallReceipt,
) -> Result<(), PackError> {
    let commands_dir = project_dir.join(".claude/commands");
    for name in LEGACY_COMMAND_NAMES {
        let target = commands_dir.join(name);
        if !target.exists() {
            continue;
        }
        // Safety: only remove if the file carries the legacy marker.
        // A user-authored file with the same name is preserved.
        let body = std::fs::read_to_string(&target).unwrap_or_default();
        if !body.contains(LEGACY_PACK_MARKER) {
            receipt.warnings.push(format!(
                "preserved user-modified legacy command `{}`; manual cleanup recommended",
                target.display()
            ));
            continue;
        }
        std::fs::remove_file(&target)?;
        receipt.warnings.push(format!(
            "removed legacy pre-pack command `{}` (superseded by cairn-* commands)",
            target.display()
        ));
    }
    Ok(())
}

/// Substrings the installer treats as pack-owned markers on re-install.
/// Any `.md` file containing one of these is treated as a previous
/// version's emission and upgraded in place (no `--force` needed).
///
/// - `"BEGIN CAIRN PACK"` matches slash-command bodies and the
///   CLAUDE.md manual fragment (both wrap content with
///   `<!-- BEGIN CAIRN PACK ... -->`).
/// - `"@pack: cairn-claude-code"` matches subagent files, which use
///   a comment-based marker directly after the frontmatter.
const PACK_OWNED_MARKERS: &[&str] = &["BEGIN CAIRN PACK", "@pack: cairn-claude-code"];

fn is_pack_owned(content: &str) -> bool {
    PACK_OWNED_MARKERS
        .iter()
        .any(|marker| content.contains(marker))
}

fn write_pack_file(
    project_dir: &Path,
    target: &Path,
    bytes: &[u8],
    force: bool,
    receipt: &mut PackInstallReceipt,
) -> Result<(), PackError> {
    reject_symlink(project_dir, target)?;
    ensure_parent(target)?;
    if target.exists() {
        let existing = std::fs::read(target)?;
        if existing == bytes {
            receipt.files_skipped.push(target.to_path_buf());
            return Ok(());
        }
        // Detect pack-owned files by their guarded marker (subagents
        // and slash commands both wrap their bodies in
        // `<!-- BEGIN CAIRN PACK ... -->`). A stale pack-owned file
        // from a previous version must be upgraded without --force —
        // otherwise safety fixes in later releases never reach
        // existing installs.
        let existing_str = std::str::from_utf8(&existing).unwrap_or("");
        let pack_owned = is_pack_owned(existing_str);
        if !force && !pack_owned {
            // User-modified file with no pack marker. Preserve, warn.
            receipt.warnings.push(format!(
                "preserved user-modified `{}`; pass --force to overwrite",
                target.display()
            ));
            receipt.files_skipped.push(target.to_path_buf());
            return Ok(());
        }
        std::fs::write(target, bytes)?;
        receipt.files_merged.push(target.to_path_buf());
    } else {
        std::fs::write(target, bytes)?;
        receipt.files_created.push(target.to_path_buf());
    }
    Ok(())
}

fn write_json_pretty(
    project_dir: &Path,
    target: &Path,
    value: &Value,
    receipt: &mut PackInstallReceipt,
) -> Result<(), PackError> {
    reject_symlink(project_dir, target)?;
    ensure_parent(target)?;
    let pretty = format!("{}\n", serde_json::to_string_pretty(value)?);
    let bytes = pretty.as_bytes();
    if target.exists() {
        let existing = std::fs::read(target)?;
        if existing == bytes {
            receipt.files_skipped.push(target.to_path_buf());
            return Ok(());
        }
        std::fs::write(target, bytes)?;
        receipt.files_merged.push(target.to_path_buf());
    } else {
        std::fs::write(target, bytes)?;
        receipt.files_created.push(target.to_path_buf());
    }
    Ok(())
}

fn write_text(
    project_dir: &Path,
    target: &Path,
    text: &str,
    receipt: &mut PackInstallReceipt,
) -> Result<(), PackError> {
    reject_symlink(project_dir, target)?;
    ensure_parent(target)?;
    let bytes = text.as_bytes();
    if target.exists() {
        let existing = std::fs::read(target)?;
        if existing == bytes {
            receipt.files_skipped.push(target.to_path_buf());
            return Ok(());
        }
        std::fs::write(target, bytes)?;
        receipt.files_merged.push(target.to_path_buf());
    } else {
        std::fs::write(target, bytes)?;
        receipt.files_created.push(target.to_path_buf());
    }
    Ok(())
}

fn ensure_parent(target: &Path) -> Result<(), PackError> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Refuse to write through a symlink at the leaf target path OR any
/// parent component between `project_dir` and the leaf.
///
/// Pack install must never overwrite a file outside the project
/// directory by following a symlink — either at the destination
/// itself, or at any parent like `.claude/` or `.claude/commands/`.
/// We walk every component from the leaf back up to (but not
/// including) `project_dir` and reject if any of them is a symlink
/// per `symlink_metadata` (does NOT follow symlinks).
fn reject_symlink(project_dir: &Path, target: &Path) -> Result<(), PackError> {
    let mut current = target;
    loop {
        match std::fs::symlink_metadata(current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(PackError::MergeConflict {
                    file: current.display().to_string(),
                    reason: "refusing to write through symlink in install path; \
                             remove the link first or pick a different project dir"
                        .to_owned(),
                });
            }
            Ok(_) | Err(_) => {}
        }
        if current == project_dir {
            break;
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if parent.as_os_str().is_empty() {
            break;
        }
        current = parent;
    }
    Ok(())
}

fn read_optional_json(path: &Path) -> Result<Value, PackError> {
    if path.exists() {
        let bytes = std::fs::read(path)?;
        let value: Value = serde_json::from_slice(&bytes)?;
        Ok(value)
    } else {
        Ok(Value::Null)
    }
}

fn read_optional_text(path: &Path) -> Result<Option<String>, PackError> {
    if path.exists() {
        Ok(Some(std::fs::read_to_string(path)?))
    } else {
        Ok(None)
    }
}

fn merge_mcp_json(
    existing: Value,
    pack: &Value,
    pack_id_at_version: &str,
    force: bool,
    warnings: &mut Vec<String>,
) -> Result<Value, PackError> {
    let mut out = match existing {
        Value::Null => serde_json::json!({}),
        Value::Object(_) => existing,
        other => {
            return Err(PackError::MergeConflict {
                file: ".mcp.json".to_owned(),
                reason: format!("expected object, got {other:?}"),
            });
        }
    };

    let pack_servers = pack
        .get("mcpServers")
        .and_then(Value::as_object)
        .ok_or_else(|| PackError::MergeConflict {
            file: ".mcp.json".to_owned(),
            reason: "pack payload missing `mcpServers`".to_owned(),
        })?;

    let pack_id = pack_id_at_version
        .split_once('@')
        .map_or(pack_id_at_version, |(id, _)| id);

    let out_servers = out
        .as_object_mut()
        .expect("out is object")
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(serde_json::Map::default()));
    let out_servers = out_servers
        .as_object_mut()
        .ok_or_else(|| PackError::MergeConflict {
            file: ".mcp.json".to_owned(),
            reason: "existing `mcpServers` is not an object".to_owned(),
        })?;
    for (name, server) in pack_servers {
        if let Some(existing_entry) = out_servers.get(name) {
            // Ownership check: only replace entries this pack already
            // wrote. User-authored entries (no `_pack`) or entries from a
            // different pack are preserved unless `force=true`.
            let owned_by_us = crate::packs::merge::marker_matches_pack(existing_entry, pack_id);
            if !owned_by_us && !force {
                warnings.push(format!(
                    "preserved existing `mcpServers.{name}` entry not owned by pack `{pack_id}`; \
                     pass --force to replace"
                ));
                continue;
            }
        }
        let mut tagged = server.clone();
        if let Some(obj) = tagged.as_object_mut() {
            obj.insert(
                "_pack".to_owned(),
                Value::String(pack_id_at_version.to_owned()),
            );
        }
        out_servers.insert(name.clone(), tagged);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn opts(dir: &Path) -> PackInstallOpts {
        PackInstallOpts {
            harness: Harness::ClaudeCode,
            project_dir: dir.to_path_buf(),
            force: false,
        }
    }

    #[test]
    fn install_into_empty_dir_creates_expected_files() {
        let tmp = tempdir().unwrap();
        let receipt = install_pack(&opts(tmp.path())).expect("install ok");
        assert_eq!(receipt.pack_id, "cairn-claude-code");
        assert!(tmp.path().join(".claude/agents/context-loader.md").exists());
        assert!(tmp.path().join(".claude/commands/cairn-ingest.md").exists());
        assert!(tmp.path().join(".claude/settings.json").exists());
        assert!(tmp.path().join(".mcp.json").exists());
        assert!(tmp.path().join("CLAUDE.md").exists());
        assert!(!receipt.files_created.is_empty());
    }

    #[test]
    fn install_is_idempotent() {
        let tmp = tempdir().unwrap();
        let first = install_pack(&opts(tmp.path())).unwrap();
        let second = install_pack(&opts(tmp.path())).unwrap();
        assert!(!first.files_created.is_empty());
        assert!(
            second.files_created.is_empty(),
            "second run creates nothing"
        );
        // Every file in the second run should be in skipped (already
        // matching) — no merges either.
        assert!(second.files_merged.is_empty(), "second run merges nothing");
    }

    #[test]
    fn install_removes_legacy_pre_pack_slash_commands() {
        // Seed a tempdir with the legacy pre-pack inline output: a
        // `forget.md` that ran `cairn forget --record` WITHOUT
        // --dry-run. Install must remove it so users can't reach the
        // destructive shortcut.
        let tmp = tempdir().unwrap();
        let legacy_forget = r#"---
description: Forget a Cairn record after explicit confirmation
argument-hint: <record-id>
---

<!-- BEGIN CAIRN AGENT SKILL -->
Confirm the user wants to forget the named Cairn record, then run:

`cairn forget --record "$ARGUMENTS"`
<!-- END CAIRN AGENT SKILL -->
"#;
        let cmd_dir = tmp.path().join(".claude/commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(cmd_dir.join("forget.md"), legacy_forget).unwrap();
        std::fs::write(
            cmd_dir.join("remember.md"),
            "<!-- BEGIN CAIRN AGENT SKILL -->\nold\n",
        )
        .unwrap();
        // Plus a USER-AUTHORED file at one of the legacy names with NO
        // legacy marker — must be preserved.
        std::fs::write(
            cmd_dir.join("recall.md"),
            "# my custom recall\n\nuser content\n",
        )
        .unwrap();

        let receipt = install_pack(&opts(tmp.path())).expect("install ok");

        assert!(
            !cmd_dir.join("forget.md").exists(),
            "legacy forget.md must be removed"
        );
        assert!(
            !cmd_dir.join("remember.md").exists(),
            "legacy remember.md must be removed"
        );
        assert!(
            cmd_dir.join("recall.md").exists(),
            "user-authored recall.md must be preserved"
        );
        assert!(
            receipt
                .warnings
                .iter()
                .any(|w| w.contains("legacy pre-pack command") && w.contains("forget.md")),
            "warning must mention removed forget.md; got {:?}",
            receipt.warnings
        );
    }

    #[test]
    fn install_upgrades_stale_pack_owned_slash_command() {
        // Simulate a previous-version pack file: same name + path, but
        // stale content carrying the pack-owned marker. A fresh install
        // (no --force) MUST upgrade it so safety fixes propagate.
        let tmp = tempdir().unwrap();
        let stale = "---\ndescription: stale\n---\n\n<!-- BEGIN CAIRN PACK -->\nold body\n<!-- END CAIRN PACK -->\n";
        let cmd = tmp.path().join(".claude/commands/cairn-ingest.md");
        std::fs::create_dir_all(cmd.parent().unwrap()).unwrap();
        std::fs::write(&cmd, stale).unwrap();

        let receipt = install_pack(&opts(tmp.path())).expect("install ok");
        let after = std::fs::read_to_string(&cmd).unwrap();
        assert_ne!(after, stale, "pack-owned file must upgrade on reinstall");
        assert!(
            after.contains("cairn ingest"),
            "upgraded file must contain pack's body; got {after}"
        );
        assert!(
            receipt
                .files_merged
                .iter()
                .any(|p| p.ends_with("cairn-ingest.md")),
            "files_merged must include the upgraded command; got {:?}",
            receipt.files_merged
        );
    }

    #[test]
    fn install_preserves_user_modified_slash_command() {
        // User overwrote a slash command and removed the pack marker.
        // Without --force, the user content must be preserved + warned.
        let tmp = tempdir().unwrap();
        let user = "# my custom ingest helper\n\nNot the pack version.\n";
        let cmd = tmp.path().join(".claude/commands/cairn-ingest.md");
        std::fs::create_dir_all(cmd.parent().unwrap()).unwrap();
        std::fs::write(&cmd, user).unwrap();

        let receipt = install_pack(&opts(tmp.path())).expect("install ok");
        let after = std::fs::read_to_string(&cmd).unwrap();
        assert_eq!(after, user, "user-modified file must be preserved");
        assert!(
            receipt
                .warnings
                .iter()
                .any(|w| w.contains("cairn-ingest.md")),
            "warning must mention the preserved file; got {:?}",
            receipt.warnings
        );
    }

    #[test]
    fn install_preserves_user_owned_mcp_server_entry() {
        // A project already has its own mcpServers.cairn pointing at a
        // custom binary with a tenant-specific arg. Install must NOT
        // overwrite it without --force.
        let tmp = tempdir().unwrap();
        let user_mcp = serde_json::json!({
            "mcpServers": {
                "cairn": {
                    "command": "/opt/custom/cairn",
                    "args": ["mcp", "--vault", "/data/tenant-a"]
                }
            }
        });
        std::fs::write(
            tmp.path().join(".mcp.json"),
            format!("{}\n", serde_json::to_string_pretty(&user_mcp).unwrap()),
        )
        .unwrap();

        let receipt = install_pack(&opts(tmp.path())).expect("install ok");

        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join(".mcp.json")).unwrap())
                .unwrap();
        assert_eq!(
            stored["mcpServers"]["cairn"]["command"], "/opt/custom/cairn",
            "user-owned cairn server must be preserved"
        );
        assert_eq!(
            stored["mcpServers"]["cairn"]["args"][1], "--vault",
            "user-owned args must be preserved"
        );
        assert!(
            receipt
                .warnings
                .iter()
                .any(|w| w.contains("mcpServers.cairn")),
            "warning must mention the preserved entry; got {:?}",
            receipt.warnings
        );
    }

    #[test]
    fn install_force_overwrites_user_owned_mcp_server_entry() {
        // With --force, the user-owned entry IS overwritten by pack's.
        let tmp = tempdir().unwrap();
        let user_mcp = serde_json::json!({
            "mcpServers": {
                "cairn": {
                    "command": "/opt/custom/cairn",
                    "args": ["mcp", "--vault", "/data/tenant-a"]
                }
            }
        });
        std::fs::write(
            tmp.path().join(".mcp.json"),
            format!("{}\n", serde_json::to_string_pretty(&user_mcp).unwrap()),
        )
        .unwrap();

        install_pack(&PackInstallOpts {
            harness: Harness::ClaudeCode,
            project_dir: tmp.path().to_path_buf(),
            force: true,
        })
        .expect("install ok");

        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join(".mcp.json")).unwrap())
                .unwrap();
        assert_eq!(stored["mcpServers"]["cairn"]["command"], "cairn");
        assert_eq!(stored["mcpServers"]["cairn"]["args"][0], "mcp");
    }

    #[test]
    fn install_preserves_user_claude_md_content() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "# Project\n\nuser content\n").unwrap();
        install_pack(&opts(tmp.path())).unwrap();
        let body = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        assert!(body.contains("# Project"));
        assert!(body.contains("user content"));
        assert!(body.contains("Cairn (Claude Code reference pack)"));
    }

    #[cfg(unix)]
    #[test]
    fn install_refuses_symlinked_target() {
        let tmp = tempdir().unwrap();
        let outside = tmp.path().join("outside-target");
        std::fs::write(&outside, b"untouched\n").unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::os::unix::fs::symlink(&outside, project.join("CLAUDE.md"))
            .expect("symlink test setup");

        let err = install_pack(&PackInstallOpts {
            harness: Harness::ClaudeCode,
            project_dir: project.clone(),
            force: true,
        })
        .expect_err("install should refuse symlinked CLAUDE.md");
        assert!(
            matches!(err, PackError::MergeConflict { .. }),
            "got {err:?}"
        );
        // The symlink target must remain unchanged.
        let unchanged = std::fs::read_to_string(&outside).unwrap();
        assert_eq!(unchanged, "untouched\n");
    }

    #[cfg(unix)]
    #[test]
    fn install_refuses_symlinked_parent_dir() {
        // Attacker plants `.claude` as a symlink to an outside directory;
        // pack install must abort instead of dropping subagent .md files
        // into the symlink target.
        let tmp = tempdir().unwrap();
        let outside_dir = tmp.path().join("attacker-stash");
        std::fs::create_dir_all(&outside_dir).unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::os::unix::fs::symlink(&outside_dir, project.join(".claude"))
            .expect("symlink test setup");

        let err = install_pack(&PackInstallOpts {
            harness: Harness::ClaudeCode,
            project_dir: project.clone(),
            force: true,
        })
        .expect_err("install should refuse symlinked .claude parent");
        assert!(
            matches!(err, PackError::MergeConflict { .. }),
            "got {err:?}"
        );
        // No agent .md files should have leaked into the symlink target.
        let leaked: Vec<_> = std::fs::read_dir(&outside_dir)
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(leaked.is_empty(), "files leaked into {outside_dir:?}");
    }
}
