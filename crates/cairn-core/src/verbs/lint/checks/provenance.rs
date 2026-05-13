//! §6.3 — source-link hygiene over record provenance.
//!
//! Records currently carry `source_ids` in frontmatter (`extra_frontmatter`)
//! rather than in the typed `Provenance` struct. This check therefore audits
//! the projected frontmatter plus the typed `provenance.source_hash` field:
//!
//! - `source_ids` must exist and contain at least one string path
//! - each referenced `sources/...` file must resolve under `vault_root`
//! - the source file bytes must match `provenance.source_hash`
//! - no active record may reference a `source_id` whose `source_forget`
//!   row has already been written (rule 4 — `source_not_forgotten`)
//! - when `vault.redact_on_forget = true`, every `source_forget` row's
//!   source file must be content-scrubbed (zero-length on disk); the
//!   journal row + hash are all that may remain (rule 5 —
//!   `source_redact_on_forget_honored`)
//!
//! The source-forget rules require `LintInputs::source_forgets` to be wired
//! by the dispatch layer (pre-fetched `consent_journal` slice keyed by
//! `source_id`). Until that write path lands, the CLI passes `None` and
//! these two rules degrade to no-ops.
//!
//! When `vault_root` is not wired, filesystem-backed checks become a no-op so
//! unrelated fixture tests can keep constructing `LintInputs` without a vault.

use std::path::{Path, PathBuf};

use blake3::Hasher as Blake3Hasher;
use sha2::{Digest, Sha256, Sha512};

use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{LintInputs, LintRecord, finding, target_path, target_record};

/// Run the source-link hygiene checks over every active record.
#[must_use]
pub fn run(inputs: &LintInputs<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();
    let source_root = inputs.config.vault.layout.sources.as_str();
    for record in inputs.records {
        let source_ids = match source_ids(record) {
            Ok(ids) => ids,
            Err(finding) => {
                findings.push(finding);
                continue;
            }
        };

        for source_id in &source_ids {
            let relative_path = match validate_source_id_shape(source_root, source_id) {
                Ok(path) => path,
                Err(detail) => {
                    findings.push(invalid_source_id(record, source_id, &detail));
                    continue;
                }
            };

            // Rule 4: source_not_forgotten — even before touching the
            // filesystem, flag records that point at a source whose
            // `source_forget` journal row has been written. Body-free
            // check; vault_root is irrelevant.
            if let Some(forgets) = inputs.source_forgets
                && let Some(entry) = forgets.get(*source_id)
            {
                findings.push(source_after_forget(record, source_id, &entry.forget_op_id));
                continue;
            }

            let Some(vault_root) = inputs.vault_root else {
                continue;
            };
            let expected_path = vault_root.join(&relative_path);
            if !expected_path.is_file() {
                findings.push(missing_source_file(record, source_id, &expected_path));
                continue;
            }

            if source_ids.len() != 1 {
                findings.push(multi_source_hash_gap(record, source_id, &expected_path));
                continue;
            }

            match source_hash_matches(
                &expected_path,
                source_id,
                &record.stored.record.provenance.source_hash,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    findings.push(hash_mismatch(record, source_id, &expected_path));
                }
                Err(message) => {
                    findings.push(hash_check_failed(
                        record,
                        source_id,
                        &expected_path,
                        &message,
                    ));
                }
            }
        }
    }

    // Rule 5: source_redact_on_forget_honored. Only enforced when both
    // the config opt-in is set and a vault_root is wired so the on-disk
    // file size can be probed. The check is per-source (not per-record):
    // each `source_forget` row is audited once regardless of how many
    // records cited the source before it was forgotten.
    if inputs.config.vault.redact_on_forget
        && let (Some(forgets), Some(vault_root)) = (inputs.source_forgets, inputs.vault_root)
    {
        let mut audited: Vec<&str> = forgets.keys().map(String::as_str).collect();
        audited.sort_unstable();
        for source_id in audited {
            let Ok(relative_path) = validate_source_id_shape(source_root, source_id) else {
                // Bad shape on a journal row is the source-forget write
                // path's bug — surface it as a deferred check rather than
                // silently dropping the audit.
                findings.push(redact_audit_invalid_source_id(source_id));
                continue;
            };
            let expected_path = vault_root.join(&relative_path);
            match std::fs::metadata(&expected_path) {
                Ok(meta) if meta.is_file() && meta.len() > 0 => {
                    findings.push(source_redact_skipped(source_id, &expected_path));
                }
                // Missing or zero-length ⇒ purged or scrubbed-in-place;
                // both satisfy the invariant.
                _ => {}
            }
        }
    }

    findings
}

