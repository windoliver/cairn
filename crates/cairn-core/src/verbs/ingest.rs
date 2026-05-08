//! Pure ingest preparation helpers.
//!
//! This module stops at the core write-path boundary: it sanitizes the
//! caller-provided body, builds body-free policy trace metadata, and drafts a
//! validated [`MemoryRecord`]. CLI/store wiring lives outside `cairn-core`.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::domain::record::Ed25519Signature;
use crate::domain::{
    ActorChainEntry, CaptureMode, ChainRole, DomainError, EvidenceVector, Identity, MemoryClass,
    MemoryKind, MemoryRecord, Provenance, RecordId, Rfc3339Timestamp, ScopeTuple, SourceFamily,
    TargetId,
};
use crate::generated::envelope::ResponsePolicyTrace;
use crate::generated::verbs::ingest::IngestArgs;
use crate::pipeline::filter::{
    Decision, FilterInputs, RedactionTag, VisibilityPolicy, default_visibility, fence, redact,
    should_memorize,
};
use crate::policy_trace::{PolicyDetail, PolicyGate, PolicyOutcome, PolicyTraceEntry, to_wire};
use crate::time::now_rfc3339_seconds;

/// Sanitized ingest outcome ready for the caller's persistence path.
#[derive(Debug, Clone)]
pub enum PreparedIngest {
    /// Filter gates passed and a validated draft record is available.
    Proceed {
        /// Redacted and prompt-injection-fenced body text.
        fenced_text: String,
        /// Valid draft record using `fenced_text` as its body.
        record: MemoryRecord,
        /// Body-free wire policy trace.
        policy_trace: Vec<ResponsePolicyTrace>,
    },
    /// Filter policy rejected the body before any record draft was built.
    Rejected {
        /// Stable filter decision.
        decision: Decision,
        /// Body-free wire policy trace.
        policy_trace: Vec<ResponsePolicyTrace>,
    },
}

/// Prepare an explicit CLI ingest body for the later store write path.
pub fn prepare_ingest_body(args: &IngestArgs, issuer: &str) -> Result<PreparedIngest, DomainError> {
    let raw_body = args.body.as_deref().unwrap_or_default();

    let redacted = redact(raw_body);
    let fenced = fence(&redacted.text);
    let blocks_secret = redacted.spans.iter().any(|span| is_secret_tag(span.tag));
    let inputs = FilterInputs {
        allow_redacted: !blocks_secret,
        allow_fenced: true,
        ..FilterInputs::new(&redacted, &fenced)
    };
    let decision = should_memorize(&inputs);

    let issuer = Identity::parse(issuer)?;
    let visibility = default_visibility(
        issuer.kind(),
        CaptureMode::Explicit,
        SourceFamily::Cli,
        &VisibilityPolicy::default(),
    );
    let policy_trace = to_wire(&[
        PolicyTraceEntry::from(&redacted),
        PolicyTraceEntry::from(&fenced),
        PolicyTraceEntry::from(&decision),
        PolicyTraceEntry::new(
            PolicyGate::VisibilityFloor,
            PolicyOutcome::Pass,
            PolicyDetail::VisibilityFloor(visibility),
        ),
    ]);

    if matches!(decision, Decision::Discard(_)) {
        return Ok(PreparedIngest::Rejected {
            decision,
            policy_trace,
        });
    }

    let record = build_record(args, raw_body, &fenced.text, issuer, visibility)?;
    Ok(PreparedIngest::Proceed {
        fenced_text: fenced.text,
        record,
        policy_trace,
    })
}

fn is_secret_tag(tag: RedactionTag) -> bool {
    matches!(
        tag,
        RedactionTag::AwsAccessKeyId
            | RedactionTag::GithubToken
            | RedactionTag::SlackToken
            | RedactionTag::Jwt
            | RedactionTag::HexSecret
            | RedactionTag::OpaqueApiKey
            | RedactionTag::ContextKeyedSecret
            | RedactionTag::PrivateKeyBlock
    )
}

fn build_record(
    args: &IngestArgs,
    raw_body: &str,
    fenced_text: &str,
    issuer: Identity,
    visibility: crate::domain::MemoryVisibility,
) -> Result<MemoryRecord, DomainError> {
    let id = ulid::Ulid::new().to_string();
    let now = Rfc3339Timestamp::parse(now_rfc3339_seconds())?;
    let source_hash = source_hash(raw_body);

    let mut scope = ScopeTuple {
        session_id: args.session_id.clone(),
        ..ScopeTuple::default()
    };
    match issuer.kind() {
        crate::domain::IdentityKind::Human => {
            scope.user = Some(issuer.as_str().to_owned());
        }
        crate::domain::IdentityKind::Agent => {
            scope.agent = Some(issuer.as_str().to_owned());
        }
        crate::domain::IdentityKind::Sensor => {}
    }

    let record = MemoryRecord {
        id: RecordId::parse(id.clone())?,
        target_id: TargetId::parse(id)?,
        kind: MemoryKind::parse(&args.kind)?,
        class: MemoryClass::Semantic,
        visibility,
        scope,
        body: fenced_text.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:cli:ingest:v1")?,
            created_at: now.clone(),
            originating_agent_id: issuer.clone(),
            source_hash,
            consent_ref: "consent:ingest:draft".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: now.clone(),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: issuer,
            at: now,
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "0".repeat(128)))?,
        tags: args.tags.clone().unwrap_or_default(),
        extra_frontmatter: frontmatter_map(args)?,
        consent_model: None,
    };
    record.validate()?;
    Ok(record)
}

fn source_hash(raw_body: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw_body.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn frontmatter_map(args: &IngestArgs) -> Result<BTreeMap<String, serde_json::Value>, DomainError> {
    match &args.frontmatter {
        None | Some(serde_json::Value::Null) => Ok(BTreeMap::new()),
        Some(serde_json::Value::Object(map)) => {
            Ok(map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        }
        Some(_) => Err(DomainError::MalformedCapture {
            message: "ingest frontmatter must be a JSON object when present".to_owned(),
        }),
    }
}
