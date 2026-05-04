//! Issue #51 acceptance criterion 1: an invalid envelope must never
//! reach WAL preparation.
//!
//! `verify_signed_intent` is structurally pure — it accepts no `Store`
//! handle and has no I/O surface. This regression test pins that
//! property by snapshotting every `sqlite_master` table's row count
//! before each rejection and asserting it is unchanged after. If a
//! future refactor accidentally couples the verifier to a store, this
//! test trips before the change lands.

use std::time::SystemTime;

use cairn_core::contract::issuer_key_resolver::{KeyLifecycle, ResolvedKey};
use cairn_core::domain::timestamp::Rfc3339Timestamp;
use cairn_core::generated::common::Ed25519Signature;
use cairn_core::generated::envelope::{SignedIntent, SignedIntentScopeTier};
use cairn_core::verifier::verify_signed_intent;
use cairn_test_fixtures::signed_intent::{FakeIssuerKeyResolver, SignedIntentFixture};
use tokio_rusqlite::params;

async fn snapshot_counts(store: &cairn_store_sqlite::SqliteMemoryStore) -> Vec<(String, i64)> {
    let conn = store.raw_conn().expect("raw_conn").clone();
    conn.call(|c| {
        let mut tables: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        tables.sort();
        let mut out = Vec::with_capacity(tables.len());
        for t in tables {
            let count: i64 =
                c.query_row(&format!("SELECT COUNT(*) FROM \"{t}\""), params![], |r| {
                    r.get(0)
                })?;
            out.push((t, count));
        }
        Ok(out)
    })
    .await
    .expect("count tables")
}

async fn assert_no_writes(
    label: &str,
    intent: SignedIntent,
    resolver: &FakeIssuerKeyResolver,
    now: SystemTime,
) {
    let store = cairn_test_fixtures::memstore().await;
    let before = snapshot_counts(&store).await;
    let outcome = verify_signed_intent(intent, resolver, now).await;
    assert!(
        outcome.is_err(),
        "[{label}] expected verify to reject, got Ok"
    );
    let after = snapshot_counts(&store).await;
    assert_eq!(
        before, after,
        "[{label}] DB row counts changed after a rejection"
    );
}

#[tokio::test]
async fn rejects_tampered_signature_no_writes() {
    let (mut intent, resolver, now) = SignedIntentFixture::default().build();
    // Flip one hex character so the signature decodes but does not verify.
    let mut chars: Vec<char> = intent.signature.0.chars().collect();
    let idx = chars.len() - 5;
    chars[idx] = if chars[idx] == 'a' { 'b' } else { 'a' };
    intent.signature = Ed25519Signature(chars.into_iter().collect());
    assert_no_writes("tamper", intent, &resolver, now).await;
}

#[tokio::test]
async fn rejects_skewed_no_writes() {
    use chrono::{DateTime, Utc};
    let (intent, resolver, _) = SignedIntentFixture::default().build();
    let now: SystemTime = DateTime::parse_from_rfc3339("2026-04-22T15:30:00Z")
        .expect("rfc3339")
        .with_timezone(&Utc)
        .into();
    assert_no_writes("skewed", intent, &resolver, now).await;
}

#[tokio::test]
async fn rejects_past_no_writes() {
    use chrono::{DateTime, Utc};
    // Set issued_at and expires_at so the (issued_at − now) skew stays under
    // 2 min (skips Skewed) but `now > expires_at` triggers Past.
    let (intent, resolver, _) = SignedIntentFixture {
        issued_at: "2026-04-22T14:02:11Z".to_owned(),
        expires_at: "2026-04-22T14:03:11Z".to_owned(),
        ..SignedIntentFixture::default()
    }
    .build();
    let now: SystemTime = DateTime::parse_from_rfc3339("2026-04-22T14:03:33Z")
        .expect("rfc3339")
        .with_timezone(&Utc)
        .into();
    assert_no_writes("past", intent, &resolver, now).await;
}

#[tokio::test]
async fn rejects_ttl_exceeded_no_writes() {
    let (mut intent, resolver, now) = SignedIntentFixture::default().build();
    intent.expires_at = "2026-04-22T14:30:11Z".to_owned(); // 28 min > 5 min cap
    assert_no_writes("ttl", intent, &resolver, now).await;
}

#[tokio::test]
async fn rejects_unknown_key_no_writes() {
    let (intent, _resolver, now) = SignedIntentFixture::default().build();
    let empty = FakeIssuerKeyResolver::new(); // empty table → UnknownKey
    assert_no_writes("unknown", intent, &empty, now).await;
}

#[tokio::test]
async fn rejects_revoked_no_writes() {
    let (intent, resolver, now) = SignedIntentFixture::default().build();
    // Override the resolver entry to Revoked-pre-issued_at.
    resolver.set(
        "hmn:tafeng",
        1,
        Some(ResolvedKey {
            public_key: [9_u8; 32],
            lifecycle: KeyLifecycle::Revoked {
                effective_at: Rfc3339Timestamp::parse("2026-04-22T14:00:00Z".to_owned())
                    .expect("rfc3339"),
            },
        }),
    );
    assert_no_writes("revoked", intent, &resolver, now).await;
}

#[tokio::test]
async fn rejects_scope_denied_no_writes() {
    let (intent, resolver, now) = SignedIntentFixture {
        issuer: "agt:bot:opus:role:v1".to_owned(),
        tier: SignedIntentScopeTier::Team,
        ..SignedIntentFixture::default()
    }
    .build();
    assert_no_writes("scope", intent, &resolver, now).await;
}
