//! `MemoryRecord` ↔ row projection.
//!
//! One side is the canonical `record_json` blob; the other side is the
//! denormalized hot columns. Every upsert writes both; every read returns
//! both for callers that want to skip JSON deserialization.
//!
//! The taxonomy wire forms (`kind`, `class`, `visibility`) are sourced from
//! the [`MemoryKind::as_str`][k] / [`MemoryClass::as_str`][c] /
//! [`MemoryVisibility::as_str`][v] methods on the domain enums so this
//! module never drifts when a new variant is added in `cairn-core`.
//!
//! [k]: cairn_core::domain::taxonomy::MemoryKind::as_str
//! [c]: cairn_core::domain::taxonomy::MemoryClass::as_str
//! [v]: cairn_core::domain::taxonomy::MemoryVisibility::as_str

// `ProjectedRow`, `from_record`, `record_from_json`, and the `*_from_str`
// parsers are wired into the read/write paths by tasks T14–T17 of the same
// plan; until then the only callers are the unit tests below. Allow at the
// module level so the staged landing keeps clippy clean.
#![allow(
    dead_code,
    reason = "wired into upsert/read paths by later tasks in plan #46"
)]

use cairn_core::domain::{
    BodyHash, MemoryRecord, RecordId, ScopeTuple, TargetId,
    taxonomy::{MemoryClass, MemoryKind, MemoryVisibility},
};

use crate::error::StoreError;

/// Owned, parameterizable view of the columns the store writes for one
/// record version.
///
/// Every field maps 1:1 to a column in the `records_latest` /
/// `records_versions` schema. `from_record` produces the value an upsert
/// statement binds; `record_from_json` is the inverse (canonical hydration
/// from `record_json`). The hot columns are denormalized for indexed reads
/// — they MUST stay in sync with `record_json`, and the
/// `hot_columns_match_json` proptest pins that invariant.
#[derive(Debug, Clone)]
pub(crate) struct ProjectedRow {
    /// ULID of the version row (`records_versions.record_id`).
    pub record_id: String,
    /// Supersession lineage key (brief §3, §3.0). Equals `record_id` for a
    /// fresh record; carries the prior `target_id` after supersession.
    pub target_id: String,
    /// Monotonic version counter for this `target_id`, starting at 1.
    pub version: i64,
    /// Vault-relative markdown path (`vault/<scope>/<id>.md`). Derived
    /// deterministically here until the markdown projector lands; FTS-on-path
    /// tests rely on a stable form.
    pub path: String,
    /// Wire-form `MemoryKind` (lower-snake-case).
    pub kind: String,
    /// Wire-form `MemoryClass` (lower-snake-case).
    pub class: String,
    /// Wire-form `MemoryVisibility` tier (lower-snake-case).
    pub visibility: String,
    /// Serialized `ScopeTuple` (canonical JSON; `None` fields omitted).
    pub scope: String,
    /// Serialized `actor_chain` array (canonical JSON).
    pub actor_chain: String,
    /// Markdown body; required and non-empty per `MemoryRecord::validate`.
    pub body: String,
    /// `blake3:` hash over `body` (drives idempotent-upsert detection).
    pub body_hash: String,
    /// Wall-clock epoch-seconds for the version's first persistence.
    pub created_at: i64,
    /// Wall-clock epoch-seconds for the most recent durable update.
    pub updated_at: i64,
    /// `1` when this row is the live head of its `target_id`; `0` otherwise.
    pub active: i64,
    /// `1` when the row is tombstoned (soft-deleted); `0` otherwise.
    pub tombstoned: i64,
    /// `1` when the row is statically pinned (hot-memory recipe baseline);
    /// `0` otherwise. Always `0` until the static-promotion workflow lands.
    pub is_static: i64,
    /// Canonical record bytes — the source of truth `record_from_json`
    /// hydrates from.
    pub record_json: String,
    /// Confidence scalar mirrored into a hot column for ranking indexes.
    pub confidence: f64,
    /// Salience scalar mirrored into a hot column for ranking indexes.
    pub salience: f64,
    /// Mirror of `MemoryRecord.target_id` into the dedicated explicit
    /// column written by migration `0008_record_extensions`. Keeps the
    /// supersession-lineage index addressable without cracking
    /// `record_json`.
    pub target_id_explicit: Option<String>,
    /// Serialized `tags` array (canonical JSON; empty array when no tags).
    pub tags_json: String,
}

impl ProjectedRow {
    /// Build a [`ProjectedRow`] from a [`MemoryRecord`] for write.
    ///
    /// `version`, `created_at`, `updated_at`, `body_hash`, `active`, and
    /// `tombstoned` are caller-supplied because they are determined by the
    /// upsert state machine (idempotency check, supersession decision, WAL
    /// timestamp), not by the record itself.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Codec`] when `serde_json` fails to serialize
    /// the record, scope, actor chain, or tags.
    pub(crate) fn from_record(
        record: &MemoryRecord,
        version: u32,
        created_at: i64,
        updated_at: i64,
        body_hash: &BodyHash,
        active: bool,
        tombstoned: bool,
    ) -> Result<Self, StoreError> {
        let record_json = serde_json::to_string(record)?;
        let scope = serde_json::to_string(&record.scope)?;
        let actor_chain = serde_json::to_string(&record.actor_chain)?;
        let tags_json = serde_json::to_string(&record.tags)?;
        Ok(Self {
            record_id: record.id.as_str().to_owned(),
            target_id: record.target_id.as_str().to_owned(),
            version: i64::from(version),
            path: derive_path(record),
            kind: kind_str(record.kind).to_owned(),
            class: class_str(record.class).to_owned(),
            visibility: visibility_str(record.visibility).to_owned(),
            scope,
            actor_chain,
            body: record.body.clone(),
            body_hash: body_hash.as_str().to_owned(),
            created_at,
            updated_at,
            active: i64::from(active),
            tombstoned: i64::from(tombstoned),
            is_static: 0,
            record_json,
            confidence: f64::from(record.confidence),
            salience: f64::from(record.salience),
            target_id_explicit: Some(record.target_id.as_str().to_owned()),
            tags_json,
        })
    }
}

