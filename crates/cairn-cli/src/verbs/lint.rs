//! `cairn lint` handler.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::folder::index::{aggregate_folders, project_index};
use cairn_core::domain::folder::policy::FolderPolicy;
use cairn_core::domain::folder::{materialize_backlinks, parse_policy};
use cairn_core::domain::projection::MarkdownProjector;
use cairn_core::generated::envelope::ResponseVerb;
use clap::ArgMatches;

use super::envelope::{emit_json, human_error, unimplemented_response};

/// Result of a `lint --fix-markdown` run.
#[derive(Debug, serde::Serialize)]
pub struct FixMarkdownResult {
    /// Vault-relative paths that were written or updated.
    pub written: Vec<PathBuf>,
    /// Number of files that were already up to date.
    pub already_current: usize,
}

/// Project all active records to markdown, writing files that are missing or stale.
///
/// `vault_root`: absolute path to the vault root (files written relative to this).
/// Returns a `FixMarkdownResult` on success.
///
/// # Errors
///
/// Returns an error if the store cannot be queried, or if any file I/O fails.
pub async fn fix_markdown_handler(
    store: &dyn MemoryStore,
    vault_root: &Path,
) -> anyhow::Result<FixMarkdownResult> {
    let projector = MarkdownProjector;
    let records = store.list_active().await.context("store: list_active")?;
    let mut written = Vec::new();
    let mut already_current: usize = 0;

    for stored in records {
        let projected = projector.project(&stored);
        let abs_path = vault_root.join(&projected.path);

        let needs_write = match tokio::fs::read_to_string(&abs_path).await {
            Ok(existing) => existing != projected.content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => return Err(anyhow::anyhow!("cannot read {}: {e}", abs_path.display())),
        };

        if needs_write {
            if let Some(parent) = abs_path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("create_dir_all {}", parent.display()))?;
            }
            // Write atomically via a unique temp file + rename. tempfile::Builder
            // assigns a random suffix so concurrent calls in the same process never
            // share a temp path; rename(2) is atomic for readers.
            let content = projected.content.clone();
            let dest = abs_path.clone();
            let parent_buf = abs_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf();
            tokio::task::spawn_blocking(move || {
                use std::io::Write as _;
                let mut tmp = tempfile::Builder::new()
                    .suffix(".md.tmp")
                    .tempfile_in(&parent_buf)
                    .with_context(|| format!("create temp file in {}", parent_buf.display()))?;
                tmp.write_all(content.as_bytes())
                    .with_context(|| format!("write temp {}", tmp.path().display()))?;
                tmp.persist(&dest).map_err(|e| {
                    anyhow::anyhow!("persist temp -> {}: {}", dest.display(), e.error)
                })?;
                Ok::<_, anyhow::Error>(())
            })
            .await
            .with_context(|| format!("spawn_blocking write {}", abs_path.display()))??;
            written.push(projected.path);
        } else {
            already_current += 1;
        }
    }

    Ok(FixMarkdownResult {
        written,
        already_current,
    })
}

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
    let mut record_paths: BTreeMap<cairn_core::domain::record::RecordId, PathBuf> =
        BTreeMap::new();
    for stored in &records {
        let pf = projector.project(stored);
        record_paths.insert(stored.record.id.clone(), pf.path);
    }

    // 2. Walk vault for files named `_policy.yaml`.
    let (policies_by_dir, policy_errors) = collect_policies(vault_root).await?;

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

/// Walk `vault_root` for `_policy.yaml` files and parse them. Bad policies
/// are recorded as [`PolicyError`] entries — they do not abort the walk.
async fn collect_policies(
    vault_root: &Path,
) -> anyhow::Result<(BTreeMap<PathBuf, FolderPolicy>, Vec<PolicyError>)> {
    let mut policies_by_dir: BTreeMap<PathBuf, FolderPolicy> = BTreeMap::new();
    let mut policy_errors: Vec<PolicyError> = Vec::new();
    // Skip hidden subdirectories (e.g. `.cairn/`, `.git/`) but never reject
    // the vault root itself — `tempfile::tempdir()` and similar tools
    // commonly produce dot-prefixed root paths.
    let walker = walkdir::WalkDir::new(vault_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_hidden_dir(e));
    for entry in walker {
        let entry = entry.with_context(|| format!("walking {}", vault_root.display()))?;
        if !entry.file_type().is_file() || entry.file_name() != "_policy.yaml" {
            continue;
        }
        let abs = entry.path().to_path_buf();
        let rel = abs
            .strip_prefix(vault_root)
            .with_context(|| format!("strip_prefix {}", abs.display()))?
            .to_path_buf();
        let dir = rel.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        let bytes = tokio::fs::read_to_string(&abs)
            .await
            .with_context(|| format!("read {}", abs.display()))?;
        match parse_policy(&bytes) {
            Ok(p) => {
                policies_by_dir.insert(dir, p);
            }
            // `FolderError` is `#[non_exhaustive]`; treat any current or
            // future variant as a non-fatal policy error so the run
            // continues and the offending subtree is skipped.
            Err(e) => {
                policy_errors.push(PolicyError {
                    path: rel,
                    reason: e.to_string(),
                });
            }
        }
    }
    Ok((policies_by_dir, policy_errors))
}

fn is_hidden_dir(entry: &walkdir::DirEntry) -> bool {
    entry.file_type().is_dir()
        && entry
            .file_name()
            .to_str()
            .is_some_and(|s| s.starts_with('.') && s != ".")
}

/// Run `cairn lint`.
#[must_use]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_markdown_result_counts_written_and_current() {
        // written=2 means 2 files were written/updated
        let result = FixMarkdownResult {
            written: vec!["a.md".into(), "b.md".into()],
            already_current: 3,
        };
        assert_eq!(result.written.len(), 2);
        assert_eq!(result.already_current, 3);
    }

    #[test]
    fn fix_markdown_result_empty() {
        let result = FixMarkdownResult {
            written: vec![],
            already_current: 0,
        };
        assert!(result.written.is_empty());
        assert_eq!(result.already_current, 0);
    }

    #[tokio::test]
    async fn fix_markdown_handler_writes_missing_files() {
        use cairn_test_fixtures::store::{FixtureStore, sample_record};

        let store = FixtureStore::default();
        let record = sample_record();
        store.upsert(record).await.unwrap();

        let vault_root = tempfile::tempdir().unwrap();
        let result = fix_markdown_handler(&store, vault_root.path())
            .await
            .unwrap();

        assert_eq!(result.written.len(), 1);
        assert_eq!(result.already_current, 0);

        // Running again should report already_current=1, written=0
        let result2 = fix_markdown_handler(&store, vault_root.path())
            .await
            .unwrap();
        assert_eq!(result2.written.len(), 0);
        assert_eq!(result2.already_current, 1);
    }
}
