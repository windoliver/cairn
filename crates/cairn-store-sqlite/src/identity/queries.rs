//! Read-path SQL helpers for [`super::SqliteIdentityRegistry`] (issue #50).
//!
//! Helpers here centralise the string↔enum mappings for `ProvisioningState`,
//! `IdentityKind`, and `IdentityVisibility` that are shared across multiple
//! read methods.

use std::num::NonZeroU32;

use chrono::{DateTime, Utc};

use cairn_core::contract::identity_registry::{IdentityVisibility, RegistryError};
use cairn_core::domain::identity::{
    Identity, IdentityKind,
    keys::{IdentityRevision, KeyVersion},
    records::{IdentityKeyEntry, PendingIdentityEntry, ProvisioningState, PublicIdentityRecord},
};

// ── ProvisioningState ↔ &str ─────────────────────────────────────────────────

/// Map a [`ProvisioningState`] variant to its `SQLite` column string.
///
/// Used by C6–C9 write paths; unused by C5 which only writes hard-coded
/// literals. Kept here so later tasks can import it without churn.
#[allow(dead_code, reason = "consumed by C6+ write paths")]
pub(super) fn state_str(s: ProvisioningState) -> &'static str {
    match s {
        ProvisioningState::Pending => "pending",
        ProvisioningState::Active => "active",
        ProvisioningState::RevokePending => "revoke_pending",
        ProvisioningState::Revoked => "revoked",
        ProvisioningState::PurgePending => "purge_pending",
        ProvisioningState::Purged => "purged",
    }
}

/// Parse a `SQLite` column string back to a [`ProvisioningState`].
///
/// # Errors
/// Returns [`RegistryError::Backend`] if `s` does not match a known state.
pub(super) fn parse_state(s: &str) -> Result<ProvisioningState, RegistryError> {
    match s {
        "pending" => Ok(ProvisioningState::Pending),
        "active" => Ok(ProvisioningState::Active),
        "revoke_pending" => Ok(ProvisioningState::RevokePending),
        "revoked" => Ok(ProvisioningState::Revoked),
        "purge_pending" => Ok(ProvisioningState::PurgePending),
        "purged" => Ok(ProvisioningState::Purged),
        other => Err(RegistryError::Backend(
            format!("unknown provisioning_state in DB: {other}").into(),
        )),
    }
}

// ── IdentityKind ↔ &str ──────────────────────────────────────────────────────

/// Map an [`IdentityKind`] variant to its `SQLite` column string.
pub(super) fn kind_str(k: IdentityKind) -> &'static str {
    match k {
        IdentityKind::Human => "human",
        IdentityKind::Agent => "agent",
        IdentityKind::Sensor => "sensor",
    }
}

/// Parse a `SQLite` column string back to an [`IdentityKind`].
///
/// # Errors
/// Returns [`RegistryError::Backend`] if `s` does not match a known kind.
pub(super) fn parse_kind(s: &str) -> Result<IdentityKind, RegistryError> {
    match s {
        "human" => Ok(IdentityKind::Human),
        "agent" => Ok(IdentityKind::Agent),
        "sensor" => Ok(IdentityKind::Sensor),
        other => Err(RegistryError::Backend(
            format!("unknown identity kind in DB: {other}").into(),
        )),
    }
}

// ── IdentityVisibility → state slice ─────────────────────────────────────────

/// Return the set of `provisioning_state` values that satisfy `visibility`.
///
/// The slice is `'static`; callers build a parameterised `IN (...)` clause
/// dynamically from its length.
pub(super) fn visibility_states(v: IdentityVisibility) -> &'static [&'static str] {
    match v {
        IdentityVisibility::Operational => &["active", "revoked"],
        IdentityVisibility::IncludingPending => &["active", "revoked", "pending"],
        IdentityVisibility::IncludingPurgePending => {
            &["active", "revoked", "pending", "purge_pending"]
        }
        // Audit and any future `#[non_exhaustive]` variants: return all
        // states so callers see rows rather than silently returning nothing.
        _ => &[
            "active",
            "revoked",
            "pending",
            "revoke_pending",
            "purge_pending",
            "purged",
        ],
    }
}

// ── Row → typed struct helpers ────────────────────────────────────────────────

/// Parse a timestamp column that is stored as RFC 3339 text.
///
/// # Errors
/// Returns [`RegistryError::Backend`] if the string cannot be parsed.
fn parse_ts(s: &str) -> Result<DateTime<Utc>, RegistryError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| RegistryError::Backend(Box::new(e)))
}

/// Parse an optional timestamp column.
///
/// # Errors
/// Returns [`RegistryError::Backend`] if the string is present but unparseable.
fn parse_opt_ts(s: Option<&str>) -> Result<Option<DateTime<Utc>>, RegistryError> {
    s.map(parse_ts).transpose()
}

