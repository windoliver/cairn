//! ID newtypes per spec §4.1. No primitives leak across crate boundaries.

use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

/// Monotonically increasing version of a keystore-managed signing key.
///
/// `KeyVersion::FIRST` is the value assigned when a key is first minted;
/// each rotation produces the next value via [`KeyVersion::next`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyVersion(NonZeroU32);

impl KeyVersion {
    /// Initial key version (`1`).
    pub const FIRST: Self = Self(NonZeroU32::MIN);

    /// Construct from a `NonZeroU32`. Used by adapters reading persisted rows.
    #[must_use]
    pub const fn new(n: NonZeroU32) -> Self {
        Self(n)
    }

    /// Underlying `u32` value (always `>= 1`).
    #[must_use]
    pub fn as_u32(self) -> u32 {
        self.0.get()
    }

    /// Next version after `self`.
    ///
    /// # Errors
    /// Returns [`DomainError::InvalidIdentity`] on `u32` overflow.
    pub fn next(self) -> Result<Self, DomainError> {
        // `checked_add(1)` on a `>= 1` value can only yield `None` on overflow;
        // a non-zero successor is therefore equivalent to a successful add.
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(DomainError::InvalidIdentity {
                message: "key_version overflow".into(),
            })
    }
}

impl std::fmt::Display for KeyVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Monotonically increasing revision of an identity's profile (model+harness
/// for agents, family+name+host for sensors, display-name for humans).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IdentityRevision(NonZeroU32);

impl IdentityRevision {
    /// Initial revision (`v1`).
    pub const FIRST: Self = Self(NonZeroU32::MIN);

    /// Construct from a `NonZeroU32`.
    #[must_use]
    pub const fn new(n: NonZeroU32) -> Self {
        Self(n)
    }

    /// Underlying `u32` value (always `>= 1`).
    #[must_use]
    pub fn as_u32(self) -> u32 {
        self.0.get()
    }

    /// Next revision after `self`.
    ///
    /// # Errors
    /// Returns [`DomainError::InvalidIdentity`] on `u32` overflow.
    pub fn next(self) -> Result<Self, DomainError> {
        // `NonZeroU32::checked_add` returns `None` only on `u32` overflow.
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(DomainError::InvalidIdentity {
                message: "identity_revision overflow".into(),
            })
    }
}

impl std::fmt::Display for IdentityRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// ULID-shaped opaque vault identifier (per-host random root-of-trust id).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VaultId(String);

impl VaultId {
    /// Parse a wire-form vault id, validating that it is a ULID.
    ///
    /// # Errors
    /// Returns [`DomainError::InvalidIdentity`] when `raw` is not a valid ULID.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        ulid::Ulid::from_string(&raw).map_err(|_| DomainError::InvalidIdentity {
            message: format!("vault_id is not a ULID: {raw}"),
        })?;
        Ok(Self(raw))
    }

    /// Mint a fresh ULID-backed [`VaultId`].
    #[must_use]
    pub fn mint() -> Self {
        Self(ulid::Ulid::new().to_string())
    }

    /// Wire-form string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for VaultId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// SHA-256 digest of an identity's public-witness payload (§4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WitnessHash([u8; 32]);

impl WitnessHash {
    /// Wrap a precomputed 32-byte digest.
    #[must_use]
    pub const fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// Borrow the underlying 32-byte digest.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Compute the SHA-256 digest of `witness`.
    #[must_use]
    pub fn from_witness(witness: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(witness);
        Self(h.finalize().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_version_next() {
        assert_eq!(KeyVersion::FIRST.next().unwrap().as_u32(), 2);
    }

    #[test]
    fn identity_revision_display() {
        assert_eq!(IdentityRevision::FIRST.to_string(), "v1");
    }

    #[test]
    fn vault_id_round_trip() {
        let id = VaultId::mint();
        let parsed = VaultId::parse(id.as_str()).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn vault_id_rejects_non_ulid() {
        assert!(VaultId::parse("not-a-ulid").is_err());
    }

    #[test]
    fn witness_hash_deterministic() {
        let h1 = WitnessHash::from_witness(b"hello");
        let h2 = WitnessHash::from_witness(b"hello");
        assert_eq!(h1, h2);
    }
}
