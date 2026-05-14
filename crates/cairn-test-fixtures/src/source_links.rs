//! Factory helpers for the six source-link hygiene fixtures
//! enumerated in the issue #257 design doc Component 10.
//!
//! Each builder returns a [`SourceLinkFixture`] carrying the records,
//! resolver byte map, and consent-journal rows that drive one of the
//! seven lint findings. Tests pair these with the in-memory
//! `StaticResolver` / `StaticJournal` adapters or `tempfile`-backed
//! vaults — the fixtures themselves stay I/O free.

use std::collections::HashMap;

use cairn_core::contract::{SourceForget, TargetReplayKey};
use cairn_core::domain::{MemoryRecord, SourceRef};
use sha2::Digest as _;

/// Source bytes + journal state needed to reproduce one source-link
/// lint scenario. Records are positioned for upsert into a
/// [`crate::store::FixtureStore`]; the byte map feeds a static
/// resolver; the journal rows feed a static `ConsentJournalReader`.
pub struct SourceLinkFixture {
    /// Records that will be lint-targets in this fixture.
    pub records: Vec<MemoryRecord>,
    /// Logical source id -> raw source bytes the resolver returns.
    pub source_bytes: HashMap<String, Vec<u8>>,
    /// Source-forget rows the journal returns.
    pub source_forgets: Vec<SourceForget>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    format!("sha256:{:x}", h.finalize())
}

fn modern(seed: u64, source_id: &str, bytes: &[u8]) -> MemoryRecord {
    let mut r = crate::sample_record(seed);
    let hash = sha256_hex(bytes);
    hash.clone_into(&mut r.provenance.source_hash);
    r.provenance.source_refs = vec![SourceRef {
        id: source_id.to_owned(),
        hash,
    }];
    r
}

fn legacy(seed: u64, source_hash: String) -> MemoryRecord {
    let mut r = crate::sample_record(seed);
    r.provenance.source_hash = source_hash;
    r.provenance.source_refs.clear();
    r
}

/// One modern record + its matching source file. Zero findings.
#[must_use]
pub fn clean() -> SourceLinkFixture {
    let bytes = b"clean source bytes".to_vec();
    let id = "sources/chat/session-clean.md";
    let r = modern(1, id, &bytes);
    SourceLinkFixture {
        records: vec![r],
        source_bytes: HashMap::from([(id.to_owned(), bytes)]),
        source_forgets: Vec::new(),
    }
}

/// Record with `source_refs: []`. Drives `source_link_missing`.
#[must_use]
pub fn empty_source_refs() -> SourceLinkFixture {
    let mut r = crate::sample_record(2);
    r.provenance.source_refs.clear();
    SourceLinkFixture {
        records: vec![r],
        source_bytes: HashMap::new(),
        source_forgets: Vec::new(),
    }
}

/// Record references a source the resolver cannot find. Drives
/// `source_link_dangling`.
#[must_use]
pub fn dangling() -> SourceLinkFixture {
    let bytes = b"dangling-source-bytes".to_vec();
    let id = "sources/chat/session-dangling.md";
    let r = modern(3, id, &bytes);
    SourceLinkFixture {
        records: vec![r],
        source_bytes: HashMap::new(),
        source_forgets: Vec::new(),
    }
}

/// Resolver returns bytes that don't match the record's source hash.
/// Drives `source_hash_mismatch`.
#[must_use]
pub fn hash_mismatch() -> SourceLinkFixture {
    let bytes = b"original source bytes".to_vec();
    let id = "sources/chat/session-mutated.md";
    let r = modern(4, id, &bytes);
    SourceLinkFixture {
        records: vec![r],
        source_bytes: HashMap::from([(id.to_owned(), b"mutated source bytes".to_vec())]),
        source_forgets: Vec::new(),
    }
}

/// Source was forgotten, record still active. Drives
/// `source_after_forget` (source scope).
#[must_use]
pub fn forgotten_still_referenced() -> SourceLinkFixture {
    let bytes = b"forgotten source bytes".to_vec();
    let id = "sources/chat/session-forgotten.md";
    let r = modern(5, id, &bytes);
    let hash = r.provenance.source_refs[0].hash.clone();
    SourceLinkFixture {
        records: vec![r],
        source_bytes: HashMap::from([(id.to_owned(), bytes)]),
        source_forgets: vec![SourceForget {
            op_id: "forget-op-forgotten".to_owned(),
            source_id: id.to_owned(),
            source_bytes_hash: hash,
            target: None,
        }],
    }
}

/// `redact_on_forget: true`, but the forgotten source file still
/// carries its original bytes. Drives `source_redact_skipped`.
#[must_use]
pub fn redact_skipped() -> SourceLinkFixture {
    let bytes = b"redact-skipped bytes".to_vec();
    let id = "sources/chat/session-redact.md";
    let r = modern(6, id, &bytes);
    let hash = r.provenance.source_refs[0].hash.clone();
    SourceLinkFixture {
        records: vec![r],
        source_bytes: HashMap::from([(id.to_owned(), bytes)]),
        source_forgets: vec![SourceForget {
            op_id: "forget-op-redact".to_owned(),
            source_id: id.to_owned(),
            source_bytes_hash: hash,
            target: None,
        }],
    }
}

/// Modern record + legacy record that share a source hash but the
/// legacy one has empty `source_refs`. Drives
/// `source_link_legacy_duplicate`.
#[must_use]
pub fn legacy_duplicate() -> SourceLinkFixture {
    let bytes = b"legacy-duplicate bytes".to_vec();
    let id = "sources/chat/session-legacy.md";
    let m = modern(7, id, &bytes);
    let l = legacy(8, m.provenance.source_hash.clone());
    SourceLinkFixture {
        records: vec![m, l],
        source_bytes: HashMap::from([(id.to_owned(), bytes)]),
        source_forgets: Vec::new(),
    }
}

/// Target-scope forget under v1 of the replay-hash. Drives
/// `source_after_forget` (target scope).
///
/// # Panics
/// Panics if the v1 replay-hash encoder is unexpectedly missing — a
/// programmer error since v1 is in [`cairn_core::pipeline::canonical::replay_hash::SUPPORTED_REPLAY_HASH_VERSIONS`].
#[must_use]
#[allow(clippy::expect_used)]
pub fn target_scope_forget() -> SourceLinkFixture {
    use cairn_core::pipeline::canonical::replay_hash;

    let bytes = b"target-scope source bytes".to_vec();
    let id = "sources/chat/session-target.md";
    let r = modern(9, id, &bytes);
    let replay = replay_hash::compute(&r, 1).expect("v1 supported");
    SourceLinkFixture {
        records: vec![r],
        source_bytes: HashMap::from([(id.to_owned(), bytes)]),
        source_forgets: vec![SourceForget {
            op_id: "forget-op-target".to_owned(),
            source_id: id.to_owned(),
            source_bytes_hash: format!("sha256:{}", "f".repeat(64)),
            target: Some(TargetReplayKey {
                hash: replay,
                version: 1,
            }),
        }],
    }
}
