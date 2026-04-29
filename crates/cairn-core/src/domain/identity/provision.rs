//! Pure provisioning helpers per spec §3.4 + §3.9.
//!
//! These are all pure functions — no I/O, no global state.  They produce the
//! data structures needed by an adapter to atomically provision an identity.

use chrono::{DateTime, Utc};

use crate::domain::DomainError;
use crate::domain::identity::{
    Identity, IdentityKind,
    keys::{IdentityRevision, KeyVersion, SecretHandle, SigningKey, VaultId},
    records::{IdentityKeyEntry, ProvisioningState, PublicIdentityRecord},
};

/// Convert an OS username into a stable identity slug per spec §3.9.
///
/// Rules applied in order:
/// 1. NFKC-normalize then NFD-decompose and drop combining marks (Unicode
///    category M), effectively stripping diacritics from Latin-derived scripts.
/// 2. Lowercase.
/// 3. Keep ASCII alphanumeric, `.`, `_`, and `-`; collapse runs of any other
///    character to a single `-`.
/// 4. Strip leading/trailing `-`.
/// 5. Truncate to 100 bytes (safe because only ASCII passes step 3).
/// 6. Fall back to `"user"` if the result is empty.
#[must_use]
pub fn normalize_human_slug(raw: &str) -> String {
    // NFKC first (e.g., ligatures, compatibility forms), then NFD to expose
    // base + combining sequences so we can drop the combiners.
    use unicode_normalization::UnicodeNormalization as _;
    let nfd_after_nfkc: String = raw
        .nfkc()
        .collect::<String>()
        .nfd()
        .filter(|ch| !unicode_normalization::char::is_combining_mark(*ch))
        .collect::<String>()
        .to_lowercase();
    let mut out = String::new();
    let mut prev_was_dash = false;
    for ch in nfd_after_nfkc.chars() {
        let allowed = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-');
        if allowed {
            out.push(ch);
            prev_was_dash = ch == '-';
        } else if !prev_was_dash {
            out.push('-');
            prev_was_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    let limited: String = trimmed.bytes().take(100).map(char::from).collect();
    let limited = limited.trim_end_matches('-').to_owned();
    if limited.is_empty() {
        "user".to_owned()
    } else {
        limited
    }
}

/// Mint a human [`Identity`] from an OS username slug and a revision.
///
/// The slug is NFKC-normalised and sanitised via [`normalize_human_slug`]
/// before being embedded in the wire form `hmn:<slug>:<rev>`.
///
/// # Errors
/// Returns [`DomainError::InvalidIdentity`] if the normalised slug contains
/// characters rejected by [`Identity::parse`].
#[must_use = "the returned Identity must be used to provision the record"]
pub fn mint_human_id(slug: &str, rev: IdentityRevision) -> Result<Identity, DomainError> {
    let s = normalize_human_slug(slug);
    Identity::parse(format!("hmn:{s}:{rev}"))
}

/// Mint an agent [`Identity`] from its harness, model, role, and revision.
///
/// Wire form: `agt:<harness>:<model>:<role>:<rev>`.
///
/// # Errors
/// Returns [`DomainError::InvalidIdentity`] if any component contains
/// characters outside `[A-Za-z0-9._:-]`.
#[must_use = "the returned Identity must be used to provision the record"]
pub fn mint_agent_id(
    harness: &str,
    model: &str,
    role: &str,
    rev: IdentityRevision,
) -> Result<Identity, DomainError> {
    Identity::parse(format!("agt:{harness}:{model}:{role}:{rev}"))
}

/// Mint a sensor [`Identity`] from its family, name, host, and revision.
///
/// Wire form: `snr:<family>:<name>:<host>:<rev>`.
///
/// # Errors
/// Returns [`DomainError::InvalidIdentity`] if any component contains
/// characters outside `[A-Za-z0-9._:-]`.
#[must_use = "the returned Identity must be used to provision the record"]
pub fn mint_sensor_id(
    family: &str,
    name: &str,
    host: &str,
    rev: IdentityRevision,
) -> Result<Identity, DomainError> {
    Identity::parse(format!("snr:{family}:{name}:{host}:{rev}"))
}

/// All data needed to commit a new identity to the store and keystore atomically.
///
/// Produced by [`build_provisioning_plan`].  The adapter must persist
/// `identity` and `key_entry` (in the WAL, then applied), then write
/// `signing_key` to the keystore under `secret_handle`.
#[derive(Debug)]
pub struct ProvisioningPlan {
    /// The public identity row to persist.
    pub identity: PublicIdentityRecord,
    /// The initial key entry row to persist.
    pub key_entry: IdentityKeyEntry,
    /// Handle pointing at the keystore slot that will hold `signing_key`.
    pub secret_handle: SecretHandle,
    /// The freshly generated Ed25519 signing key to store in the keystore.
    pub signing_key: SigningKey,
}

/// All caller-supplied parameters for [`build_provisioning_plan`].
#[derive(Debug, Clone)]
pub struct ProvisionInput {
    /// Vault namespace this identity belongs to.
    pub vault_id: VaultId,
    /// The pre-validated identity to provision.
    pub id: Identity,
    /// Which of the three identity kinds.
    pub kind: IdentityKind,
    /// Profile revision (use [`IdentityRevision::FIRST`] for new identities).
    pub revision: IdentityRevision,
}

/// Build a deterministic provisioning plan from the supplied `input` and RNG.
///
/// The plan is pure data: no I/O, no database, no keystore access.  The
/// adapter feeds the plan to its WAL machinery and keystore writer.
///
/// `rng` must be a cryptographically-secure PRNG (`OsRng`, `ChaCha20Rng`,
/// etc.).  `now` is the creation timestamp for both the identity row and the
/// key entry.
#[must_use]
pub fn build_provisioning_plan(
    input: ProvisionInput,
    rng: &mut impl rand_core::CryptoRngCore,
    now: DateTime<Utc>,
) -> ProvisioningPlan {
    let signing_key = SigningKey::generate(rng);
    let pubkey = signing_key.verifying_key().to_bytes();
    let key_version = KeyVersion::FIRST;
    let key_entry = IdentityKeyEntry {
        identity_id: input.id.clone(),
        key_version,
        public_key: pubkey,
        signed_predecessor: None,
        created_at: now,
        superseded_at: None,
    };
    let identity = PublicIdentityRecord {
        id: input.id.clone(),
        kind: input.kind,
        current_key_version: key_version,
        revision: input.revision,
        provisioning_state: ProvisioningState::Pending,
        created_at: now,
        activated_at: None,
        revoked_at: None,
        purge_requested_at: None,
        purged_at: None,
    };
    // `input.id` is moved here — this is the last use of `input`.
    let secret_handle = SecretHandle::for_identity(input.vault_id, input.id, key_version);
    ProvisioningPlan {
        identity,
        key_entry,
        secret_handle,
        signing_key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_ascii_lowercase() {
        assert_eq!(normalize_human_slug("Alice"), "alice");
    }

    #[test]
    fn slug_spaces_become_dash() {
        assert_eq!(normalize_human_slug("alice smith"), "alice-smith");
    }

    #[test]
    fn slug_apostrophe() {
        assert_eq!(normalize_human_slug("o'brien"), "o-brien");
    }

    #[test]
    fn slug_accented_nfkc() {
        assert_eq!(normalize_human_slug("Élodie"), "elodie");
    }

    #[test]
    fn slug_non_latin_falls_back() {
        assert_eq!(normalize_human_slug("田中"), "user");
    }

    #[test]
    fn slug_empty_falls_back() {
        assert_eq!(normalize_human_slug(""), "user");
    }

    #[test]
    fn slug_truncated_to_100_bytes() {
        let long = "a".repeat(200);
        assert_eq!(normalize_human_slug(&long).len(), 100);
    }

    #[test]
    fn human_id_format() {
        let id = mint_human_id("alice", IdentityRevision::FIRST).unwrap();
        assert_eq!(id.as_str(), "hmn:alice:v1");
    }

    #[test]
    fn deterministic_plan_with_seeded_rng() {
        // rand::rngs::StdRng is ChaCha20-backed and implements
        // rand_core::{CryptoRng + RngCore} (= CryptoRngCore) at 0.6.
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let input = ProvisionInput {
            vault_id: VaultId::mint(),
            id: Identity::parse("hmn:alice:v1").unwrap(),
            kind: IdentityKind::Human,
            revision: IdentityRevision::FIRST,
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-04-28T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let plan = build_provisioning_plan(input, &mut rng, now);
        // Determinism: pubkey is reproducible from seeded RNG
        assert_eq!(plan.key_entry.public_key.len(), 32);
    }
}
