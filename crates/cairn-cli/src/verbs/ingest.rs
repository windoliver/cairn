//! `cairn ingest` handler.
//!
//! Parses CLI args. When source is `-`, reads body from stdin (§5.8).
//! Returns `Internal aborted` until the store is wired (issue #9).
//!
//! The `--resync <path>` flag re-ingests an out-of-band edited markdown
//! projection (brief §3.0, #43). The handler is fully implemented and
//! accepts `&dyn MemoryStore`; the real store is wired in #46.

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::projection::{ConflictOutcome, MarkdownProjector, ResyncError};
use cairn_core::generated::envelope::ResponseVerb;
use clap::ArgMatches;

use super::envelope::{emit_json, human_error, unimplemented_response};

/// Result of a successful `ingest --resync` operation.
#[must_use]
#[derive(Debug, serde::Serialize)]
pub struct ResyncResult {
    /// `"updated"` when the record was written; `"noop"` when the file
    /// was identical to the current store version.
    pub status: &'static str,
    /// Absolute path of the file that was resynced.
    pub path: std::path::PathBuf,
    /// Stable record identifier from the frontmatter `id` field.
    pub target_id: String,
    /// Version of the record as returned by the store after the upsert.
    pub version: u32,
}

/// Re-ingest a markdown projection file that has been edited out-of-band.
///
/// Steps:
/// 1. Read the file from `path`.
/// 2. Parse it with [`MarkdownProjector::parse`].
/// 3. Look up the current store record with [`MemoryStore::get`].
/// 4. Run [`MarkdownProjector::check_conflict`].
/// 5. On [`ConflictOutcome::Clean`]: upsert the updated record.
/// 6. On [`ConflictOutcome::Conflict`]: write a quarantine file to
///    `<vault_root>/.cairn/quarantine/<ts>-<id>.rejected` and return an error.
///
/// # Errors
///
/// Returns an error if the file cannot be read, fails to parse, the store
/// operation fails, or a conflict is detected.
pub async fn resync_handler(
    store: &dyn MemoryStore,
    path: &Path,
    vault_root: &Path,
) -> anyhow::Result<ResyncResult> {
    let content = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("ingest --resync: {}", path.display()))?;

    let projector = MarkdownProjector;
    let parsed = projector.parse(&content).map_err(|e| match e {
        ResyncError::MissingId => {
            anyhow::anyhow!("ingest --resync: missing `id` field in {}", path.display())
        }
        ResyncError::ParseFailed(ref msg) => anyhow::anyhow!(
            "ingest --resync: parse error in {}: {msg}",
            path.display()
        ),
        // Conflict variant on parse should not occur; surface it defensively.
        ResyncError::Conflict { file_version, store_version, ref reason } => anyhow::anyhow!(
            "ingest --resync: unexpected conflict during parse (file={file_version}, store={store_version}): {reason}"
        ),
        _ => anyhow::anyhow!("ingest --resync: {e:?}"),
    })?;

    let current = store.get(&parsed.target_id).await.context("store: get")?;

    let outcome = projector.check_conflict(&parsed, current.as_ref());

    match outcome {
        ConflictOutcome::Clean => {
            if let Some(ref stored) = current {
                // Check if mutable fields are already up to date
                if stored.record.body == parsed.body && stored.record.tags == parsed.tags {
                    return Ok(ResyncResult {
                        status: "noop",
                        path: path.to_path_buf(),
                        target_id: parsed.target_id,
                        version: stored.version,
                    });
                }
                // Merge mutable fields and upsert
                let mut r = stored.record.clone();
                r.body = parsed.body.clone();
                r.tags = parsed.tags.clone();
                let new_stored = store.upsert(r).await.context("store: upsert")?;
                Ok(ResyncResult {
                    status: "updated",
                    path: path.to_path_buf(),
                    target_id: parsed.target_id,
                    version: new_stored.version,
                })
            } else {
                // New record — build_record_from_parsed (deferred to #46)
                let record = build_record_from_parsed(&parsed)?;
                let new_stored = store.upsert(record).await.context("store: upsert")?;
                Ok(ResyncResult {
                    status: "updated",
                    path: path.to_path_buf(),
                    target_id: parsed.target_id,
                    version: new_stored.version,
                })
            }
        }
        ConflictOutcome::Conflict {
            ref marker,
            file_version,
            store_version,
        } => {
            // Write a quarantine file so the editor's changes are not lost.
            let quarantine_dir = vault_root.join(".cairn/quarantine");
            tokio::fs::create_dir_all(&quarantine_dir)
                .await
                .with_context(|| format!("create quarantine dir {}", quarantine_dir.display()))?;
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let quarantine_path =
                quarantine_dir.join(format!("{timestamp}-{}.rejected", &parsed.target_id));
            tokio::fs::write(&quarantine_path, &content)
                .await
                .with_context(|| format!("write quarantine {}", quarantine_path.display()))?;

            Err(anyhow::anyhow!(
                "conflict: file version {file_version}, store version {store_version}; {marker}; \
                 rejected content saved to .cairn/quarantine/"
            ))
        }
        // ConflictOutcome is #[non_exhaustive]; catch future variants.
        _ => Err(anyhow::anyhow!(
            "ingest --resync: unexpected conflict outcome"
        )),
    }
}

