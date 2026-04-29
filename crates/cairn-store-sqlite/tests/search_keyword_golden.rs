//! Golden keyword query tests (issue #47, brief §15 / §18.c US7).
//!
//! Drives `search_keyword` through the canonical wire fixtures under
//! `fixtures/v0/search-filters/`. The fixtures pin the *input* shape;
//! the assertions here pin the *result* shape against a deterministic
//! corpus so that any regression in either the FTS query, the filter
//! compiler, or the row projection trips a test failure.

use std::path::PathBuf;

use cairn_core::contract::memory_store::{KeywordSearchArgs, MemoryStore};
use cairn_core::domain::filter::{ValidatedFilter, validate_filter};
use cairn_core::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
use cairn_core::domain::{MemoryRecord, RecordId, TargetId};
use cairn_core::generated::verbs::search::{SearchArgs, SearchArgsFilters};
use cairn_store_sqlite::{SqliteMemoryStore, open_in_memory};

fn fixture_path(name: &str) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut p = manifest_dir
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf();
    p.push("fixtures/v0/search-filters");
    p.push(name);
    p
}

fn load_search_args(name: &str) -> SearchArgs {
    let raw = std::fs::read_to_string(fixture_path(name)).expect("read fixture");
    serde_json::from_str(&raw).expect("parse SearchArgs")
}

fn load_filter(name: &str) -> SearchArgsFilters {
    let raw = std::fs::read_to_string(fixture_path(name)).expect("read fixture");
    serde_json::from_str(&raw).expect("parse SearchArgsFilters")
}

fn record(
    seed: char,
    body: &str,
    kind: MemoryKind,
    class: MemoryClass,
    visibility: MemoryVisibility,
) -> MemoryRecord {
    let mut r = cairn_core::domain::record::tests_export::sample_record();
    let mut id = String::from("01HQZX9F5N000000000000000");
    id.push(seed.to_ascii_uppercase());
    r.id = RecordId::parse(id.clone()).expect("valid id");
    r.target_id = TargetId::parse(id).expect("valid target");
    body.clone_into(&mut r.body);
    r.kind = kind;
    r.class = class;
    r.visibility = visibility;
    r
}

/// Seeds a five-row corpus designed to exercise the §18.c US7 keyword
/// search story: a `user` memory matches a "dark mode vim" query, a
/// `feedback` memory mentions vim but in a different kind, a public
/// record exists for the visibility filter, and two non-matching rows
/// pad the corpus so the query is doing real selection work.
async fn seed() -> SqliteMemoryStore {
    let store = open_in_memory().await.expect("open");
    store
        .upsert(&record(
            '1',
            "dark mode vim keybindings preferred",
            MemoryKind::User,
            MemoryClass::Semantic,
            MemoryVisibility::Private,
        ))
        .await
        .expect("seed r1");
    store
        .upsert(&record(
            '2',
            "vim keybindings shipped to project",
            MemoryKind::Feedback,
            MemoryClass::Semantic,
            MemoryVisibility::Private,
        ))
        .await
        .expect("seed r2");
    store
        .upsert(&record(
            '3',
            "dark mode public announcement",
            MemoryKind::User,
            MemoryClass::Semantic,
            MemoryVisibility::Public,
        ))
        .await
        .expect("seed r3");
    store
        .upsert(&record(
            '4',
            "rule: always ship behind feature flag",
            MemoryKind::Rule,
            MemoryClass::Procedural,
            MemoryVisibility::Private,
        ))
        .await
        .expect("seed r4");
    store
        .upsert(&record(
            '5',
            "user prefers light mode in summer",
            MemoryKind::User,
            MemoryClass::Semantic,
            MemoryVisibility::Private,
        ))
        .await
        .expect("seed r5");
    store
}

fn validated(filter: &SearchArgsFilters) -> ValidatedFilter<'_> {
    validate_filter(filter).expect("filter validates")
}

fn ids(page: &cairn_core::contract::memory_store::KeywordSearchPage) -> Vec<String> {
    page.candidates
        .iter()
        .map(|c| c.record_id.as_str().to_owned())
        .collect()
}

// ── §18.c US7 — keyword + and(kind=user, visibility=private) ──────────────────

#[tokio::test]
async fn golden_search_args_keyword_filters_to_user_private() {
    let store = seed().await;
    let args = load_search_args("search-args-keyword.json");
    let filter = args.filters.expect("fixture has filters");
    let validated = validated(&filter);

    let limit = usize::try_from(args.limit.expect("fixture sets limit")).expect("limit fits");
    let key_args = KeywordSearchArgs {
        query: args.query.clone(),
        filter: Some(validated),
        visibility_allowlist: vec![
            MemoryVisibility::Private,
            MemoryVisibility::Public,
        ],
        limit,
        cursor: None,
    };

    let page = store.search_keyword(&key_args).await.expect("search");
    // Only r1 satisfies query terms AND kind=user AND visibility=private.
    // r3 matches "dark mode" but is public; r2 mentions vim but kind=feedback;
    // r4/r5 are off-topic.
    assert_eq!(
        ids(&page),
        vec!["01HQZX9F5N0000000000000001".to_owned()],
        "golden: only r1 matches the §18.c US7 fixture",
    );
}

// ── leaf-eq.json — single equality filter against keyword ─────────────────────

#[tokio::test]
async fn golden_leaf_eq_filter_narrows_to_user_kind() {
    let store = seed().await;
    let filter = load_filter("leaf-eq.json");
    let validated = validated(&filter);

    let key_args = KeywordSearchArgs {
        query: "dark".to_owned(),
        filter: Some(validated),
        visibility_allowlist: vec![
            MemoryVisibility::Private,
            MemoryVisibility::Public,
        ],
        limit: 10,
        cursor: None,
    };

    let page = store.search_keyword(&key_args).await.expect("search");
    let mut got = ids(&page);
    got.sort();
    assert_eq!(
        got,
        vec![
            "01HQZX9F5N0000000000000001".to_owned(),
            "01HQZX9F5N0000000000000003".to_owned(),
        ],
        "leaf-eq: kind=user matches r1 + r3 (both contain 'dark')",
    );
}

// ── or-with-not.json — disjunction with negation across visibility ────────────

#[tokio::test]
async fn golden_or_with_not_admits_public_or_non_private() {
    let store = seed().await;
    let filter = load_filter("or-with-not.json");
    let validated = validated(&filter);

    let key_args = KeywordSearchArgs {
        query: "dark OR vim".to_owned(),
        filter: Some(validated),
        visibility_allowlist: vec![
            MemoryVisibility::Private,
            MemoryVisibility::Public,
        ],
        limit: 10,
        cursor: None,
    };

    let page = store.search_keyword(&key_args).await.expect("search");
    let mut got = ids(&page);
    got.sort();
    // The fixture filter is `kind in [user,feedback,rule] OR not(visibility=private)`.
    // r1 (user, private): kind branch ✓. r2 (feedback, private): kind ✓.
    // r3 (user, public): kind ✓ AND not-private ✓. r4/r5 don't match query.
    assert_eq!(
        got,
        vec![
            "01HQZX9F5N0000000000000001".to_owned(),
            "01HQZX9F5N0000000000000002".to_owned(),
            "01HQZX9F5N0000000000000003".to_owned(),
        ],
    );
}
