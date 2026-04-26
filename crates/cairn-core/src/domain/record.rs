//! [`MemoryRecord`] — the typed durable record (brief §3, §6, §6.5, §4.2).
//!
//! A `MemoryRecord` is the core domain type Cairn writes, retrieves, and
//! reasons about. It is serialized three ways without re-derivation:
//!
//! - **API envelopes** — `serde_json` wire form.
//! - **`SQLite` row JSON columns** — same `serde_json` representation.
//! - **Markdown frontmatter** — YAML; the markdown projector splits the
//!   record into a `body`-less header + the body content.
//!
//! Construction does not enforce invariants on its own — call
//! [`MemoryRecord::validate`] before any [`crate::contract::MemoryStore`]
//! write so the typed errors in [`crate::domain::DomainError`] surface
//! before the WAL is touched.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::{
    ActorChainEntry, CanonicalRecordHash, ChainRole, DomainError, EvidenceVector, Identity,
    IdentityKind, Provenance, Rfc3339Timestamp, ScopeTuple, VerifiedSignedIntent,
    actor_chain::validate_chain,
    taxonomy::{MemoryClass, MemoryKind, MemoryVisibility},
};
use crate::generated::envelope::SignedIntentScopeTier;

/// Ed25519 signature in `ed25519:<128 lowercase hex>` form. Mirrors the
/// schema in `crates/cairn-idl/schema/common/primitives.json` so domain
/// signatures parse and serialize identically to wire signatures.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Ed25519Signature(String);

impl Ed25519Signature {
    /// Parse an `ed25519:<128 hex>` signature.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        let Some(tail) = raw.strip_prefix("ed25519:") else {
            return Err(DomainError::MissingSignature {
                message: "signature must start with `ed25519:`".to_owned(),
            });
        };
        if tail.len() != 128 {
            return Err(DomainError::MissingSignature {
                message: format!(
                    "signature must be `ed25519:` + exactly 128 hex chars (got {} hex chars)",
                    tail.len()
                ),
            });
        }
        if !tail.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(DomainError::MissingSignature {
                message: "signature hex tail must be lowercase 0-9 a-f".to_owned(),
            });
        }
        Ok(Self(raw))
    }

    /// Wire-form signature string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Ed25519Signature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

/// ULID-typed record id. 26 chars, Crockford base32, uppercase, no `I L O U`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct RecordId(String);

impl RecordId {
    /// Parse a wire-form ULID.
    ///
    /// Length is 26, alphabet is Crockford base32 (uppercase, no `I L O
    /// U`), and the first character is bounded to `[0..=7]` because a
    /// ULID encodes a 128-bit integer and the high 5 bits of the leading
    /// Crockford symbol must be zero — otherwise the value overflows
    /// 128 bits and downstream ULID decoders will misorder or reject it.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        if raw.len() != 26 {
            return Err(DomainError::EmptyField { field: "record_id" });
        }
        let bytes = raw.as_bytes();
        if !matches!(bytes[0], b'0'..=b'7') {
            return Err(DomainError::EmptyField { field: "record_id" });
        }
        if !bytes[1..].iter().all(|b| {
            matches!(b,
                b'0'..=b'9'
                | b'A'..=b'H'
                | b'J'
                | b'K'
                | b'M'
                | b'N'
                | b'P'..=b'T'
                | b'V'..=b'Z')
        }) {
            return Err(DomainError::EmptyField { field: "record_id" });
        }
        Ok(Self(raw))
    }

    /// Underlying ULID string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RecordId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

/// The typed durable memory record.
///
/// Field ordering of this struct *is the wire ordering* — `serde` emits
/// fields in declaration order, which means JSON / YAML / `SQLite` rows all
/// agree on canonical key order. Adapters should call [`Self::validate`]
/// before any persistence side effect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecord {
    /// ULID — the stable record identifier.
    pub id: RecordId,
    /// Memory kind (§6.1).
    pub kind: MemoryKind,
    /// Memory class (§6.2).
    pub class: MemoryClass,
    /// Visibility tier (§6.3). Default for new records is `private` or
    /// `session` per kind config — domain validation does not enforce that
    /// default.
    pub visibility: MemoryVisibility,
    /// Scope tuple (§6, §4.2). At least one dimension must be set.
    pub scope: ScopeTuple,
    /// Markdown body. Required and non-empty.
    pub body: String,
    /// Mandatory provenance frontmatter (§6.5).
    pub provenance: Provenance,
    /// Wall-clock instant of the most recent durable update.
    pub updated_at: Rfc3339Timestamp,
    /// Evidence vector (§6.4).
    pub evidence: EvidenceVector,
    /// Salience scalar in `[0.0, 1.0]`.
    pub salience: f32,
    /// Confidence scalar in `[0.0, 1.0]`. Banding lives in
    /// [`crate::domain::ConfidenceBand::from_scalar`].
    pub confidence: f32,
    /// Actor chain (§4.2). At minimum: one `author` entry.
    pub actor_chain: Vec<ActorChainEntry>,
    /// Author signature over the canonical record bytes.
    pub signature: Ed25519Signature,
    /// Tags (free-form). Empty by default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Extra YAML/JSON frontmatter the ingest call carried (§ schema
    /// `verbs/ingest.json`). Stored verbatim; ordered for deterministic
    /// re-emission via `BTreeMap`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_frontmatter: BTreeMap<String, serde_json::Value>,
}