fn source_ids<'a>(record: &'a LintRecord) -> Result<Vec<&'a str>, Finding> {
    let Some(value) = record.stored.record.extra_frontmatter.get("source_ids") else {
        return Err(missing_source_ids(
            record,
            "record frontmatter is missing `source_ids`",
        ));
    };
    let Some(items) = value.as_array() else {
        return Err(missing_source_ids(
            record,
            "`source_ids` must be an array of vault-relative source paths",
        ));
    };

    let ids: Vec<&str> = items.iter().filter_map(serde_json::Value::as_str).collect();
    if ids.is_empty() || ids.len() != items.len() {
        return Err(missing_source_ids(
            record,
            "`source_ids` must contain at least one string path",
        ));
    }

    Ok(ids)
}

fn validate_source_id_shape(source_root: &str, source_id: &str) -> Result<PathBuf, String> {
    use std::path::Component;

    let path = Path::new(source_id);
    let required_prefix = format!("{source_root}/");
    if source_id.is_empty() {
        return Err("source_ids entries must not be empty".to_owned());
    }
    if source_id.contains('\0') {
        return Err("source_ids entries must not contain NUL bytes".to_owned());
    }
    if source_id.contains('\\') {
        return Err("source_ids entries must use forward slashes".to_owned());
    }
    if source_id.contains("://") {
        return Err("source_ids entries must not contain a URL scheme".to_owned());
    }
    if path.is_absolute() {
        return Err("source_ids entries must be vault-relative paths".to_owned());
    }
    if !source_id.starts_with(&required_prefix) {
        return Err(format!(
            "source_ids entries must live under `{source_root}/`"
        ));
    }
    if source_id.contains('?') || source_id.contains('#') {
        return Err("source_ids entries must not contain query or fragment suffixes".to_owned());
    }
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err("source_ids entries must not contain `..`".to_owned());
            }
            Component::Normal(seg) if seg.is_empty() => {
                return Err("source_ids entries must not contain empty path segments".to_owned());
            }
            _ => {}
        }
    }
    if source_id.split('/').any(str::is_empty) {
        return Err("source_ids entries must not contain empty path segments".to_owned());
    }
    Ok(path.to_path_buf())
}

fn source_hash_matches(path: &Path, source_id: &str, expected: &str) -> Result<bool, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("unable to read source file {}: {e}", path.display()))?;

    if hash_bytes(expected, &bytes)? == *expected {
        return Ok(true);
    }

    if !can_normalize_text_line_endings(source_id, &bytes) {
        return Ok(false);
    }

    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(false);
    };
    let normalized = text.replace("\r\n", "\n");
    Ok(hash_bytes(expected, normalized.as_bytes())? == *expected)
}

fn can_normalize_text_line_endings(source_id: &str, bytes: &[u8]) -> bool {
    if has_known_binary_extension(source_id) {
        return false;
    }

    is_text_like_bytes(bytes)
}

fn has_known_binary_extension(source_id: &str) -> bool {
    let Some(ext) = Path::new(source_id)
        .extension()
        .and_then(|ext| ext.to_str())
    else {
        return false;
    };

    matches!(
        ext.to_ascii_lowercase().as_str(),
        "bin"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "pdf"
            | "zip"
            | "gz"
            | "bz2"
            | "xz"
            | "7z"
            | "tar"
            | "mp3"
            | "mp4"
            | "mov"
            | "avi"
            | "wav"
            | "ogg"
            | "woff"
            | "woff2"
            | "ttf"
            | "otf"
            | "exe"
            | "dll"
            | "so"
            | "dylib"
            | "class"
            | "jar"
            | "pyc"
    )
}

fn is_text_like_bytes(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };

    !text
        .chars()
        .any(|ch| ch.is_control() && ch != '\n' && ch != '\r' && ch != '\t')
}

