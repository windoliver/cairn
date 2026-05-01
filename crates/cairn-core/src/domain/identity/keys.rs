//! ID newtypes per spec §4.1. No primitives leak across crate boundaries.

use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::domain::DomainError;
use crate::domain::identity::Identity;

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

/// Wrapped Ed25519 signing key — zeroized on drop, no Clone, no public bytes.
pub struct SigningKey(ed25519_dalek::SigningKey);

impl SigningKey {
    /// Generate a fresh Ed25519 keypair using the supplied CSPRNG.
    #[must_use]
    pub fn generate(rng: &mut impl rand_core::CryptoRngCore) -> Self {
        Self(ed25519_dalek::SigningKey::generate(rng))
    }
    /// Reconstruct a signing key from raw secret bytes (e.g. loaded from the keystore).
    #[must_use]
    pub fn from_bytes(bytes: &[u8; 32]) -> Self {
        Self(ed25519_dalek::SigningKey::from_bytes(bytes))
    }
    /// The corresponding public verifying key.
    #[must_use]
    pub fn verifying_key(&self) -> ed25519_dalek::VerifyingKey {
        self.0.verifying_key()
    }
    /// Sign `msg` with the wrapped key.
    #[must_use]
    pub fn sign(&self, msg: &[u8]) -> ed25519_dalek::Signature {
        use ed25519_dalek::Signer;
        self.0.sign(msg)
    }
    /// Borrow the raw secret bytes for keystore persistence. Caller is
    /// responsible for not retaining the slice.
    #[must_use]
    pub fn expose_secret_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }
}

impl Drop for SigningKey {
    fn drop(&mut self) {
        let mut bytes = self.0.to_bytes();
        bytes.zeroize();
    }
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SigningKey(<redacted>)")
    }
}

/// Witness/secret bytes — zeroized on drop, slice-borrow only.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    /// Wrap raw bytes (e.g. loaded from the keystore witness slot).
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    /// Borrow as a slice. The slice is invalidated on drop.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretBytes({} bytes <redacted>)", self.0.len())
    }
}

/// Discriminator for the per-handle account string. Either an identity
/// keypair (`<wire>#k<version>`) or the per-vault witness slot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HandleAccount {
    /// Per-identity, per-version signing keypair.
    Identity {
        /// The identity that owns the keypair.
        identity: Identity,
        /// The key version slot.
        version: KeyVersion,
    },
    /// Per-vault witness (binding tag).
    Witness,
}

/// Typed handle into a `Keystore`. The vault id encodes namespace
/// isolation at the type level so callers can't construct cross-vault handles.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretHandle {
    /// Vault namespace this handle belongs to.
    pub vault_id: VaultId,
    /// Which slot inside the vault the handle points at.
    pub account: HandleAccount,
}

impl SecretHandle {
    /// Construct a handle for the per-version signing keypair of `identity`.
    #[must_use]
    pub fn for_identity(vault_id: VaultId, identity: Identity, version: KeyVersion) -> Self {
        Self {
            vault_id,
            account: HandleAccount::Identity { identity, version },
        }
    }
    /// Construct a handle for the vault's witness slot.
    #[must_use]
    pub fn for_witness(vault_id: VaultId) -> Self {
        Self {
            vault_id,
            account: HandleAccount::Witness,
        }
    }
    /// The keystore service string: `cairn:<vault_id>`.
    #[must_use]
    pub fn service(&self) -> String {
        format!("cairn:{}", self.vault_id)
    }
    /// The keystore account string. `Identity { ... }` → `<wire>#k<version>`;
    /// `Witness` → the literal `__vault_witness__`.
    #[must_use]
    pub fn account_string(&self) -> String {
        match &self.account {
            HandleAccount::Witness => "__vault_witness__".to_owned(),
            HandleAccount::Identity { identity, version } => {
                format!("{identity}#k{version}")
            }
        }
    }
}

#[cfg(test)]
mod handle_tests {
    use super::*;
    use crate::domain::identity::Identity;

    #[test]
    fn identity_handle_format() {
        let v = VaultId::mint();
        let id = Identity::parse("hmn:alice:v1").unwrap();
        let h = SecretHandle::for_identity(v.clone(), id.clone(), KeyVersion::FIRST);
        assert_eq!(h.service(), format!("cairn:{v}"));
        assert_eq!(h.account_string(), "hmn:alice:v1#k1");
    }

    #[test]
    fn witness_handle_format() {
        let v = VaultId::mint();
        let h = SecretHandle::for_witness(v);
        assert_eq!(h.account_string(), "__vault_witness__");
    }

    #[test]
    fn signing_key_does_not_leak_in_debug() {
        let mut rng = rand_core::OsRng;
        let k = SigningKey::generate(&mut rng);
        assert_eq!(format!("{k:?}"), "SigningKey(<redacted>)");
    }
}