/// Construct a minimal [`cairn_core::domain::record::MemoryRecord`] from a
/// [`cairn_core::domain::projection::ParsedProjection`] for the "new record"
/// branch of the resync path.
///
/// The "new record" path is a TODO(#46) stub — the real pipeline (WAL,
/// consent journal, signing) is not wired yet. Returns an error directing
/// the caller to use `cairn ingest` for brand-new records.
fn build_record_from_parsed(
    _parsed: &cairn_core::domain::projection::ParsedProjection,
) -> anyhow::Result<cairn_core::domain::record::MemoryRecord> {
    Err(anyhow::anyhow!(
        "ingest --resync: creating a brand-new record via resync requires the full ingest \
         pipeline (TODO #46); please run `cairn ingest` first to create the record, \
         then use --resync to re-ingest edits"
    ))
}

/// Run `cairn ingest`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");

    // --resync <path>: re-ingest an out-of-band edited markdown projection.
    if let Some(resync_path) = sub.get_one::<std::path::PathBuf>("resync") {
        // TODO(#46): wire vault_root from resolved vault config.
        // For now: use CWD as vault_root placeholder.
        let resp = unimplemented_response(ResponseVerb::Ingest);
        if json {
            emit_json(&resp);
        } else {
            human_error(
                "ingest",
                "Internal",
                &format!(
                    "store not wired in this P0 build — --resync {} requires #46",
                    resync_path.display()
                ),
                &resp.operation_id,
            );
        }
        return ExitCode::FAILURE;
    }

    // Enforce IDL exactly-one-of: body/file/url (positional `source` counts as one).
    let has_source = sub.get_one::<String>("source").is_some();
    let has_body = sub.get_one::<String>("body").is_some();
    let has_file = sub.get_one::<std::path::PathBuf>("file").is_some();
    let has_url = sub.get_one::<String>("url").is_some();
    let source_count =
        u8::from(has_source) + u8::from(has_body) + u8::from(has_file) + u8::from(has_url);
    if source_count != 1 {
        eprintln!(
            "cairn ingest: exactly one of [source, --body, --file, --url] is required (got {source_count})"
        );
        return ExitCode::from(64);
    }

    // Resolve body: positional `source` wins if set; --body/--file/--url otherwise.
    let _body_resolved: Option<String> = if let Some(src) = sub.get_one::<String>("source") {
        if src == "-" {
            let mut buf = String::new();
            // Cap at 4 MiB to avoid unbounded allocation in the stubbed path.
            if std::io::stdin()
                .take(4 * 1024 * 1024)
                .read_to_string(&mut buf)
                .is_err()
            {
                let r = unimplemented_response(ResponseVerb::Ingest);
                if json {
                    emit_json(&r);
                } else {
                    human_error(
                        "ingest",
                        "Internal",
                        "failed to read stdin",
                        &r.operation_id,
                    );
                }
                return ExitCode::FAILURE;
            }
            Some(buf)
        } else {
            Some(src.clone())
        }
    } else {
        sub.get_one::<String>("body").cloned()
    };

    let resp = unimplemented_response(ResponseVerb::Ingest);
    if json {
        emit_json(&resp);
    } else {
        let op = resp.operation_id.clone();
        human_error(
            "ingest",
            "Internal",
            "store not wired in this P0 build",
            &op,
        );
    }
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::domain::projection::MarkdownProjector;
    use cairn_test_fixtures::store::{FixtureStore, sample_stored_record};

    #[tokio::test]
    async fn resync_clean_upserts_record() {
        let store = FixtureStore::default();
        // Pre-populate store with version 1.
        let stored = sample_stored_record(1);
        store.upsert(stored.record.clone()).await.unwrap();

        // Project to markdown, then modify the body so the resync is a real
        // edit (not a noop).  Version still matches → Clean → upsert.
        let proj = MarkdownProjector;
        let file = proj.project(&stored);
        // Append " edited" to the body so body != stored body → triggers upsert.
        let modified_content = file.content.replace(
            &stored.record.body,
            &format!("{} edited", stored.record.body),
        );
        let vault_root = tempfile::tempdir().unwrap();
        let abs_path = vault_root.path().join(&file.path);
        tokio::fs::create_dir_all(abs_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&abs_path, &modified_content)
            .await
            .unwrap();

        let result = resync_handler(&store, &abs_path, vault_root.path())
            .await
            .unwrap();
        assert_eq!(result.target_id, stored.record.id.as_str());
        // Store started at version 1 (one upsert above); resync does another
        // upsert → version 2.
        assert_eq!(result.version, 2);
        assert_eq!(result.status, "updated");
    }

    #[tokio::test]
    async fn resync_noop_when_content_unchanged() {
        let store = FixtureStore::default();
        // Pre-populate store with version 1.
        let stored = sample_stored_record(1);
        store.upsert(stored.record.clone()).await.unwrap();

        // Project to markdown and resync it — body/tags are identical → noop.
        let proj = MarkdownProjector;
        let file = proj.project(&stored);
        let vault_root = tempfile::tempdir().unwrap();
        let abs_path = vault_root.path().join(&file.path);
        tokio::fs::create_dir_all(abs_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&abs_path, &file.content).await.unwrap();

        let result = resync_handler(&store, &abs_path, vault_root.path())
            .await
            .unwrap();
        assert_eq!(result.status, "noop");
        // Version should be unchanged at 1 — no upsert was performed.
        assert_eq!(result.version, 1);
        assert_eq!(result.target_id, stored.record.id.as_str());
    }

    #[tokio::test]
    async fn resync_conflict_writes_quarantine_file() {
        let store = FixtureStore::default();
        // Store a record and then upsert it multiple times to advance the
        // version to 5, while keeping a v1 projected file.
        let base = sample_stored_record(1);
        store.upsert(base.record.clone()).await.unwrap(); // → v1
        store.upsert(base.record.clone()).await.unwrap(); // → v2
        store.upsert(base.record.clone()).await.unwrap(); // → v3
        store.upsert(base.record.clone()).await.unwrap(); // → v4
        store.upsert(base.record.clone()).await.unwrap(); // → v5

        // Write a file that claims to be at version 1 (stale).
        let proj = MarkdownProjector;
        let v1_stored = sample_stored_record(1);
        let file = proj.project(&v1_stored);
        let vault_root = tempfile::tempdir().unwrap();
        let quarantine_dir = vault_root.path().join(".cairn/quarantine");
        let abs_path = vault_root.path().join(&file.path);
        tokio::fs::create_dir_all(abs_path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&abs_path, &file.content).await.unwrap();

        let err = resync_handler(&store, &abs_path, vault_root.path())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("conflict"),
            "error should mention conflict: {err}"
        );

        // Quarantine file should exist.
        let entries: Vec<_> = std::fs::read_dir(&quarantine_dir)
            .expect("quarantine dir should exist after conflict")
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            !entries.is_empty(),
            "quarantine file should have been written"
        );
    }
}