fn hash_bytes(expected: &str, bytes: &[u8]) -> Result<String, String> {
    let Some((algo, _)) = expected.split_once(':') else {
        return Err(format!(
            "unsupported provenance.source_hash format `{expected}`"
        ));
    };

    let digest = match algo {
        "sha256" => format!("sha256:{:x}", Sha256::digest(bytes)),
        "sha512" => format!("sha512:{:x}", Sha512::digest(bytes)),
        "blake3" => {
            let mut hasher = Blake3Hasher::new();
            hasher.update(bytes);
            format!("blake3:{}", hasher.finalize().to_hex())
        }
        _ => {
            return Err(format!(
                "unsupported provenance.source_hash algorithm `{algo}`"
            ));
        }
    };

    Ok(digest)
}

fn missing_source_ids(record: &LintRecord, detail: &str) -> Finding {
    let mut f = finding(
        Kind::MissingProvenance,
        Severity::Error,
        format!(
            "record is missing valid source-link provenance: {detail} \
             (expected non-empty `source_ids` frontmatter)"
        ),
    );
    f.target = Some(target_record(&record.stored.record.id));
    f.suggested_fix = Some(
        "re-ingest the record with `source_ids` frontmatter pointing at the \
         source file(s) under the configured source root"
            .to_owned(),
    );
    f
}

fn missing_source_file(record: &LintRecord, source_id: &str, expected_path: &Path) -> Finding {
    let mut f = finding(
        Kind::BrokenSourceLink,
        Severity::Error,
        format!(
            "source link does not resolve: source_id={source_id} \
             expected_path={}",
            expected_path.display()
        ),
    );
    f.target = Some(target_record(&record.stored.record.id));
    f.suggested_fix = Some(
        "restore the missing source file or re-ingest the record with a valid \
         `source_ids` entry"
            .to_owned(),
    );
    f
}

fn invalid_source_id(record: &LintRecord, source_id: &str, detail: &str) -> Finding {
    let mut f = finding(
        Kind::MissingProvenance,
        Severity::Error,
        format!("invalid source_id `{source_id}`: {detail}"),
    );
    f.target = Some(target_record(&record.stored.record.id));
    f.suggested_fix = Some(
        "re-ingest the record with a vault-relative source-root path in \
         `source_ids`"
            .to_owned(),
    );
    f
}

fn hash_mismatch(record: &LintRecord, source_id: &str, expected_path: &Path) -> Finding {
    let mut f = finding(
        Kind::MissingProvenance,
        Severity::Error,
        format!(
            "source hash mismatch: source_id={source_id} expected_path={} \
             expected_hash={}",
            expected_path.display(),
            record.stored.record.provenance.source_hash
        ),
    );
    f.target = Some(target_record(&record.stored.record.id));
    f.suggested_fix = Some(
        "restore the original source bytes or re-ingest the record so \
         provenance.source_hash matches the current source file"
            .to_owned(),
    );
    f
}

fn hash_check_failed(
    record: &LintRecord,
    source_id: &str,
    expected_path: &Path,
    detail: &str,
) -> Finding {
    let mut f = finding(
        Kind::MissingProvenance,
        Severity::Error,
        format!(
            "source hash check failed: source_id={source_id} expected_path={} {detail}",
            expected_path.display()
        ),
    );
    f.target = Some(target_record(&record.stored.record.id));
    f
}

fn source_after_forget(record: &LintRecord, source_id: &str, forget_op_id: &str) -> Finding {
    let mut f = finding(
        Kind::MissingProvenance,
        Severity::Error,
        format!(
            "source_after_forget: record references forgotten source. \
             source_id={source_id} forget_op_id={forget_op_id}"
        ),
    );
    f.target = Some(target_record(&record.stored.record.id));
    f.suggested_fix = Some(
        "forget this record (or rewrite it without the forgotten \
         source_id) — the source's `source_forget` journal row was \
         already written"
            .to_owned(),
    );
    f.tracking_issue = Some(257);
    f
}

fn source_redact_skipped(source_id: &str, expected_path: &Path) -> Finding {
    let mut f = finding(
        Kind::MissingProvenance,
        Severity::Error,
        format!(
            "source_redact_skipped: vault.redact_on_forget is true but the \
             source file still has non-zero length on disk. \
             source_id={source_id} expected_path={}",
            expected_path.display()
        ),
    );
    f.target = Some(target_path(source_id.to_owned()));
    f.suggested_fix = Some(
        "scrub the source file in place (truncate to zero length) or \
         disable `vault.redact_on_forget` if mandatory redaction is not \
         the intended policy"
            .to_owned(),
    );
    f.tracking_issue = Some(257);
    f
}