impl MemoryRecord {
    /// Validate every domain invariant. Returns the first violation found.
    ///
    /// This is **shape validation only** — it confirms the record is
    /// well-formed (provenance present, identity refs parse, scope is
    /// non-empty, visibility/kind/class are recognized, evidence and
    /// scalar ranges hold, signature has the right wire form). It does
    /// **not** verify the cryptographic signature against the author's
    /// key material; that check belongs to the store boundary where
    /// keychain-resident keys are available (brief §4.2 "Signature-first
    /// rejection"). A successful return from `validate` means the record
    /// is *eligible* for crypto verification, not that it has been
    /// verified.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.body.is_empty() {
            return Err(DomainError::EmptyField { field: "body" });
        }
        if self.id.as_str().is_empty() {
            return Err(DomainError::EmptyField { field: "id" });
        }
        self.scope.validate()?;
        self.provenance.validate()?;
        self.evidence.validate()?;
        if !(0.0..=1.0).contains(&self.salience) || self.salience.is_nan() {
            return Err(DomainError::OutOfRange {
                field: "salience",
                message: format!("must be in [0.0, 1.0], was {}", self.salience),
            });
        }
        if !(0.0..=1.0).contains(&self.confidence) || self.confidence.is_nan() {
            return Err(DomainError::OutOfRange {
                field: "confidence",
                message: format!("must be in [0.0, 1.0], was {}", self.confidence),
            });
        }
        validate_chain(&self.actor_chain)?;
        self.validate_p0_chain_shape()?;
        self.validate_sensor_consistency()?;
        self.validate_actor_scope_consistency()?;
        self.validate_temporal_invariants()?;
        for tag in &self.tags {
            if tag.is_empty() {
                return Err(DomainError::EmptyField { field: "tag" });
            }
        }
        Ok(())
    }

    /// Cross-field check: the sensor that captured the source bytes
    /// (`provenance.source_sensor`) must agree with what the actor chain
    /// claims about sensor involvement. Otherwise downstream policy may
    /// trust the provenance sensor while the signature only proves a
    /// different actor authored the record.
    ///
    /// Rules:
    /// - `MemoryKind::SensorObservation` records must be attributable to
    ///   `provenance.source_sensor` via the chain — either the author *is*
    ///   that sensor, or an explicit `Sensor` role entry naming that
    ///   sensor is present. Otherwise the signature does not prove the
    ///   sensor in `provenance.source_sensor` had any role in the write.
    /// - When the chain author is a `Sensor`, that sensor identity must
    ///   equal `provenance.source_sensor`.
    /// - When the chain has explicit `Sensor` role entries (any kind of
    ///   record), `provenance.source_sensor` must match one of them.
    fn validate_sensor_consistency(&self) -> Result<(), DomainError> {
        let author = self
            .actor_chain
            .iter()
            .find(|e| e.role == ChainRole::Author);
        if let Some(author) = author
            && author.identity.kind() == IdentityKind::Sensor
            && author.identity != self.provenance.source_sensor
        {
            return Err(DomainError::InvalidIdentity {
                message: format!(
                    "sensor-authored record: chain author `{}` does not match provenance.source_sensor `{}`",
                    author.identity.as_str(),
                    self.provenance.source_sensor.as_str()
                ),
            });
        }

        // Every `Sensor` chain entry must equal `provenance.source_sensor`.
        // Provenance is single-source (one `source_sensor`, one
        // `source_hash`) so any extra sensor identity in the chain would
        // be unattributed and potentially treated as a co-capturer by
        // downstream policy.
        for entry in self
            .actor_chain
            .iter()
            .filter(|e| e.role == ChainRole::Sensor)
        {
            if entry.identity != self.provenance.source_sensor {
                return Err(DomainError::InvalidIdentity {
                    message: format!(
                        "actor_chain sensor entry `{}` does not equal provenance.source_sensor `{}` (provenance is single-source until multi-sensor records are modeled)",
                        entry.identity.as_str(),
                        self.provenance.source_sensor.as_str()
                    ),
                });
            }
        }
        // Bidirectional sensor-author invariant:
        //   - `SensorObservation` records *must* have a sensor author equal
        //     to `provenance.source_sensor` (otherwise the signature does
        //     not prove sensor participation; unsigned `Sensor` chain
        //     entries are claims, not proof, until P2 countersignatures).
        //   - Sensor authors are *only* legal for `SensorObservation`. A
        //     sensor key has narrow trust (raw event capture); allowing it
        //     to author derived kinds like `Rule`, `Fact`, or `Reasoning`
        //     would let a low-trust signer mint high-trust memories.
        let author_is_sensor =
            matches!(author, Some(a) if a.identity.kind() == IdentityKind::Sensor);
        match self.kind {
            MemoryKind::SensorObservation => {
                let author_is_source =
                    matches!(author, Some(a) if a.identity == self.provenance.source_sensor);
                if !author_is_source {
                    return Err(DomainError::InvalidIdentity {
                        message: format!(
                            "sensor_observation record must have author == provenance.source_sensor `{}` (unsigned `sensor` chain entries do not prove sensor participation until P2 countersignatures land)",
                            self.provenance.source_sensor.as_str()
                        ),
                    });
                }
            }
            other if author_is_sensor => {
                return Err(DomainError::InvalidIdentity {
                    message: format!(
                        "sensor identities may only author `sensor_observation` records, not `{}` (derived kinds need a human or agent author)",
                        other.as_str()
                    ),
                });
            }
            _ => {}
        }
        Ok(())
    }

    /// Pre-store containment check: the record's scope and visibility
    /// must be contained by the [`VerifiedSignedIntent`] that authorizes
    /// the write. `validate()` alone is shape-only; the store boundary
    /// must call this method as well so a record signed for one
    /// `(tenant, workspace, entity, tier)` cannot be persisted into
    /// another.
    ///
    /// Containment rules:
    /// - The canonical signed-payload hash of `self` must equal
    ///   `intent.target_hash` — the binding is recomputed from the
    ///   record on every call, so a stale hash from an older record
    ///   instance cannot pass even if the caller still holds it.
    /// - `scope.tenant`, `scope.workspace`, `scope.entity` (when set) must
    ///   exactly equal the corresponding `SignedIntent.scope` field.
    /// - `visibility` must be `≤` `SignedIntent.scope.tier` — promotion
    ///   to a tier broader than the signed authorization is rejected.
    /// - `scope.session_id` is **not yet in the signed envelope** — set it
    ///   on a record and containment rejects the write. Once
    ///   `SignedIntentScope` grows a `session_id` field this check will
    ///   compare them.
    /// - `scope.user`, when set, must equal a cryptographically-established
    ///   human identity: the chain author (when human) or `intent.issuer`
    ///   (when human). The unsigned chain entries at P0 are not sufficient.
    /// - `scope.agent`, when set, must equal the chain author (when agent)
    ///   or `intent.issuer` (when agent).
    ///
    /// The `intent` is consumed as a [`VerifiedSignedIntent`] so the type
    /// system enforces "the upstream verifier already checked signature,
    /// expiry, nonce, and sequence". This method does **not** re-derive
    /// crypto truth.
    pub fn validate_against_intent(
        &self,
        intent: &VerifiedSignedIntent,
    ) -> Result<(), DomainError> {
        let intent = intent.as_inner();
        let computed = CanonicalRecordHash::compute(self)?;
        if computed.as_str() != intent.target_hash {
            return Err(DomainError::MissingSignature {
                message: format!(
                    "canonical record hash `{}` does not match SignedIntent.target_hash `{}` — the signed intent is not bound to this record",
                    computed.as_str(),
                    intent.target_hash
                ),
            });
        }
        if self.scope.session_id.is_some() {
            return Err(DomainError::MalformedScope {
                message: "scope.session_id is set, but the signed intent envelope has no session dimension yet — containment cannot authorize a session-scoped write".to_owned(),
            });
        }
        // The signed intent's scope (tenant/workspace/entity/tier) is the
        // authorization envelope. The record must *explicitly* carry the
        // same tenant/workspace/entity — omission would let a write into
        // a narrower record scope satisfy a broader intent and pollute
        // governance/retrieval boundaries downstream.
        let record_tenant =
            self.scope
                .tenant
                .as_deref()
                .ok_or_else(|| DomainError::MalformedScope {
                    message: format!(
                        "scope.tenant must be set to `{}` to match SignedIntent.scope.tenant",
                        intent.scope.tenant
                    ),
                })?;
        if record_tenant != intent.scope.tenant {
            return Err(DomainError::MalformedScope {
                message: format!(
                    "scope.tenant `{record_tenant}` does not match SignedIntent.scope.tenant `{}`",
                    intent.scope.tenant
                ),
            });
        }
        let record_workspace =
            self.scope
                .workspace
                .as_deref()
                .ok_or_else(|| DomainError::MalformedScope {
                    message: format!(
                        "scope.workspace must be set to `{}` to match SignedIntent.scope.workspace",
                        intent.scope.workspace
                    ),
                })?;
        if record_workspace != intent.scope.workspace {
            return Err(DomainError::MalformedScope {
                message: format!(
                    "scope.workspace `{record_workspace}` does not match SignedIntent.scope.workspace `{}`",
                    intent.scope.workspace
                ),
            });
        }
        let record_entity =
            self.scope
                .entity
                .as_deref()
                .ok_or_else(|| DomainError::MalformedScope {
                    message: format!(
                        "scope.entity must be set to `{}` to match SignedIntent.scope.entity",
                        intent.scope.entity
                    ),
                })?;
        if record_entity != intent.scope.entity {
            return Err(DomainError::MalformedScope {
                message: format!(
                    "scope.entity `{record_entity}` does not match SignedIntent.scope.entity `{}`",
                    intent.scope.entity
                ),
            });
        }
        let intent_tier = intent_tier_to_visibility(intent.scope.tier);
        if self.visibility > intent_tier {
            return Err(DomainError::UnsupportedVisibility {
                value: format!(
                    "record visibility `{:?}` exceeds SignedIntent tier `{:?}`",
                    self.visibility, intent_tier
                ),
            });
        }
        self.validate_scope_principals_against_intent(intent)
    }

    /// scope.user / scope.agent must bind to a cryptographically established
    /// identity — chain author (signed at P0) or `intent.issuer` (signed by
    /// the issuing key). Without this, an agent author could persist
    /// `scope.user = usr:victim` and have it ride the intent's authorization
    /// onto downstream consumers that filter by user scope.
    fn validate_scope_principals_against_intent(
        &self,
        intent: &crate::generated::envelope::SignedIntent,
    ) -> Result<(), DomainError> {
        let issuer = intent.issuer.0.as_str();
        let author_entry = self
            .actor_chain
            .iter()
            .find(|e| e.role == ChainRole::Author);
        let author = author_entry.map_or("", |e| e.identity.as_str());
        let author_kind = author_entry.map(|e| e.identity.kind());
        if let Some(user) = self.scope.user.as_deref() {
            let author_matches = author_kind == Some(IdentityKind::Human) && user == author;
            let issuer_matches = issuer.starts_with("usr:") && user == issuer;
            if !(author_matches || issuer_matches) {
                return Err(DomainError::MalformedScope {
                    message: format!(
                        "scope.user `{user}` is not cryptographically established — must equal the human chain author or SignedIntent.issuer (got author=`{author}`, issuer=`{issuer}`)"
                    ),
                });
            }
        }
        if let Some(agent) = self.scope.agent.as_deref() {
            let author_matches = author_kind == Some(IdentityKind::Agent) && agent == author;
            let issuer_matches = issuer.starts_with("agt:") && agent == issuer;
            if !(author_matches || issuer_matches) {
                return Err(DomainError::MalformedScope {
                    message: format!(
                        "scope.agent `{agent}` is not cryptographically established — must equal the agent chain author or SignedIntent.issuer (got author=`{author}`, issuer=`{issuer}`)"
                    ),
                });
            }
        }
        Ok(())
    }

    /// At P0 only the author signs the record — principal, delegator, and
    /// `Sensor` chain entries are unsigned claims, not verified provenance.
    /// Until per-entry countersignatures land at P2 (brief §4.2
    /// "Countersignatures") downstream audit, ranking, and consent flows
    /// would have no way to distinguish a real principal/delegator from an
    /// arbitrary string an attacker added. So at P0 the chain is allowed
    /// to contain *only* the single `Author` entry.
    fn validate_p0_chain_shape(&self) -> Result<(), DomainError> {
        for entry in &self.actor_chain {
            if entry.role != ChainRole::Author {
                return Err(DomainError::MissingSignature {
                    message: format!(
                        "actor_chain entry with role `{:?}` is not allowed at P0 — only the `Author` role is signed; principal/delegator/sensor entries become valid once P2 countersignatures are modeled",
                        entry.role
                    ),
                });
            }
        }
        Ok(())
    }

    /// Shape-only check on `scope.user` / `scope.agent` and the
    /// `provenance.originating_agent_id` ↔ author binding.
    ///
    /// `scope.user` / `scope.agent` must be canonical *full* identity
    /// strings (`usr:tafeng`, `agt:claude-code:opus-4-7:main:v1`) so the
    /// IDL filter sees the same string a query uses. This catches
    /// kind/format mistakes early.
    ///
    /// **Identity binding** (scope value matches a cryptographically
    /// established principal — author or `SignedIntent.issuer`) lives in
    /// [`MemoryRecord::validate_against_intent`]; without an intent, validate alone
    /// cannot prove the user/agent claim. Stores **must** call both:
    /// validate alone is insufficient to authorize a write.
    ///
    /// At P0 `provenance.originating_agent_id` must equal the signing
    /// author — the originator is the signer until P2 countersignatures
    /// land (brief §4.2). Cross-kind delegation (`agent author` writing
    /// on behalf of a human) is expressed via `scope.user` plus a signed
    /// intent issued by that human, not by setting `originating_agent_id`
    /// to a different identity.
    fn validate_actor_scope_consistency(&self) -> Result<(), DomainError> {
        let Some(author) = self
            .actor_chain
            .iter()
            .find(|e| e.role == ChainRole::Author)
        else {
            // Caught earlier by `validate_chain`; reachable only if
            // someone bypassed that step.
            return Ok(());
        };

        if let Some(user) = self.scope.user.as_deref() {
            let parsed =
                Identity::parse(user.to_owned()).map_err(|_| DomainError::MalformedScope {
                    message: format!(
                        "scope.user `{user}` is not a canonical identity (full `usr:` form required)"
                    ),
                })?;
            if parsed.kind() != IdentityKind::Human {
                return Err(DomainError::MalformedScope {
                    message: format!("scope.user `{user}` must be a human (`usr:`) identity"),
                });
            }
        }
        if let Some(agent) = self.scope.agent.as_deref() {
            let parsed =
                Identity::parse(agent.to_owned()).map_err(|_| DomainError::MalformedScope {
                    message: format!(
                        "scope.agent `{agent}` is not a canonical identity (full `agt:` form required)"
                    ),
                })?;
            if parsed.kind() != IdentityKind::Agent {
                return Err(DomainError::MalformedScope {
                    message: format!("scope.agent `{agent}` must be an agent (`agt:`) identity"),
                });
            }
        }
        if self.provenance.originating_agent_id != author.identity {
            return Err(DomainError::InvalidIdentity {
                message: format!(
                    "provenance.originating_agent_id `{}` does not match the signing author `{}` (P0: delegation requires P2 countersignatures)",
                    self.provenance.originating_agent_id.as_str(),
                    author.identity.as_str()
                ),
            });
        }
        Ok(())
    }

    fn validate_temporal_invariants(&self) -> Result<(), DomainError> {
        let created = epoch_with_nanos(self.provenance.created_at.as_str())?;
        let updated = epoch_with_nanos(self.updated_at.as_str())?;
        if created > updated {
            return Err(DomainError::InvalidTimestamp {
                message: format!(
                    "provenance.created_at `{}` is after updated_at `{}`",
                    self.provenance.created_at.as_str(),
                    self.updated_at.as_str()
                ),
            });
        }
        for entry in &self.actor_chain {
            let at = epoch_with_nanos(entry.at.as_str())?;
            if at > updated {
                return Err(DomainError::InvalidTimestamp {
                    message: format!(
                        "actor_chain entry `at` ({}) is after updated_at ({})",
                        entry.at.as_str(),
                        self.updated_at.as_str()
                    ),
                });
            }
        }
        Ok(())
    }
}

