//! `cairn lint` handler.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use cairn_core::contract::memory_store::MemoryStore;
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
            // Write atomically via a temp file + rename so concurrent readers never see
            // a partially-written file. Include the PID in the temp name so concurrent
            // lint invocations don't clobber each other's in-flight writes.
            let tmp_path = abs_path.with_extension(format!("md.{}.tmp", std::process::id()));
            tokio::fs::write(&tmp_path, &projected.content)
                .await
                .with_context(|| format!("write tmp {}", tmp_path.display()))?;
            tokio::fs::rename(&tmp_path, &abs_path)
                .await
                .with_context(|| {
                    format!("rename {} -> {}", tmp_path.display(), abs_path.display())
                })?;
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

/// Run `cairn lint`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    let fix_markdown = sub.get_flag("fix-markdown");

    if fix_markdown {
        // TODO(#9): wire the real SQLite store here once `cairn-store-sqlite` is done.
        // The handler is fully implemented and accepts a `&dyn MemoryStore`, so only
        // this dispatch site needs updating when the store is available.
        let resp = unimplemented_response(ResponseVerb::Lint);
        if json {
            emit_json(&resp);
        } else {
            human_error(
                "lint",
                "Internal",
                "store not wired in this P0 build — --fix-markdown requires #46",
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