/// Map a `rusqlite::Row` from a `SELECT … FROM identities` query into a
/// [`PublicIdentityRecord`].
///
/// Column order expected: `id`, `kind`, `current_key_version`,
/// `provisioning_state`, `created_at`, `activated_at`, `revoked_at`,
/// `purge_requested_at`, `purged_at`.
pub(super) fn row_to_public_identity_record(
    row: &rusqlite::Row<'_>,
) -> Result<PublicIdentityRecord, rusqlite::Error> {
    // We convert domain errors to rusqlite errors so the closure signature
    // matches what `query_row` / `query_map` expect.
    let id_str: String = row.get(0)?;
    let kind_str_val: String = row.get(1)?;
    let kv_u32: u32 = row.get(2)?;
    let state_str_val: String = row.get(3)?;
    let created_str: String = row.get(4)?;
    let activated_str: Option<String> = row.get(5)?;
    let revoked_str: Option<String> = row.get(6)?;
    let purge_req_str: Option<String> = row.get(7)?;
    let purged_str: Option<String> = row.get(8)?;

    // --- parse each field, converting errors to rusqlite::Error::ToSqlConversionFailure
    let id = Identity::parse(id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let kind = parse_kind(&kind_str_val).map_err(|e| match e {
        RegistryError::Backend(b) => {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, b)
        }
        _ => rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            "kind parse error".into(),
        ),
    })?;

    let nz = NonZeroU32::new(kv_u32).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Integer,
            "key_version must be >= 1".into(),
        )
    })?;
    let current_key_version = KeyVersion::new(nz);

    let provisioning_state = parse_state(&state_str_val).map_err(|e| match e {
        RegistryError::Backend(b) => {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, b)
        }
        _ => rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            "state parse error".into(),
        ),
    })?;

    let created_at = parse_ts(&created_str).map_err(|e| match e {
        RegistryError::Backend(b) => {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, b)
        }
        _ => rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            "ts parse error".into(),
        ),
    })?;

    let mk_opt_ts =
        |s: Option<String>, col: usize| -> Result<Option<DateTime<Utc>>, rusqlite::Error> {
            parse_opt_ts(s.as_deref()).map_err(|e| match e {
                RegistryError::Backend(b) => {
                    rusqlite::Error::FromSqlConversionFailure(col, rusqlite::types::Type::Text, b)
                }
                _ => rusqlite::Error::FromSqlConversionFailure(
                    col,
                    rusqlite::types::Type::Text,
                    "ts parse error".into(),
                ),
            })
        };

    let activated_at = mk_opt_ts(activated_str, 5)?;
    let revoked_at = mk_opt_ts(revoked_str, 6)?;
    let purge_requested_at = mk_opt_ts(purge_req_str, 7)?;
    let purged_at = mk_opt_ts(purged_str, 8)?;

    Ok(PublicIdentityRecord {
        id,
        kind,
        current_key_version,
        // The revision is structural and encoded as `FIRST` here; callers that
        // need a different revision must derive it from the identity id token
        // (e.g., the `:v<N>` suffix).  The DB schema has no revision column.
        revision: IdentityRevision::FIRST,
        provisioning_state,
        created_at,
        activated_at,
        revoked_at,
        purge_requested_at,
        purged_at,
    })
}

/// Map a `rusqlite::Row` from a `SELECT … FROM identity_keys` query into an
/// [`IdentityKeyEntry`].
///
/// Column order expected: `identity_id`, `key_version`, `public_key`,
/// `signed_predecessor`, `created_at`, `superseded_at`.
pub(super) fn row_to_identity_key_entry(
    row: &rusqlite::Row<'_>,
) -> Result<IdentityKeyEntry, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let kv_u32: u32 = row.get(1)?;
    let pk_bytes: Vec<u8> = row.get(2)?;
    let signed_pred: Option<Vec<u8>> = row.get(3)?;
    let created_str: String = row.get(4)?;
    let superseded_str: Option<String> = row.get(5)?;

    let identity_id = Identity::parse(id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let nz = NonZeroU32::new(kv_u32).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Integer,
            "key_version must be >= 1".into(),
        )
    })?;
    let key_version = KeyVersion::new(nz);

    let public_key: [u8; 32] = pk_bytes.as_slice().try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Blob,
            "public_key must be 32 bytes".into(),
        )
    })?;

    let created_at = parse_ts(&created_str).map_err(|e| match e {
        RegistryError::Backend(b) => {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, b)
        }
        _ => rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            "ts parse error".into(),
        ),
    })?;

    let superseded_at = parse_opt_ts(superseded_str.as_deref()).map_err(|e| match e {
        RegistryError::Backend(b) => {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, b)
        }
        _ => rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Text,
            "ts parse error".into(),
        ),
    })?;

    Ok(IdentityKeyEntry {
        identity_id,
        key_version,
        public_key,
        signed_predecessor: signed_pred,
        created_at,
        superseded_at,
    })
}

/// Map a `rusqlite::Row` from the `list_pending` query into a
/// [`PendingIdentityEntry`].
///
/// Column order expected: `identity_id`, `key_version`, `public_key`,
/// `created_at`.
pub(super) fn row_to_pending_identity_entry(
    row: &rusqlite::Row<'_>,
) -> Result<PendingIdentityEntry, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let kv_u32: u32 = row.get(1)?;
    let pk_bytes: Vec<u8> = row.get(2)?;
    let created_str: String = row.get(3)?;

    let identity = Identity::parse(id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;

    let nz = NonZeroU32::new(kv_u32).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Integer,
            "key_version must be >= 1".into(),
        )
    })?;
    let key_version = KeyVersion::new(nz);

    let public_key: [u8; 32] = pk_bytes.as_slice().try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Blob,
            "public_key must be 32 bytes".into(),
        )
    })?;

    let created_at = parse_ts(&created_str).map_err(|e| match e {
        RegistryError::Backend(b) => {
            rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, b)
        }
        _ => rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            "ts parse error".into(),
        ),
    })?;

    Ok(PendingIdentityEntry {
        identity,
        key_version,
        public_key,
        created_at,
    })
}

/// Expand an IN-clause placeholder string for `n` items: `(?1,?2,...,?n)`.
///
/// Used to build parameterised visibility filters without SQL injection.
/// `offset` is the 1-based index of the first placeholder (for when earlier
/// params are already bound).
pub(super) fn in_placeholders(count: usize, offset: usize) -> String {
    (0..count)
        .map(|i| format!("?{}", offset + i))
        .collect::<Vec<_>>()
        .join(",")
}