/// Convert a validated RFC3339 timestamp string to UTC `(epoch_seconds,
/// nanos)` for ordering with subsecond precision.
///
/// Cheap parser used only for ordering inside [`MemoryRecord::validate`];
/// the input has already passed [`Rfc3339Timestamp::parse`] so range checks
/// here are belt-and-braces. We avoid `chrono`/`time` to keep `cairn-core`
/// dep-free.
fn epoch_with_nanos(raw: &str) -> Result<(i64, u32), DomainError> {
    let bytes = raw.as_bytes();
    let invalid = || DomainError::InvalidTimestamp {
        message: format!("`{raw}`: cannot parse for ordering"),
    };

    if bytes.len() < 20 {
        return Err(invalid());
    }
    let year: i64 = parse_int(&bytes[..4]).ok_or_else(invalid)?;
    let month: i64 = parse_int(&bytes[5..7]).ok_or_else(invalid)?;
    let day: i64 = parse_int(&bytes[8..10]).ok_or_else(invalid)?;
    let hour: i64 = parse_int(&bytes[11..13]).ok_or_else(invalid)?;
    let minute: i64 = parse_int(&bytes[14..16]).ok_or_else(invalid)?;
    let second: i64 = parse_int(&bytes[17..19]).ok_or_else(invalid)?;

    let mut idx = 19;
    let mut nanos: u32 = 0;
    if idx < bytes.len() && bytes[idx] == b'.' {
        idx += 1;
        let frac_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        // Pad / truncate to 9 digits for nanoseconds.
        let mut acc: u64 = 0;
        let mut count = 0;
        for &b in &bytes[frac_start..idx] {
            if count >= 9 {
                break;
            }
            acc = acc * 10 + u64::from(b - b'0');
            count += 1;
        }
        while count < 9 {
            acc *= 10;
            count += 1;
        }
        nanos = u32::try_from(acc).map_err(|_| invalid())?;
    }
    let offset_seconds: i64 = match bytes.get(idx) {
        Some(b'Z' | b'z') => 0,
        Some(b'+' | b'-') => {
            let sign: i64 = if bytes[idx] == b'-' { -1 } else { 1 };
            let oh: i64 = parse_int(&bytes[idx + 1..idx + 3]).ok_or_else(invalid)?;
            let om: i64 = parse_int(&bytes[idx + 4..idx + 6]).ok_or_else(invalid)?;
            sign * (oh * 3600 + om * 60)
        }
        _ => return Err(invalid()),
    };

    let days = days_from_civil(year, month, day);
    let local = days * 86_400 + hour * 3600 + minute * 60 + second;
    Ok((local - offset_seconds, nanos))
}

