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

    // 1. Subagents → .claude/agents/<id>.md
    for s in &manifest.subagents {
        let bytes = dir.get_file(&s.path).unwrap().contents();
        let target = opts
            .project_dir
            .join(".claude/agents")
            .join(format!("{}.md", s.id));
        write_pack_file(&target, bytes, opts.force, &mut receipt)?;
    }

    // 2. Commands → .claude/commands/<id>.md
    for c in &manifest.commands {
        let bytes = dir.get_file(&c.path).unwrap().contents();
        let target = opts
            .project_dir
            .join(".claude/commands")
            .join(format!("{}.md", c.id));
        write_pack_file(&target, bytes, opts.force, &mut receipt)?;
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
    write_json_pretty(&settings_target, &merged_settings, &mut receipt)?;

    // 4. hooks/.mcp.json → project .mcp.json (deep merge of mcpServers).
    if let Some(mcp_file) = dir.get_file("hooks/.mcp.json") {
        let pack_mcp: Value = serde_json::from_slice(mcp_file.contents())?;
        let mcp_target = opts.project_dir.join(".mcp.json");
        let existing_mcp = read_optional_json(&mcp_target)?;
        let merged_mcp = merge_mcp_json(existing_mcp, &pack_mcp, &pack_id_at_version)?;
        write_json_pretty(&mcp_target, &merged_mcp, &mut receipt)?;
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
    write_text(&claude_md_target, &injected, &mut receipt)?;

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

fn write_pack_file(
    target: &Path,
    bytes: &[u8],
    force: bool,
    receipt: &mut PackInstallReceipt,
) -> Result<(), PackError> {
    ensure_parent(target)?;
    reject_symlink(target)?;
    if target.exists() {
        let existing = std::fs::read(target)?;
        if existing == bytes {
            receipt.files_skipped.push(target.to_path_buf());
            return Ok(());
        }
        if !force {
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
    target: &Path,
    value: &Value,
    receipt: &mut PackInstallReceipt,
) -> Result<(), PackError> {
    ensure_parent(target)?;
    reject_symlink(target)?;
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
    target: &Path,
    text: &str,
    receipt: &mut PackInstallReceipt,
) -> Result<(), PackError> {
    ensure_parent(target)?;
    reject_symlink(target)?;
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

/// Refuse to write through a symlink at the leaf target path.
///
/// Pack install must never overwrite a file outside the project
/// directory by following a symlink that an attacker (or stale state)
/// has placed at the destination. We check `symlink_metadata` which
/// does NOT follow symlinks; if the leaf is a symlink, abort.
///
/// This is leaf-only; symlinked *parent* directories would still be
/// followed by `create_dir_all` + `write`. Callers that need full
/// containment guarantees must canonicalize the project root and walk
/// every parent component — out of scope for v1.
fn reject_symlink(target: &Path) -> Result<(), PackError> {
    match std::fs::symlink_metadata(target) {
        Ok(meta) if meta.file_type().is_symlink() => Err(PackError::MergeConflict {
            file: target.display().to_string(),
            reason: "refusing to write through symlink at install target; \
                     remove the link first or pick a different project dir"
                .to_owned(),
        }),
        Ok(_) | Err(_) => Ok(()),
    }
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
}
