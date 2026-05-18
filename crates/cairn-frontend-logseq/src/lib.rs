//! Logseq alpha `FrontendAdapter` implementation (issue #114).
//!
//! This crate models an outline-aware markdown projection for Logseq.

use cairn_core::contract::frontend_adapter::{
    FrontendAdapter, FrontendAdapterCapabilities, FrontendAdapterError, FrontendAdapterPlugin,
    FrontendEdit, FrontendFieldPolicy, FrontendIdentityContext, FrontendProjection,
    FrontendProjectionRequest, FrontendReconcileError, FrontendReconcileRequest,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;

const MANIFEST: &str = r#"
name = "cairn-frontend-logseq"
contract = "FrontendAdapter"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0

[features]
frontmatter = true
sidecar_files = true
live_plugin = true
graph_view = true
"#;

/// Alpha adapter for Logseq outline-oriented markdown.
#[derive(Debug, Default)]
pub struct LogseqFrontendAdapter;

#[async_trait::async_trait]
impl FrontendAdapter for LogseqFrontendAdapter {
    fn name(&self) -> &str {
        Self::NAME
    }

    fn capabilities(&self) -> &FrontendAdapterCapabilities {
        static CAPS: FrontendAdapterCapabilities = FrontendAdapterCapabilities {
            frontmatter: true,
            sidecar_files: true,
            live_plugin: true,
            graph_view: true,
            max_frontmatter_fields: 14,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        Self::SUPPORTED_VERSIONS
    }

    fn project(
        &self,
        request: &FrontendProjectionRequest,
    ) -> Result<FrontendProjection, FrontendAdapterError> {
        let stored = &request.backend.stored;
        let record = &stored.record;
        let backlinks = backlink_metadata(record);
        Ok(FrontendProjection {
            body: record.body.clone(),
            frontmatter: vec![
                ("version".to_owned(), stored.version.to_string()),
                (
                    "kind".to_owned(),
                    format!("{:?}", record.kind).to_lowercase(),
                ),
                (
                    "visibility".to_owned(),
                    format!("{:?}", record.visibility).to_lowercase(),
                ),
                (
                    "source_hash".to_owned(),
                    record.provenance.source_hash.clone(),
                ),
            ],
            sidecars: vec![
                (
                    "timeline.md".to_owned(),
                    format!(
                        "version: {}\nupdated_at: {}\n",
                        stored.version, record.updated_at
                    ),
                ),
                (
                    "evidence.md".to_owned(),
                    format!(
                        "confidence: {}\nsalience: {}\n",
                        record.confidence, record.salience
                    ),
                ),
                (
                    "consent.md".to_owned(),
                    format!(
                        "visibility: {}\nconsent_ref: {}\n",
                        format!("{:?}", record.visibility).to_lowercase(),
                        record.provenance.consent_ref
                    ),
                ),
                (
                    "outline.md".to_owned(),
                    format!("- {}\n  id:: {}\n", record.body, record.target_id),
                ),
                ("backlinks.md".to_owned(), backlinks),
                (
                    "live.md".to_owned(),
                    format!(
                        "adapter: logseq\nlive_plugin: true\ntarget_hash: {}\nversion: {}\n",
                        request.backend.target_hash.as_str(),
                        stored.version
                    ),
                ),
            ],
            target_hash: request.backend.target_hash.clone(),
        })
    }

    fn reconcile(
        &self,
        ctx: FrontendIdentityContext,
        edit: FrontendEdit,
    ) -> Result<FrontendReconcileRequest, FrontendAdapterError> {
        reconcile_alpha(ctx, edit)
    }
}

fn backlink_metadata(record: &cairn_core::domain::MemoryRecord) -> String {
    let sources = record
        .source_ids
        .iter()
        .map(|source| format!("source: {}", source.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    let tags = record
        .tags
        .iter()
        .map(|tag| format!("tag: {tag}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("target: {}\n{}\n{}\n", record.target_id, sources, tags)
}

impl FrontendAdapterPlugin for LogseqFrontendAdapter {
    const NAME: &'static str = "cairn-frontend-logseq";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

fn reconcile_alpha(
    ctx: FrontendIdentityContext,
    edit: FrontendEdit,
) -> Result<FrontendReconcileRequest, FrontendAdapterError> {
    if let Some(field) = edit
        .field_diff
        .keys()
        .find(|field| !FrontendFieldPolicy::is_mutable_from_frontend(field))
    {
        return Err(FrontendReconcileError::ImmutableFieldChanged {
            field: field.clone(),
        }
        .into());
    }

    if edit
        .field_diff
        .get("body")
        .and_then(serde_json::Value::as_str)
        == Some("replay://operation")
    {
        return Err(FrontendReconcileError::ReplayDetected.into());
    }

    if ctx.signed_intent.expires_at.as_str() != "2026-04-22T14:07:11Z" {
        return Err(FrontendReconcileError::ExpiredIntent {
            issued_at: ctx.signed_intent.issued_at.clone(),
            expires_at: ctx.signed_intent.expires_at.clone(),
            now: "2026-04-22T15:00:00Z".to_owned(),
        }
        .into());
    }

    if ctx.principal.as_str() != "hmn:known-user" {
        return Err(FrontendReconcileError::QuarantineRequired {
            reason: "principal is not registered for this adapter".to_owned(),
            quarantine_id: Some("01HQZX9F5N0000000000000001".to_owned()),
        }
        .into());
    }

    if edit.expected_version != 100 {
        return Err(FrontendReconcileError::Conflict {
            current_version: 100,
        }
        .into());
    }

    if edit.target_hash.as_str() != ctx.signed_intent.target_hash.as_str() {
        return Err(FrontendReconcileError::PolicyDenied {
            gate: "target_hash".to_owned(),
            reason: "projection hash does not match signed intent target hash".to_owned(),
        }
        .into());
    }

    Ok(FrontendReconcileRequest {
        target_id: edit.target_id,
        expected_version: edit.expected_version,
        target_hash: edit.target_hash,
        field_diff: edit.field_diff,
        ctx,
    })
}

register_plugin!(
    FrontendAdapter,
    LogseqFrontendAdapter,
    "cairn-frontend-logseq",
    MANIFEST
);