fn intent_tier_to_visibility(t: SignedIntentScopeTier) -> MemoryVisibility {
    match t {
        SignedIntentScopeTier::Private => MemoryVisibility::Private,
        SignedIntentScopeTier::Session => MemoryVisibility::Session,
        SignedIntentScopeTier::Project => MemoryVisibility::Project,
        SignedIntentScopeTier::Team => MemoryVisibility::Team,
        SignedIntentScopeTier::Org => MemoryVisibility::Org,
        SignedIntentScopeTier::Public => MemoryVisibility::Public,
    }
}

fn parse_int(bytes: &[u8]) -> Option<i64> {
    if !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut acc: i64 = 0;
    for b in bytes {
        acc = acc * 10 + i64::from(b - b'0');
    }
    Some(acc)
}

/// Days since 1970-01-01 for a (proleptic Gregorian) civil date. Algorithm:
/// Howard Hinnant, *date.h* — `days_from_civil`.
const fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ActorChainEntry, ChainRole, Identity};

    pub(crate) fn sample_record() -> MemoryRecord {
        // Single human author at P0: scope.user, originating_agent_id, and
        // chain author all bind to `usr:tafeng`. Delegation chains arrive
        // with P2 countersignatures.
        let user_id = Identity::parse("usr:tafeng").expect("valid");
        MemoryRecord {
            id: RecordId::parse("01HQZX9F5N0000000000000000").expect("valid"),
            kind: MemoryKind::User,
            class: MemoryClass::Semantic,
            visibility: MemoryVisibility::Private,
            scope: ScopeTuple {
                user: Some("usr:tafeng".to_owned()),
                ..ScopeTuple::default()
            },
            body: "user prefers dark mode".to_owned(),
            provenance: Provenance {
                source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
                created_at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
                originating_agent_id: user_id.clone(),
                source_hash: format!("sha256:{}", "a".repeat(64)),
                consent_ref: "consent:01HQZ".to_owned(),
                llm_id_if_any: None,
            },
            updated_at: Rfc3339Timestamp::parse("2026-04-22T14:05:11Z").expect("valid"),
            evidence: EvidenceVector::default(),
            salience: 0.5,
            confidence: 0.7,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: user_id,
                at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
            }],
            signature: Ed25519Signature::parse(format!("ed25519:{}", "a".repeat(128)))
                .expect("valid"),
            tags: vec!["pref".to_owned()],
            extra_frontmatter: BTreeMap::new(),
        }
    }

    #[test]
    fn valid_record_passes_validation() {
        sample_record().validate().expect("valid");
    }

    #[test]
    fn empty_body_rejected() {
        let mut r = sample_record();
        r.body.clear();
        let err = r.validate().unwrap_err();
        assert_eq!(err, DomainError::EmptyField { field: "body" });
    }

    #[test]
    fn empty_scope_rejected() {
        let mut r = sample_record();
        r.scope = ScopeTuple::default();
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::MalformedScope { .. }));
    }

    #[test]
    fn out_of_range_confidence_rejected() {
        let mut r = sample_record();
        r.confidence = 1.5;
        let err = r.validate().unwrap_err();
        assert!(matches!(
            err,
            DomainError::OutOfRange {
                field: "confidence",
                ..
            }
        ));
    }

    #[test]
    fn out_of_range_salience_rejected() {
        let mut r = sample_record();
        r.salience = -0.1;
        let err = r.validate().unwrap_err();
        assert!(matches!(
            err,
            DomainError::OutOfRange {
                field: "salience",
                ..
            }
        ));
    }

    #[test]
    fn missing_author_rejected() {
        let mut r = sample_record();
        r.actor_chain.clear();
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::MissingSignature { .. }));
    }

    #[test]
    fn bad_signature_rejected_at_parse() {
        let err = Ed25519Signature::parse("notasig").unwrap_err();
        assert!(matches!(err, DomainError::MissingSignature { .. }));
    }

    #[test]
    fn json_round_trip_preserves_all_fields() {
        let r = sample_record();
        let s = serde_json::to_string(&r).expect("ser");
        let back: MemoryRecord = serde_json::from_str(&s).expect("de");
        assert_eq!(r, back);
    }

    #[test]
    fn sensor_authored_record_must_match_provenance() {
        let mut r = sample_record();
        // Sensor authors are only valid for SensorObservation.
        r.kind = MemoryKind::SensorObservation;
        let sensor =
            Identity::parse("snr:local:hook:cc-session:v1").expect("valid sensor identity");
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: sensor.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }];
        r.provenance.source_sensor = sensor.clone();
        r.provenance.originating_agent_id = sensor.clone();
        r.scope = ScopeTuple {
            entity: Some("camera-4".to_owned()),
            ..ScopeTuple::default()
        };
        r.validate().expect("matched sensor author + provenance");

        // Now flip provenance to a different sensor.
        r.provenance.source_sensor =
            Identity::parse("snr:local:hook:other:v1").expect("valid sensor identity");
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::InvalidIdentity { .. }));
    }

    #[test]
    fn sensor_observation_requires_sensor_author() {
        let mut r = sample_record();
        r.kind = MemoryKind::SensorObservation;
        // Default sample has agent author — invalid for SensorObservation.
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::InvalidIdentity { .. }));

        // Adding a sensor chain entry naming source_sensor is NOT enough at
        // P0 — the P0 chain-shape rule rejects any non-Author role
        // (sensor entries are unsigned attestations until P2).
        r.actor_chain.push(ActorChainEntry {
            role: ChainRole::Sensor,
            identity: r.provenance.source_sensor.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        });
        let err = r.validate().unwrap_err();
        assert!(
            matches!(
                err,
                DomainError::MissingSignature { .. } | DomainError::InvalidIdentity { .. }
            ),
            "unsigned sensor entry must not be sufficient for sensor_observation"
        );

        // Make the sensor the author → valid (after aligning scope and
        // originating_agent_id with the sensor-only chain).
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: r.provenance.source_sensor.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }];
        r.provenance.originating_agent_id = r.provenance.source_sensor.clone();
        r.scope = ScopeTuple {
            entity: Some("camera-4".to_owned()),
            ..ScopeTuple::default()
        };
        r.validate()
            .expect("sensor-as-author is the only valid sensor_observation shape");
    }

    #[test]
    fn sensor_chain_entry_rejected_at_p0() {
        // Sensor role entries are unsigned at P0 → rejected by the P0
        // chain-shape rule before sensor consistency runs. (When P2
        // countersignatures land, the sensor-entry-equals-source_sensor
        // check inside validate_sensor_consistency takes over.)
        let mut r = sample_record();
        r.actor_chain.push(ActorChainEntry {
            role: ChainRole::Sensor,
            identity: Identity::parse("snr:local:hook:other:v1").expect("valid"),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        });
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::MissingSignature { .. }));
    }

    fn intent_for(
        record: &MemoryRecord,
        tenant: &str,
        workspace: &str,
        entity: &str,
        tier: SignedIntentScopeTier,
    ) -> VerifiedSignedIntent {
        // target_hash binds to the canonical hash of the actual record
        // under test, so tests exercising other rules (scope mismatch,
        // visibility promotion, etc.) get a matching hash by default.
        let target_hash = CanonicalRecordHash::compute(record).expect("compute");
        VerifiedSignedIntent::dangerous_unverified_for_testing(
            crate::generated::envelope::SignedIntent {
                chain_parents: vec![],
                expires_at: "2026-04-22T14:07:11Z".to_owned(),
                issued_at: "2026-04-22T14:02:11Z".to_owned(),
                issuer: crate::generated::common::Identity("usr:tafeng".to_owned()),
                key_version: 1,
                nonce: crate::generated::common::Nonce16Base64(
                    "AAAAAAAAAAAAAAAAAAAAAA==".to_owned(),
                ),
                operation_id: crate::generated::common::Ulid(
                    "01HQZX9F5N0000000000000000".to_owned(),
                ),
                scope: crate::generated::envelope::SignedIntentScope {
                    tenant: tenant.to_owned(),
                    workspace: workspace.to_owned(),
                    entity: entity.to_owned(),
                    tier,
                },
                sequence: Some(1),
                server_challenge: None,
                signature: crate::generated::common::Ed25519Signature(format!(
                    "ed25519:{}",
                    "a".repeat(128)
                )),
                target_hash: target_hash.as_str().to_owned(),
            },
        )
    }

    #[test]
    fn intent_containment_rejects_tenant_mismatch() {
        let mut r = sample_record();
        r.scope.tenant = Some("acme".to_owned());
        let intent = intent_for(&r, "other", "ws", "ent", SignedIntentScopeTier::Project);
        let err = r.validate_against_intent(&intent).unwrap_err();
        assert!(matches!(err, DomainError::MalformedScope { .. }));
    }

    #[test]
    fn intent_containment_rejects_visibility_promotion() {
        let mut r = sample_record();
        r.scope.tenant = Some("acme".to_owned());
        r.scope.workspace = Some("ws".to_owned());
        r.scope.entity = Some("ent".to_owned());
        r.visibility = MemoryVisibility::Team;
        let intent = intent_for(&r, "acme", "ws", "ent", SignedIntentScopeTier::Project);
        let err = r.validate_against_intent(&intent).unwrap_err();
        assert!(matches!(err, DomainError::UnsupportedVisibility { .. }));
    }

    #[test]
    fn intent_containment_rejects_unset_tenant() {
        let mut r = sample_record();
        r.scope.tenant = None;
        r.scope.workspace = Some("ws".to_owned());
        r.scope.entity = Some("ent".to_owned());
        let intent = intent_for(&r, "acme", "ws", "ent", SignedIntentScopeTier::Project);
        let err = r.validate_against_intent(&intent).unwrap_err();
        assert!(matches!(err, DomainError::MalformedScope { .. }));
    }

    #[test]
    fn intent_containment_rejects_unset_workspace() {
        let mut r = sample_record();
        r.scope.tenant = Some("acme".to_owned());
        r.scope.workspace = None;
        r.scope.entity = Some("ent".to_owned());
        let intent = intent_for(&r, "acme", "ws", "ent", SignedIntentScopeTier::Project);
        let err = r.validate_against_intent(&intent).unwrap_err();
        assert!(matches!(err, DomainError::MalformedScope { .. }));
    }

    #[test]
    fn intent_containment_rejects_unset_entity() {
        let mut r = sample_record();
        r.scope.tenant = Some("acme".to_owned());
        r.scope.workspace = Some("ws".to_owned());
        r.scope.entity = None;
        let intent = intent_for(&r, "acme", "ws", "ent", SignedIntentScopeTier::Project);
        let err = r.validate_against_intent(&intent).unwrap_err();
        assert!(matches!(err, DomainError::MalformedScope { .. }));
    }

    #[test]
    fn intent_containment_accepts_matching_scope() {
        let mut r = sample_record();
        r.scope.tenant = Some("acme".to_owned());
        r.scope.workspace = Some("ws".to_owned());
        r.scope.entity = Some("ent".to_owned());
        r.visibility = MemoryVisibility::Private;
        let intent = intent_for(&r, "acme", "ws", "ent", SignedIntentScopeTier::Project);
        r.validate_against_intent(&intent)
            .expect("scope contained, visibility ≤ tier, target hash matches");
    }

    #[test]
    fn intent_containment_rejects_target_hash_mismatch() {
        // The signed intent binds to a specific record content via
        // `target_hash`. Mutating any signed field after the intent was
        // issued must invalidate the binding.
        let mut r = sample_record();
        r.scope.tenant = Some("acme".to_owned());
        r.scope.workspace = Some("ws".to_owned());
        r.scope.entity = Some("ent".to_owned());
        // Capture the intent BEFORE mutating the body, then mutate.
        let intent = intent_for(&r, "acme", "ws", "ent", SignedIntentScopeTier::Project);
        r.body.push_str(" (tampered)");
        let err = r.validate_against_intent(&intent).unwrap_err();
        assert!(matches!(err, DomainError::MissingSignature { .. }));
    }

    #[test]
    fn intent_containment_rejects_session_scope() {
        // SignedIntentScope has no session dimension yet; until it does,
        // a session-scoped record cannot be authorized by intent
        // containment — fail closed.
        let mut r = sample_record();
        r.scope.tenant = Some("acme".to_owned());
        r.scope.workspace = Some("ws".to_owned());
        r.scope.entity = Some("ent".to_owned());
        r.scope.session_id = Some("session-42".to_owned());
        let intent = intent_for(&r, "acme", "ws", "ent", SignedIntentScopeTier::Project);
        let err = r.validate_against_intent(&intent).unwrap_err();
        assert!(matches!(err, DomainError::MalformedScope { .. }));
    }

    #[test]
    fn intent_containment_accepts_user_scope_via_intent_issuer() {
        // Agent-authored record about a user: scope.user binds to the
        // human intent issuer, who cryptographically authorized the
        // write. This is the legitimate "agent acts on behalf of user"
        // path that homogeneous-author validation alone cannot express.
        let agent = Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("valid");
        let mut r = sample_record();
        r.scope.tenant = Some("acme".to_owned());
        r.scope.workspace = Some("ws".to_owned());
        r.scope.entity = Some("ent".to_owned());
        r.scope.user = Some("usr:tafeng".to_owned());
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: agent.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }];
        r.provenance.originating_agent_id = agent;
        let intent = intent_for(&r, "acme", "ws", "ent", SignedIntentScopeTier::Project);
        r.validate_against_intent(&intent)
            .expect("agent-authored user memory authorized by user-issued intent");
    }

    #[test]
    fn intent_containment_rejects_user_scope_without_matching_principal() {
        let agent = Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("valid");
        let mut r = sample_record();
        r.scope.tenant = Some("acme".to_owned());
        r.scope.workspace = Some("ws".to_owned());
        r.scope.entity = Some("ent".to_owned());
        r.scope.user = Some("usr:victim".to_owned());
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: agent.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }];
        r.provenance.originating_agent_id = agent;
        let intent = intent_for(&r, "acme", "ws", "ent", SignedIntentScopeTier::Project);
        let err = r.validate_against_intent(&intent).unwrap_err();
        assert!(matches!(err, DomainError::MalformedScope { .. }));
    }

    #[test]
    fn record_id_rejects_overflow_first_char() {
        let err = RecordId::parse("8ZZZZZZZZZZZZZZZZZZZZZZZZZ").unwrap_err();
        assert!(matches!(
            err,
            DomainError::EmptyField { field: "record_id" }
        ));
    }

    #[test]
    fn record_id_accepts_max_valid_first_char() {
        RecordId::parse("7ZZZZZZZZZZZZZZZZZZZZZZZZZ").expect("first char `7` is the max valid");
    }

    #[test]
    fn p0_chain_rejects_non_author_roles() {
        // Even when scope/originator bind to the author, P0 chains must
        // not contain unsigned principal/delegator/sensor entries — they
        // would be exposed as provenance to downstream code with no
        // signature backing them.
        let mut r = sample_record();
        r.actor_chain.insert(
            0,
            ActorChainEntry {
                role: ChainRole::Principal,
                identity: Identity::parse("usr:tafeng").expect("valid"),
                at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
            },
        );
        // Re-align the chain to keep the only signed entry the author —
        // but the unsigned principal must still reject.
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::MissingSignature { .. }));
    }

    #[test]
    fn scope_agent_accepts_full_identity() {
        let mut r = sample_record();
        // Re-author as an agent and use the full `agt:...` form for
        // scope.agent.
        let agent = Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("valid");
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: agent.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }];
        r.provenance.originating_agent_id = agent.clone();
        r.scope = ScopeTuple {
            agent: Some(agent.as_str().to_owned()),
            ..ScopeTuple::default()
        };
        r.validate()
            .expect("full agt: identity accepted as scope.agent");
    }

    #[test]
    fn scope_agent_rejects_bare_body() {
        // Canonical scope encoding is the full identity. Bare body forms
        // ("claude-code:opus-4-7:main:v1") are rejected to avoid splitting
        // the IDL filter key space.
        let mut r = sample_record();
        let agent = Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("valid");
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: agent.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }];
        r.provenance.originating_agent_id = agent;
        r.scope = ScopeTuple {
            agent: Some("claude-code:opus-4-7:main:v1".to_owned()),
            ..ScopeTuple::default()
        };
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::MalformedScope { .. }));
    }

    #[test]
    fn scope_user_rejects_agent_author() {
        // scope.user requires a human author.
        let mut r = sample_record();
        let agent = Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("valid");
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: agent.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }];
        r.provenance.originating_agent_id = agent.clone();
        r.scope = ScopeTuple {
            user: Some(agent.as_str().to_owned()),
            ..ScopeTuple::default()
        };
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::MalformedScope { .. }));
    }

    #[test]
    fn scope_agent_rejects_human_author() {
        // scope.agent requires an agent author.
        let mut r = sample_record();
        // Sample author is `usr:tafeng`. Set scope.agent — must reject.
        r.scope = ScopeTuple {
            agent: Some("usr:tafeng".to_owned()),
            ..ScopeTuple::default()
        };
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::MalformedScope { .. }));
    }

    #[test]
    fn provenance_llm_id_serialized_when_none() {
        let r = sample_record();
        let json = serde_json::to_value(&r).expect("ser");
        let provenance = json
            .get("provenance")
            .and_then(|v| v.as_object())
            .expect("provenance object");
        assert!(
            provenance.contains_key("llm_id_if_any"),
            "llm_id_if_any must always serialize, even when None"
        );
        assert_eq!(
            provenance.get("llm_id_if_any"),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn provenance_round_trip_preserves_explicit_no_llm() {
        // The serialization side ensures every record carries the
        // `llm_id_if_any` key (null when no LLM was used). This test pins
        // the round-trip so the structural-stability invariant codex
        // round 1 flagged is enforced from the producer side: a sender
        // can never emit a record without the key.
        let r = sample_record();
        let s = serde_json::to_string(&r).expect("ser");
        assert!(
            s.contains("\"llm_id_if_any\":null"),
            "expected explicit `llm_id_if_any: null` in {s}"
        );
        let back: MemoryRecord = serde_json::from_str(&s).expect("de");
        assert_eq!(r, back);
    }

    #[test]
    fn agent_author_cannot_forge_user_scope_via_unsigned_principal() {
        // P0 attack: agent signs a record but adds an unsigned `principal:
        // usr:victim` entry, claiming `scope.user = victim`. With the P0
        // chain-shape rule the unsigned principal is rejected before the
        // scope cross-check even runs — both gates close the forgery.
        let mut r = sample_record();
        r.actor_chain = vec![
            ActorChainEntry {
                role: ChainRole::Principal,
                identity: Identity::parse("usr:victim").expect("valid"),
                at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
            },
            ActorChainEntry {
                role: ChainRole::Author,
                identity: Identity::parse("agt:attacker:v1").expect("valid"),
                at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
            },
        ];
        r.scope = ScopeTuple {
            user: Some("victim".to_owned()),
            ..ScopeTuple::default()
        };
        r.provenance.originating_agent_id = Identity::parse("agt:attacker:v1").expect("valid");
        let err = r.validate().unwrap_err();
        assert!(
            matches!(
                err,
                DomainError::MissingSignature { .. } | DomainError::MalformedScope { .. }
            ),
            "agent author cannot satisfy scope.user via unsigned principal entry"
        );
    }

    #[test]
    fn sensor_author_rejected_for_non_sensor_kinds() {
        let mut r = sample_record();
        r.kind = MemoryKind::Rule;
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: r.provenance.source_sensor.clone(),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }];
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::InvalidIdentity { .. }));
    }

    #[test]
    fn created_after_updated_rejected() {
        let mut r = sample_record();
        r.provenance.created_at = Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid");
        r.updated_at = Rfc3339Timestamp::parse("2026-04-22T14:00:00Z").expect("valid");
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::InvalidTimestamp { .. }));
    }

    #[test]
    fn chain_entry_after_updated_rejected() {
        let mut r = sample_record();
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: Identity::parse("usr:tafeng").expect("valid"),
            at: Rfc3339Timestamp::parse("2026-04-22T16:00:00Z").expect("valid"),
        }];
        r.updated_at = Rfc3339Timestamp::parse("2026-04-22T14:00:00Z").expect("valid");
        let err = r.validate().unwrap_err();
        assert!(matches!(err, DomainError::InvalidTimestamp { .. }));
    }

    #[test]
    fn temporal_check_handles_offsets() {
        // 14:00 +02:00 == 12:00 UTC, which is BEFORE 13:00 Z, so the
        // ordering must be chronological, not lexical.
        let mut r = sample_record();
        r.provenance.created_at =
            Rfc3339Timestamp::parse("2026-04-22T14:00:00+02:00").expect("valid");
        r.updated_at = Rfc3339Timestamp::parse("2026-04-22T13:00:00Z").expect("valid");
        r.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: Identity::parse("usr:tafeng").expect("valid"),
            at: Rfc3339Timestamp::parse("2026-04-22T14:00:00+02:00").expect("valid"),
        }];
        r.validate()
            .expect("created_at 14:00+02:00 (= 12:00Z) is before updated_at 13:00Z");
    }

    #[test]
    fn deserialize_rejects_unknown_fields() {
        let mut value = serde_json::to_value(sample_record()).expect("ser");
        value
            .as_object_mut()
            .expect("object")
            .insert("zzz".to_owned(), serde_json::json!("bad"));
        let res: Result<MemoryRecord, _> = serde_json::from_value(value);
        assert!(res.is_err(), "unknown field should reject");
    }
}