fn redact_audit_invalid_source_id(source_id: &str) -> Finding {
    let mut f = finding(
        Kind::DeferredCheck,
        Severity::Warning,
        format!(
            "redact_on_forget audit skipped: `source_forget` journal row \
             carries a malformed source_id={source_id} (the write path \
             allowed a value the audit cannot resolve to a vault path)"
        ),
    );
    f.target = Some(target_path(source_id.to_owned()));
    f.tracking_issue = Some(257);
    f
}

fn multi_source_hash_gap(record: &LintRecord, source_id: &str, expected_path: &Path) -> Finding {
    let mut f = finding(
        Kind::DeferredCheck,
        Severity::Info,
        format!(
            "multi-source hash verification is deferred for source_id={source_id} \
             expected_path={}: record carries multiple `source_ids` but only one \
             `provenance.source_hash` value",
            expected_path.display()
        ),
    );
    f.target = Some(target_record(&record.stored.record.id));
    f.suggested_fix = Some(
        "extend the record data model with per-source hashes before enforcing \
         hash equality across multi-source records"
            .to_owned(),
    );
    f.tracking_issue = Some(257);
    f
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};

    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::{IndexStats, StoredRecord};
    use crate::domain::Rfc3339Timestamp;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::source_forget::SourceForgetEntry;
    use crate::verbs::lint::{
        ConsentModel, LintInputs, LintRecord, SchemaVersion, empty_author_states,
        empty_unresolvable_authors,
    };

    fn lint_record() -> LintRecord {
        LintRecord {
            stored: StoredRecord {
                record: sample_record(),
                version: 1,
                schema_version: Some(SchemaVersion::current()),
            },
            consent_model: ConsentModel::LegacyEvent,
        }
    }

    fn with_source_ids(mut record: LintRecord, ids: &[&str]) -> LintRecord {
        record.stored.record.extra_frontmatter =
            BTreeMap::from([("source_ids".to_owned(), serde_json::json!(ids))]);
        record
    }

    fn with_expected_hash(mut record: LintRecord, hash: String) -> LintRecord {
        record.stored.record.provenance.source_hash = hash;
        record
    }

    fn inputs<'a>(
        cfg: &'a CairnConfig,
        records: &'a [LintRecord],
        root: Option<&'a Path>,
    ) -> LintInputs<'a> {
        LintInputs {
            records,
            config: cfg,
            index_stats: IndexStats::new(records.len() as u64, records.len() as u64),
            author_states: empty_author_states(),
            unresolvable_authors: empty_unresolvable_authors(),
            consent_lookup: None,
            vault_root: root,
            source_forgets: None,
            hot_body_loader: None,
        }
    }

    fn inputs_with_forgets<'a>(
        cfg: &'a CairnConfig,
        records: &'a [LintRecord],
        root: Option<&'a Path>,
        forgets: &'a HashMap<String, SourceForgetEntry>,
    ) -> LintInputs<'a> {
        let mut li = inputs(cfg, records, root);
        li.source_forgets = Some(forgets);
        li
    }

    fn forgets(entries: &[(&str, &str)]) -> HashMap<String, SourceForgetEntry> {
        let ts = Rfc3339Timestamp::parse("2026-05-12T00:00:00Z").expect("invariant: valid ts");
        entries
            .iter()
            .map(|(sid, op)| {
                (
                    (*sid).to_owned(),
                    SourceForgetEntry::new(*sid, *op, ts.clone()),
                )
            })
            .collect()
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        format!("sha256:{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn missing_source_ids_emits_missing_provenance_error() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let records = [lint_record()];
        let findings = run(&inputs(&cfg, &records, Some(dir.path())));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("source_ids"));
    }

    #[test]
    fn empty_source_ids_emits_missing_provenance_error() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let records = [with_source_ids(lint_record(), &[])];
        let findings = run(&inputs(&cfg, &records, Some(dir.path())));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert!(findings[0].message.contains("source_ids"));
    }

    #[test]
    fn missing_source_file_emits_broken_source_link_error() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "sources/hook/missing.txt";
        let records = [with_source_ids(lint_record(), &[source_id])];
        let findings = run(&inputs(&cfg, &records, Some(dir.path())));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::BrokenSourceLink);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains(source_id));
        assert!(findings[0].message.contains("expected_path="));
    }

    #[test]
    fn source_id_outside_sources_emits_missing_provenance_error() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let records = [with_source_ids(lint_record(), &["../escape.txt"])];
        let findings = run(&inputs(&cfg, &records, Some(dir.path())));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert!(findings[0].message.contains("invalid source_id"));
    }

    #[test]
    fn source_id_with_scheme_is_rejected() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let records = [with_source_ids(
            lint_record(),
            &["https://example.com/source.txt"],
        )];
        let findings = run(&inputs(&cfg, &records, Some(dir.path())));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert!(findings[0].message.contains("invalid source_id"));
    }

    #[test]
    fn source_id_with_empty_segment_is_rejected() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let records = [with_source_ids(lint_record(), &["sources//double.txt"])];
        let findings = run(&inputs(&cfg, &records, Some(dir.path())));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert!(findings[0].message.contains("invalid source_id"));
    }

    #[test]
    fn edited_source_file_emits_missing_provenance_error() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "sources/hook/source.txt";
        let source_path = dir.path().join(source_id);
        std::fs::create_dir_all(source_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&source_path, "edited bytes").expect("write source");

        let record = with_expected_hash(
            with_source_ids(lint_record(), &[source_id]),
            sha256_hex(b"original bytes"),
        );
        let findings = run(&inputs(&cfg, &[record], Some(dir.path())));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert!(findings[0].message.contains("source hash mismatch"));
    }

    #[test]
    fn malformed_source_hash_format_emits_hash_check_failed_error() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "sources/hook/source.txt";
        let source_path = dir.path().join(source_id);
        std::fs::create_dir_all(source_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&source_path, "alpha\nbeta\n").expect("write source");

        let record = with_expected_hash(
            with_source_ids(lint_record(), &[source_id]),
            "not-a-digest".to_owned(),
        );
        let findings = run(&inputs(&cfg, &[record], Some(dir.path())));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert!(findings[0].message.contains("source hash check failed"));
        assert!(
            findings[0]
                .message
                .contains("unsupported provenance.source_hash format")
        );
    }

    #[test]
    fn unsupported_source_hash_algorithm_emits_hash_check_failed_error() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "sources/hook/source.txt";
        let source_path = dir.path().join(source_id);
        std::fs::create_dir_all(source_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&source_path, "alpha\nbeta\n").expect("write source");

        let record = with_expected_hash(
            with_source_ids(lint_record(), &[source_id]),
            "sha1:deadbeef".to_owned(),
        );
        let findings = run(&inputs(&cfg, &[record], Some(dir.path())));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert!(findings[0].message.contains("source hash check failed"));
        assert!(
            findings[0]
                .message
                .contains("unsupported provenance.source_hash algorithm")
        );
    }

    #[test]
    fn crlf_text_source_matches_lf_hash() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "sources/hook/source.txt";
        let source_path = dir.path().join(source_id);
        std::fs::create_dir_all(source_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&source_path, "alpha\r\nbeta\r\n").expect("write source");

        let record = with_expected_hash(
            with_source_ids(lint_record(), &[source_id]),
            sha256_hex(b"alpha\nbeta\n"),
        );
        let findings = run(&inputs(&cfg, &[record], Some(dir.path())));
        assert!(
            findings.is_empty(),
            "expected no findings, got {findings:?}"
        );
    }

    #[test]
    fn no_vault_root_still_enforces_source_ids_presence() {
        let cfg = CairnConfig::default();
        let records = [lint_record()];
        let findings = run(&inputs(&cfg, &records, None));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert!(findings[0].message.contains("source_ids"));
    }

    #[test]
    fn no_vault_root_skips_filesystem_backed_source_checks() {
        let cfg = CairnConfig::default();
        let records = [with_source_ids(lint_record(), &["sources/hook/source.txt"])];
        assert!(run(&inputs(&cfg, &records, None)).is_empty());
    }

    #[test]
    fn no_vault_root_still_rejects_invalid_source_id_shapes() {
        let cfg = CairnConfig::default();
        let records = [with_source_ids(lint_record(), &["../escape.txt"])];
        let findings = run(&inputs(&cfg, &records, None));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert!(findings[0].message.contains("invalid source_id"));
    }

    #[test]
    fn crlf_normalization_does_not_hide_non_text_utf8_byte_drift() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "sources/hook/source.bin";
        let source_path = dir.path().join(source_id);
        std::fs::create_dir_all(source_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&source_path, "{\"a\":1}\r\n{\"b\":2}\r\n").expect("write source");

        let record = with_expected_hash(
            with_source_ids(lint_record(), &[source_id]),
            sha256_hex(b"{\"a\":1}\n{\"b\":2}\n"),
        );
        let findings = run(&inputs(&cfg, &[record], Some(dir.path())));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert!(findings[0].message.contains("source hash mismatch"));
    }

    #[test]
    fn crlf_extensionless_text_source_matches_lf_hash() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "sources/hook/raw-email";
        let source_path = dir.path().join(source_id);
        std::fs::create_dir_all(source_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&source_path, "alpha\r\nbeta\r\n").expect("write source");

        let record = with_expected_hash(
            with_source_ids(lint_record(), &[source_id]),
            sha256_hex(b"alpha\nbeta\n"),
        );
        let findings = run(&inputs(&cfg, &[record], Some(dir.path())));
        assert!(
            findings.is_empty(),
            "expected no findings, got {findings:?}"
        );
    }

    #[test]
    fn multiple_source_ids_emit_deferred_hash_gap_instead_of_false_mismatch() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("sources/hook/a.txt");
        let b = dir.path().join("sources/hook/b.txt");
        std::fs::create_dir_all(a.parent().expect("parent")).expect("mkdir");
        std::fs::write(&a, "alpha").expect("write a");
        std::fs::write(&b, "beta").expect("write b");

        let record = with_expected_hash(
            with_source_ids(lint_record(), &["sources/hook/a.txt", "sources/hook/b.txt"]),
            sha256_hex(b"alpha"),
        );
        let findings = run(&inputs(&cfg, &[record], Some(dir.path())));
        assert_eq!(findings.len(), 2);
        assert!(findings.iter().all(|f| f.kind == Kind::DeferredCheck));
    }

    #[test]
    fn configurable_source_root_is_respected() {
        let mut cfg = CairnConfig::default();
        cfg.vault.layout.sources = "inbox".to_owned();
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "inbox/hook/source.txt";
        let source_path = dir.path().join(source_id);
        std::fs::create_dir_all(source_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&source_path, "alpha\nbeta\n").expect("write source");

        let record = with_expected_hash(
            with_source_ids(lint_record(), &[source_id]),
            sha256_hex(b"alpha\nbeta\n"),
        );
        let findings = run(&inputs(&cfg, &[record], Some(dir.path())));
        assert!(
            findings.is_empty(),
            "expected no findings, got {findings:?}"
        );
    }

    // ── Rule 4: source_not_forgotten ───────────────────────────────────

    #[test]
    fn record_referencing_forgotten_source_emits_source_after_forget_error() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "sources/hook/forgotten.txt";
        let records = [with_source_ids(lint_record(), &[source_id])];
        let f = forgets(&[(source_id, "op-forget-42")]);
        let findings = run(&inputs_with_forgets(&cfg, &records, Some(dir.path()), &f));
        assert_eq!(findings.len(), 1, "got: {findings:?}");
        assert_eq!(findings[0].kind, Kind::MissingProvenance);
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("source_after_forget"));
        assert!(findings[0].message.contains(source_id));
        assert!(findings[0].message.contains("op-forget-42"));
        assert_eq!(findings[0].tracking_issue, Some(257));
    }

    #[test]
    fn record_with_non_forgotten_source_passes_rule_4() {
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "sources/hook/active.txt";
        let source_path = dir.path().join(source_id);
        std::fs::create_dir_all(source_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&source_path, "alpha\nbeta\n").expect("write source");
        let record = with_expected_hash(
            with_source_ids(lint_record(), &[source_id]),
            sha256_hex(b"alpha\nbeta\n"),
        );
        let f = forgets(&[]);
        let findings = run(&inputs_with_forgets(&cfg, &[record], Some(dir.path()), &f));
        assert!(findings.is_empty(), "got: {findings:?}");
    }

    #[test]
    fn rule_4_no_op_when_source_forgets_unwired() {
        // None for source_forgets ⇒ rule 4 must not synthesize findings.
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "sources/hook/active.txt";
        let source_path = dir.path().join(source_id);
        std::fs::create_dir_all(source_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&source_path, "alpha\nbeta\n").expect("write source");
        let record = with_expected_hash(
            with_source_ids(lint_record(), &[source_id]),
            sha256_hex(b"alpha\nbeta\n"),
        );
        let findings = run(&inputs(&cfg, &[record], Some(dir.path())));
        assert!(findings.is_empty(), "got: {findings:?}");
    }

    #[test]
    fn rule_4_short_circuits_filesystem_check() {
        // When source is forgotten, emit forget finding, not BrokenSourceLink.
        let cfg = CairnConfig::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "sources/hook/forgotten.txt";
        let records = [with_source_ids(lint_record(), &[source_id])];
        let f = forgets(&[(source_id, "op-forget-7")]);
        let findings = run(&inputs_with_forgets(&cfg, &records, Some(dir.path()), &f));
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("source_after_forget"));
    }

    // ── Rule 5: source_redact_on_forget_honored ────────────────────────

    fn cfg_with_redact_on_forget() -> CairnConfig {
        let mut cfg = CairnConfig::default();
        cfg.vault.redact_on_forget = true;
        cfg
    }

    #[test]
    fn rule_5_flags_unscrubbed_source_when_config_enabled() {
        let cfg = cfg_with_redact_on_forget();
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "sources/hook/forgotten.txt";
        let source_path = dir.path().join(source_id);
        std::fs::create_dir_all(source_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&source_path, "still has content").expect("write source");
        let f = forgets(&[(source_id, "op-forget-9")]);
        let findings = run(&inputs_with_forgets(&cfg, &[], Some(dir.path()), &f));
        let redact: Vec<_> = findings
            .iter()
            .filter(|f| f.message.contains("source_redact_skipped"))
            .collect();
        assert_eq!(redact.len(), 1, "got: {findings:?}");
        assert_eq!(redact[0].severity, Severity::Error);
        assert_eq!(redact[0].tracking_issue, Some(257));
    }

    #[test]
    fn rule_5_passes_when_source_file_is_zero_length() {
        let cfg = cfg_with_redact_on_forget();
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "sources/hook/forgotten.txt";
        let source_path = dir.path().join(source_id);
        std::fs::create_dir_all(source_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&source_path, b"").expect("write empty");
        let f = forgets(&[(source_id, "op-forget-9")]);
        let findings = run(&inputs_with_forgets(&cfg, &[], Some(dir.path()), &f));
        assert!(findings.is_empty(), "got: {findings:?}");
    }

    #[test]
    fn rule_5_passes_when_source_file_is_missing() {
        // Fully purged sources satisfy redact invariant.
        let cfg = cfg_with_redact_on_forget();
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "sources/hook/purged.txt";
        let f = forgets(&[(source_id, "op-forget-9")]);
        let findings = run(&inputs_with_forgets(&cfg, &[], Some(dir.path()), &f));
        assert!(findings.is_empty(), "got: {findings:?}");
    }

    #[test]
    fn rule_5_no_op_when_redact_on_forget_disabled() {
        let cfg = CairnConfig::default();
        assert!(!cfg.vault.redact_on_forget);
        let dir = tempfile::tempdir().expect("tempdir");
        let source_id = "sources/hook/forgotten.txt";
        let source_path = dir.path().join(source_id);
        std::fs::create_dir_all(source_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&source_path, "still here").expect("write source");
        let f = forgets(&[(source_id, "op-forget-9")]);
        let findings = run(&inputs_with_forgets(&cfg, &[], Some(dir.path()), &f));
        assert!(findings.is_empty(), "got: {findings:?}");
    }

    #[test]
    fn rule_5_no_op_without_vault_root() {
        let cfg = cfg_with_redact_on_forget();
        let source_id = "sources/hook/forgotten.txt";
        let f = forgets(&[(source_id, "op-forget-9")]);
        let findings = run(&inputs_with_forgets(&cfg, &[], None, &f));
        assert!(findings.is_empty(), "got: {findings:?}");
    }

    #[test]
    fn rule_5_emits_deferred_finding_on_malformed_journal_source_id() {
        let cfg = cfg_with_redact_on_forget();
        let dir = tempfile::tempdir().expect("tempdir");
        let bad_id = "../escape.txt";
        let f = forgets(&[(bad_id, "op-forget-9")]);
        let findings = run(&inputs_with_forgets(&cfg, &[], Some(dir.path()), &f));
        let deferred: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == Kind::DeferredCheck)
            .collect();
        assert_eq!(deferred.len(), 1, "got: {findings:?}");
        assert!(
            deferred[0]
                .message
                .contains("redact_on_forget audit skipped")
        );
    }
}
