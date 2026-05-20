//! Signed trust-state receipts per spec §4.1.
//!
//! A [`ReceiptPayload`] captures the parameters of a key-rotation or
//! key-revocation event, serialised to canonical JSON and signed with the
//! signer's [`SigningKey`].  Recipients verify with the corresponding
//! [`ed25519_dalek::VerifyingKey`].

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::domain::identity::{
    Identity,
    keys::{KeyVersion, SigningKey},
    records::ReceiptId,
};

/// Discriminates between a key-rotation and a key-revocation receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReceiptOpKind {
    /// The signer rotated the target's active signing key.
    Rotation,
    /// The signer revoked the target's active signing key.
    Revocation,
}

/// The fields that are canonically JSON-encoded and then signed.
///
/// Field order in the Rust struct determines serialisation order, which is the
/// canonical form — do not reorder fields without bumping the wire version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptPayload {
    /// The kind of operation this receipt attests to.
    pub op_kind: ReceiptOpKind,
    /// The identity whose key state changed.
    pub target: Identity,
    /// The identity that signed this receipt.
    pub signer: Identity,
    /// The version of the signer's key that produced the signature.
    pub signer_key_version: KeyVersion,
    /// The key version being superseded (rotation) or disabled (revocation).
    /// `None` when the signer is establishing a first key.
    pub old_key_version: Option<KeyVersion>,
    /// The replacement key version.  `None` for pure revocations.
    pub new_key_version: Option<KeyVersion>,
    /// Wall-clock timestamp at which the receipt was issued.
    pub issued_at: DateTime<Utc>,
}

impl ReceiptPayload {
    /// Canonical JSON encoding — keys in field-declaration order.
    ///
    /// Used as the byte slice signed by the signer's ed25519 key.
    ///
    /// # Errors
    /// Propagates [`serde_json::Error`] on serialisation failure (e.g. a
    /// map key that is not a string).
    pub fn canonical_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Sign the canonical JSON encoding of this payload.
    ///
    /// Returns the resulting [`Signature`], or a [`serde_json::Error`] if
    /// serialisation fails.
    ///
    /// # Errors
    /// Propagates [`serde_json::Error`] from [`Self::canonical_json`].
    pub fn sign(&self, signer: &SigningKey) -> Result<Signature, serde_json::Error> {
        let bytes = self.canonical_json()?;
        Ok(signer.sign(&bytes))
    }

    /// Verify that `signature` over this payload was produced by `signer_key`.
    ///
    /// # Errors
    /// Returns [`ReceiptError::Encode`] if serialisation fails, or
    /// [`ReceiptError::BadSignature`] if the signature does not match.
    pub fn verify(
        &self,
        signature: &Signature,
        signer_key: &VerifyingKey,
    ) -> Result<(), ReceiptError> {
        let bytes = self.canonical_json().map_err(ReceiptError::Encode)?;
        signer_key
            .verify(&bytes, signature)
            .map_err(|_| ReceiptError::BadSignature)
    }
}

/// A completed key-rotation event with its [`ReceiptPayload`] and detached
/// signature.  `pending_eviction` is `true` while the old key material is
/// awaiting deletion from the keystore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationReceipt {
    /// Unique identifier for this receipt.
    pub id: ReceiptId,
    /// The signed payload attesting to the rotation.
    pub payload: ReceiptPayload,
    /// Raw ed25519 signature bytes (64 bytes).
    pub signature: Vec<u8>,
    /// `true` while old key material is pending eviction from the keystore.
    pub pending_eviction: bool,
}

/// A completed key-revocation event with its [`ReceiptPayload`] and detached
/// signature.  `pending_key_disable` is `true` while the key is awaiting
/// deactivation in the keystore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevocationReceipt {
    /// Unique identifier for this receipt.
    pub id: ReceiptId,
    /// The signed payload attesting to the revocation.
    pub payload: ReceiptPayload,
    /// Raw ed25519 signature bytes (64 bytes).
    pub signature: Vec<u8>,
    /// `true` while the revoked key is pending deactivation in the keystore.
    pub pending_key_disable: bool,
}

/// Errors produced by [`ReceiptPayload`] sign/verify operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReceiptError {
    /// Canonical JSON serialisation of the payload failed.
    #[error("canonical JSON encoding failed: {0}")]
    Encode(#[from] serde_json::Error),
    /// The ed25519 signature did not match the payload and key.
    #[error("ed25519 signature verification failed")]
    BadSignature,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::identity::keys::SigningKey;

    #[test]
    fn round_trip_sign_verify() {
        let mut rng = rand_core::OsRng;
        let signer_key = SigningKey::generate(&mut rng);
        let signer_pub = signer_key.verifying_key();
        let payload = ReceiptPayload {
            op_kind: ReceiptOpKind::Rotation,
            target: Identity::parse("hmn:alice:v1").unwrap(),
            signer: Identity::parse("agt:claude-opus-coder:v1").unwrap(),
            signer_key_version: KeyVersion::FIRST,
            old_key_version: Some(KeyVersion::FIRST),
            new_key_version: Some(KeyVersion::FIRST.next().unwrap()),
            issued_at: chrono::Utc::now(),
        };
        let sig = payload.sign(&signer_key).unwrap();
        payload.verify(&sig, &signer_pub).unwrap();
    }

    #[test]
    fn tampered_payload_fails_verify() {
        let mut rng = rand_core::OsRng;
        let signer_key = SigningKey::generate(&mut rng);
        let mut payload = ReceiptPayload {
            op_kind: ReceiptOpKind::Rotation,
            target: Identity::parse("hmn:a:v1").unwrap(),
            signer: Identity::parse("hmn:a:v1").unwrap(),
            signer_key_version: KeyVersion::FIRST,
            old_key_version: None,
            new_key_version: Some(KeyVersion::FIRST),
            issued_at: chrono::Utc::now(),
        };
        let sig = payload.sign(&signer_key).unwrap();
        payload.target = Identity::parse("hmn:b:v1").unwrap();
        assert!(payload.verify(&sig, &signer_key.verifying_key()).is_err());
    }
}
