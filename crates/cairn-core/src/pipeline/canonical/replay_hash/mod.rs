//! Versioned replay-hash for target-scope `source_forget` rows.
//!
//! Issue #257, design doc Component 4. Every supported version `vN`
//! ships three frozen artifacts (input struct, projection, encoder)
//! and an immutable golden-vector set. The chain
//! `project_vN ∘ encode_vN ∘ sha256` is the replay-hash; bumping the
//! domain tag (`cairn.replay_hash.vN`) mints a new hash space.

use sha2::Digest as _;

use crate::domain::MemoryRecord;

pub mod v1;

/// Encoder versions this binary can compute and read. Removing a
/// version is a deprecation cycle requiring an offline
/// journal-rewrite (out of scope for #257).
pub const SUPPORTED_REPLAY_HASH_VERSIONS: &[u32] = &[1];

/// Compute the replay-hash for `record` under `version`. Returns
/// `None` when `version` is not in
/// [`SUPPORTED_REPLAY_HASH_VERSIONS`] — callers fail closed.
#[must_use]
pub fn compute(record: &MemoryRecord, version: u32) -> Option<String> {
    match version {
        1 => Some(sha256_hex(&v1::encode_v1(&v1::project_v1(record)))),
        _ => None,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}