/// Hydrate a [`MemoryRecord`] from a row's `record_json` column.
///
/// # Errors
///
/// Returns [`StoreError::Codec`] when the stored JSON cannot be parsed.
/// This indicates either schema drift or corruption — the store should
/// surface the failure rather than silently dropping the row.
pub(crate) fn record_from_json(json: &str) -> Result<MemoryRecord, StoreError> {
    Ok(serde_json::from_str(json)?)
}

/// Derive the vault-relative markdown path for a record.
///
/// The markdown projector (planned, not yet implemented) is the eventual
/// owner of this mapping. Until that lands the projection module emits a
/// deterministic fallback so FTS-on-path tests still see a stable column.
fn derive_path(record: &MemoryRecord) -> String {
    format!(
        "vault/{}/{}.md",
        scope_segment(&record.scope),
        record.id.as_str()
    )
}

/// Render a [`ScopeTuple`] as a path segment for [`derive_path`]. Returns
/// `_root` when no dimension is set (defence-in-depth — `validate` rejects
/// empty scopes before persistence).
fn scope_segment(scope: &ScopeTuple) -> String {
    let mut parts = Vec::new();
    if let Some(t) = scope.tenant.as_deref() {
        parts.push(format!("tenant-{t}"));
    }
    if let Some(w) = scope.workspace.as_deref() {
        parts.push(format!("ws-{w}"));
    }
    if let Some(e) = scope.entity.as_deref() {
        parts.push(format!("ent-{e}"));
    }
    if parts.is_empty() {
        "_root".to_owned()
    } else {
        parts.join("/")
    }
}

/// Wire-form kind string. Delegates to
/// [`MemoryKind::as_str`][cairn_core::domain::taxonomy::MemoryKind::as_str]
/// so the column stays in lock-step with the canonical IDL spelling.
fn kind_str(k: MemoryKind) -> &'static str {
    k.as_str()
}

/// Wire-form class string. Delegates to
/// [`MemoryClass::as_str`][cairn_core::domain::taxonomy::MemoryClass::as_str].
fn class_str(c: MemoryClass) -> &'static str {
    c.as_str()
}

/// Wire-form visibility tier string. Delegates to
/// [`MemoryVisibility::as_str`][cairn_core::domain::taxonomy::MemoryVisibility::as_str].
fn visibility_str(v: MemoryVisibility) -> &'static str {
    v.as_str()
}

/// Parse a [`RecordId`] from a column value, mapping a domain parse
/// failure to [`StoreError::Invariant`] — the store wrote it, so a
/// rejection here is a corruption / schema-drift signal, not user input.
///
/// # Errors
///
/// Returns [`StoreError::Invariant`] when the string is not a valid wire-form
/// `RecordId` (length, alphabet, or leading-character checks fail).
pub(crate) fn record_id_from_str(s: &str) -> Result<RecordId, StoreError> {
    RecordId::parse(s.to_owned()).map_err(|e| StoreError::Invariant {
        what: format!("invalid record_id `{s}`: {e}"),
    })
}

/// Parse a [`TargetId`] from a column value. See [`record_id_from_str`]
/// for the failure rationale.
///
/// # Errors
///
/// Returns [`StoreError::Invariant`] when the stored target id fails the
/// wire-form validation in [`TargetId::parse`].
pub(crate) fn target_id_from_str(s: &str) -> Result<TargetId, StoreError> {
    TargetId::parse(s.to_owned()).map_err(|e| StoreError::Invariant {
        what: format!("invalid target_id `{s}`: {e}"),
    })
}

/// Parse a [`BodyHash`] from a column value. See [`record_id_from_str`]
/// for the failure rationale.
///
/// # Errors
///
/// Returns [`StoreError::Invariant`] when the stored hash fails the
/// wire-form validation in [`BodyHash::parse`].
pub(crate) fn body_hash_from_str(s: &str) -> Result<BodyHash, StoreError> {
    BodyHash::parse(s.to_owned()).map_err(|e| StoreError::Invariant {
        what: format!("invalid body_hash `{s}`: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MemoryRecord {
        cairn_core::domain::record::tests_export::sample_record()
    }

    #[test]
    fn json_round_trip_via_projection() {
        let r = sample();
        let body_hash = BodyHash::compute(&r.body);
        let row = ProjectedRow::from_record(&r, 1, 1000, 2000, &body_hash, true, false)
            .expect("project");
        let back = record_from_json(&row.record_json).expect("hydrate");
        assert_eq!(r, back);
    }

    #[test]
    fn target_id_explicit_mirrors_record() {
        let r = sample();
        let body_hash = BodyHash::compute(&r.body);
        let row = ProjectedRow::from_record(&r, 1, 1000, 2000, &body_hash, true, false)
            .expect("project");
        assert_eq!(row.target_id_explicit.as_deref(), Some(r.target_id.as_str()));
    }
}
