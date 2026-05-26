//! Snapshot manifest schema, canonical JSON serializer, and integrity envelope.
//!
//! Per spec §6.2 the manifest is the first member of the snapshot tarball.
//! Canonical-JSON serialization (RFC 8785) keeps `sha256(manifest_bytes)`
//! stable across serializations, which is what the integrity envelope
//! depends on.
//!
//! ## `u64` counts and JCS precision
//!
//! RFC 8785 (JCS) maps all JSON numbers through IEEE 754 doubles, which
//! means integers above 2^53 lose precision on the round-trip.  `u64` counts
//! (`record_count`, `tombstone_count`) are therefore serialized as decimal
//! strings (`"18446744073709551615"`) so the manifest hash is stable for
//! arbitrarily large vaults.  Deserialisation accepts the same string form.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Serde helper: encode/decode a `u64` as a JSON decimal string so that
/// JCS round-trips remain exact for values beyond 2^53.
mod u64_string {
    use super::{Deserialize, Deserializer, Serializer};

    // serde's `with` API requires `&T` — the lint fires because `u64` is
    // `Copy` and small, but we cannot change the signature here.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub(super) fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse::<u64>().map_err(serde::de::Error::custom)
    }
}

/// Current value of the manifest format version (the `schema_version`
/// field). Increment ONLY when introducing a forward-incompat manifest
/// change. The `schema_version: 1` set is the v0.2 release contract.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Complete description of a snapshot artifact: provenance, counts, and
/// migration heads.  Serialized as canonical JSON (RFC 8785) so the
/// sha256 over its bytes is deterministic across platforms and Rust versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotManifest {
    /// Manifest format version — currently always `1`.
    pub schema_version: u32,
    /// Globally unique identifier for this snapshot artifact.
    pub backup_id: String,
    /// Wall-clock time the snapshot was sealed.
    pub created_at: DateTime<Utc>,
    /// Opaque identifier for the machine that created the snapshot.
    pub source_machine_id: String,
    /// Opaque identifier for the vault that was snapshotted.
    pub source_vault_id: String,
    /// WAL frontier step at snapshot time (deterministic replay cursor).
    pub frontier_step: String,
    /// Number of live records in the snapshot.
    ///
    /// Serialized as a decimal string so the value round-trips exactly
    /// through RFC 8785 (JCS), which maps all numbers through IEEE 754
    /// doubles and would lose precision for values above 2^53.
    #[serde(with = "u64_string")]
    pub record_count: u64,
    /// Number of tombstone records in the snapshot.
    ///
    /// Serialized as a decimal string for the same reason as
    /// [`SnapshotManifest::record_count`].
    #[serde(with = "u64_string")]
    pub tombstone_count: u64,
    /// Per-component migration heads at snapshot time. `BTreeMap` so the
    /// non-canonical serializer (if ever used) also produces stable output;
    /// the canonical serializer normalizes anyway.
    pub schema_versions: BTreeMap<String, u32>,
    /// Optional human-readable label for the snapshot.
    pub label: Option<String>,
}

impl SnapshotManifest {
    /// Canonical JSON: sorted keys, no whitespace (RFC 8785 / JCS).
    /// Stable input for the integrity envelope.
    ///
    /// # Errors
    /// Returns the underlying serializer error if any field fails to
    /// serialize (none of the field types fail under normal conditions).
    pub fn to_canonical_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json_canonicalizer::to_string(self).map(String::into_bytes)
    }

    /// Parse from canonical JSON. Note: this accepts any valid JSON; the
    /// caller is responsible for asserting the input was actually canonical
    /// when integrity matters (see `IntegrityEnvelope`).
    ///
    /// # Errors
    /// Returns the underlying serde error if the input is malformed.
    pub fn from_canonical_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

/// Three-part integrity envelope: `sha256(manifest) || sha256(db) || sha256(tree)`.
/// Each component is computed independently so a partial check (manifest-only)
/// is cheap on the read path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrityEnvelope {
    /// Lowercase hex sha256 of the serialized `SnapshotManifest` bytes.
    pub manifest_sha256: String,
    /// Lowercase hex sha256 of the `SQLite` database blob.
    pub db_sha256: String,
    /// Lowercase hex sha256 of the markdown vault tree archive.
    pub tree_sha256: String,
}

impl IntegrityEnvelope {
    /// Returns the artifact-level hash `sha256(manifest_sha || db_sha || tree_sha)`
    /// in lowercase hex.
    #[must_use]
    pub fn artifact_sha256(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.manifest_sha256.as_bytes());
        h.update(self.db_sha256.as_bytes());
        h.update(self.tree_sha256.as_bytes());
        hex(h.finalize().as_slice())
    }
}

/// Compute sha256 of raw bytes, lowercase hex.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex(h.finalize().as_slice())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn manifest_strategy() -> impl Strategy<Value = SnapshotManifest> {
        (
            "[a-zA-Z0-9]{8,32}",
            0i64..=4_102_444_800i64, // 2100-01-01
            "[a-z0-9]{16,64}",
            "[a-z0-9-]{8,36}",
            "[a-z0-9:]{8,32}",
            any::<u64>(),
            any::<u64>(),
            proptest::collection::btree_map(
                "[a-z]{1,10}".prop_map(String::from),
                any::<u32>(),
                0..4,
            ),
            proptest::option::of("[a-zA-Z0-9 _-]{0,32}".prop_map(String::from)),
        )
            .prop_map(|(backup_id, ts, mid, vid, step, rc, tc, vers, label)| {
                SnapshotManifest {
                    schema_version: MANIFEST_SCHEMA_VERSION,
                    backup_id,
                    created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
                        .expect("timestamp in valid range"),
                    source_machine_id: mid,
                    source_vault_id: vid,
                    frontier_step: step,
                    record_count: rc,
                    tombstone_count: tc,
                    schema_versions: vers,
                    label,
                }
            })
    }

    proptest! {
        #[test]
        fn canonical_json_roundtrip(m in manifest_strategy()) {
            let bytes = m.to_canonical_json().expect("serialize");
            let parsed = SnapshotManifest::from_canonical_json(&bytes).expect("parse");
            prop_assert_eq!(m, parsed);
        }

        #[test]
        fn canonical_json_is_stable(m in manifest_strategy()) {
            let a = m.to_canonical_json().expect("serialize a");
            let b = m.to_canonical_json().expect("serialize b");
            prop_assert_eq!(a, b);
        }
    }

    #[test]
    fn artifact_sha_changes_when_db_sha_changes() {
        let env_a = IntegrityEnvelope {
            manifest_sha256: "aa".into(),
            db_sha256: "bb".into(),
            tree_sha256: "cc".into(),
        };
        let env_b = IntegrityEnvelope { db_sha256: "ff".into(), ..env_a.clone() };
        assert_ne!(env_a.artifact_sha256(), env_b.artifact_sha256());
    }

    #[test]
    fn artifact_sha_stable_for_same_input() {
        let env = IntegrityEnvelope {
            manifest_sha256: "aa".into(),
            db_sha256: "bb".into(),
            tree_sha256: "cc".into(),
        };
        assert_eq!(env.artifact_sha256(), env.artifact_sha256());
    }

    #[test]
    fn sha256_hex_known_value() {
        // sha256("hello world") = b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9
        assert_eq!(
            sha256_hex(b"hello world"),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9",
        );
    }
}
