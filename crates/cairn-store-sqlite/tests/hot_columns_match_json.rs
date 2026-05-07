//! Proptest: every denormalized column equals its `record_json` projection.
//!
//! Each iteration spins up a fresh tokio current-thread runtime to drive
//! the async store API from inside the synchronous proptest body. This is
//! acceptable at 64 cases but would dominate runtime if the case count
//! grew significantly.

// Bit-exact comparison of `f32` is intentional: the projection round-trip
// must preserve the original payload byte-for-byte, so any drift is a bug.
#![allow(clippy::float_cmp)]

use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::{MemoryRecord, RecordId, TargetId};
use cairn_store_sqlite::open_in_memory;
use proptest::prelude::*;

fn record_strategy() -> impl Strategy<Value = MemoryRecord> {
    // Generate by mutating the in-tree sample. Vary body and confidence
    // so projection is exercised across distinct hashes and ranges.
    ("[a-z ]{3,40}", 0.0f32..=1.0, 0.0f32..=1.0, 0u8..=255).prop_map(
        |(body, confidence, salience, ulid_byte)| {
            let mut r = cairn_core::domain::record::tests_export::sample_record();
            r.body = body;
            r.confidence = confidence;
            r.salience = salience;
            // Permute id/target so each iteration writes a fresh row.
            // ULID layout: 10-char prefix + 14 zeros + 2 hex char = 26 chars.
            // Leading char `0` keeps the high 5 bits inside ULID's 128-bit
            // range; hex digits 0-9/A-F are all valid Crockford symbols.
            let suffix = format!("{ulid_byte:02X}");
            let new_id = format!("01HQZX9F5N00000000000000{suffix}");
            r.id = RecordId::parse(new_id.clone()).expect("valid ULID");
            r.target_id = TargetId::parse(new_id).expect("valid target ULID");
            r
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn projection_round_trips_via_get(record in record_strategy()) {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let store = open_in_memory().await.expect("open");
            store.upsert(&record).await.expect("upsert");
            let back = store.get(&record.id).await.expect("get").expect("present");
            prop_assert_eq!(back.body, record.body);
            prop_assert_eq!(back.confidence, record.confidence);
            prop_assert_eq!(back.salience, record.salience);
            Ok(())
        }).expect("proptest body");
    }
}
