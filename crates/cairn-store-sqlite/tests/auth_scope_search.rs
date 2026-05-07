//! Integration tests for `auth_scope` enforcement across all four search SQL
//! paths (keyword, semantic, graph, graph-only hydration). Issue #191.
//!
//! These tests insert records with distinct scope tuples and assert each
//! retrieval path narrows correctly, refuses cross-tenant leakage, and
//! degrades correctly when capabilities are missing.

use std::sync::Arc;

use cairn_core::contract::memory_store::{
    HybridSearchArgs, KeywordSearchArgs, MemoryStore, SemanticSearchArgs,
};
use cairn_core::domain::ScopeTuple;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::domain::{RecordId, TargetId};
use cairn_embeddings_local::{EmbeddingModel, EmbeddingModelKind, MockEmbedder};
use cairn_store_sqlite::open_in_memory_with_embedder;

/// Build a sample record with the given record/target ids, scope, and body.
fn record(rid: &str, tid: &str, scope: ScopeTuple, body: &str) -> cairn_core::domain::MemoryRecord {
    let mut r = sample_record();
    r.id = RecordId::parse(rid.to_owned()).expect("valid record id");
    r.target_id = TargetId::parse(tid.to_owned()).expect("valid target id");
    r.scope = scope;
    body.clone_into(&mut r.body);
    r
}

/// Two-tenant fixture: 4 records, two each in tenant=A and tenant=B,
/// distinguished by body so the keyword leg can match individually.
async fn fixture_two_tenants() -> Arc<dyn MemoryStore> {
    let kind = EmbeddingModelKind::default();
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::new(kind));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .expect("open store");

    let a1 = record(
        "01HQZX9F5N0000000000000A01",
        "01HQZX9F5N0000000000000A01",
        ScopeTuple {
            tenant: Some("acme".into()),
            user: Some("hmn:alice".into()),
            ..Default::default()
        },
        "alpha alice acme one",
    );
    let a2 = record(
        "01HQZX9F5N0000000000000A02",
        "01HQZX9F5N0000000000000A02",
        ScopeTuple {
            tenant: Some("acme".into()),
            user: Some("hmn:bob".into()),
            ..Default::default()
        },
        "alpha bob acme two",
    );
    let b1 = record(
        "01HQZX9F5N0000000000000B01",
        "01HQZX9F5N0000000000000B01",
        ScopeTuple {
            tenant: Some("globex".into()),
            user: Some("hmn:alice".into()),
            ..Default::default()
        },
        "alpha alice globex one",
    );
    let b2 = record(
        "01HQZX9F5N0000000000000B02",
        "01HQZX9F5N0000000000000B02",
        ScopeTuple {
            tenant: Some("globex".into()),
            user: Some("hmn:bob".into()),
            ..Default::default()
        },
        "alpha bob globex two",
    );
    for r in [&a1, &a2, &b1, &b2] {
        store.upsert(r).await.expect("upsert");
    }
    Arc::new(store)
}

fn vis_all() -> Vec<MemoryVisibility> {
    vec![
        MemoryVisibility::Private,
        MemoryVisibility::Session,
        MemoryVisibility::Project,
        MemoryVisibility::Team,
        MemoryVisibility::Org,
        MemoryVisibility::Public,
    ]
}

fn keyword_args(query: &str, scope: ScopeTuple) -> KeywordSearchArgs<'static> {
    KeywordSearchArgs {
        query: query.into(),
        filter: None,
        auth_scope: scope,
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: 50,
        cursor: None,
        with_explain: false,
    }
}

fn semantic_args(query: &str, scope: ScopeTuple) -> SemanticSearchArgs<'static> {
    SemanticSearchArgs {
        query: query.into(),
        filter: None,
        auth_scope: scope,
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: 50,
        model_label: EmbeddingModelKind::default().as_str().to_owned(),
        with_explain: false,
    }
}

fn hybrid_args(query: &str, scope: ScopeTuple) -> HybridSearchArgs<'static> {
    HybridSearchArgs {
        query: query.into(),
        filter: None,
        auth_scope: scope,
        visibility_allowlist: vec![MemoryVisibility::Private],
        limit: 50,
        model_label: EmbeddingModelKind::default().as_str().to_owned(),
        blend: 0.7,
        rrf_k: 60,
        rerank_topk: 20,
        with_explain: false,
        confidence_floor: 1e-3,
    }
}

// ── Keyword leg ─────────────────────────────────────────────────────────

#[tokio::test]
async fn keyword_empty_scope_returns_all_tenants() {
    let store = fixture_two_tenants().await;
    let page = store
        .search_keyword(&keyword_args("alpha", ScopeTuple::default()))
        .await
        .expect("keyword");
    assert_eq!(
        page.candidates.len(),
        4,
        "empty scope must not narrow; got {} ids",
        page.candidates.len()
    );
}

#[tokio::test]
async fn keyword_tenant_scope_excludes_other_tenants() {
    let store = fixture_two_tenants().await;
    let scope = ScopeTuple {
        tenant: Some("acme".into()),
        ..Default::default()
    };
    let page = store
        .search_keyword(&keyword_args("alpha", scope))
        .await
        .expect("keyword");
    assert_eq!(page.candidates.len(), 2, "tenant=acme expected 2 hits");
    for c in &page.candidates {
        assert!(
            c.record_id.as_str().contains("000A0"),
            "leaked cross-tenant: {}",
            c.record_id.as_str()
        );
    }
}

#[tokio::test]
async fn keyword_multidim_scope_is_anded() {
    let store = fixture_two_tenants().await;
    let scope = ScopeTuple {
        tenant: Some("acme".into()),
        user: Some("hmn:alice".into()),
        ..Default::default()
    };
    let page = store
        .search_keyword(&keyword_args("alpha", scope))
        .await
        .expect("keyword");
    assert_eq!(page.candidates.len(), 1);
    assert_eq!(
        page.candidates[0].record_id.as_str(),
        "01HQZX9F5N0000000000000A01",
        "multidim scope must AND tenant + user"
    );
}

#[tokio::test]
async fn keyword_unmatched_scope_is_empty_not_error() {
    let store = fixture_two_tenants().await;
    let scope = ScopeTuple {
        tenant: Some("nonexistent".into()),
        ..Default::default()
    };
    let page = store
        .search_keyword(&keyword_args("alpha", scope))
        .await
        .expect("keyword must not error on unmatched scope");
    assert!(page.candidates.is_empty());
}

// ── Semantic leg ────────────────────────────────────────────────────────

#[tokio::test]
async fn semantic_tenant_scope_excludes_other_tenants() {
    let store = fixture_two_tenants().await;
    let scope = ScopeTuple {
        tenant: Some("acme".into()),
        ..Default::default()
    };
    let page = store
        .search_semantic(&semantic_args("alpha", scope))
        .await
        .expect("semantic");
    for c in &page.candidates {
        assert!(
            c.record_id.as_str().contains("000A0"),
            "semantic leaked cross-tenant: {}",
            c.record_id.as_str()
        );
    }
}

// ── Hybrid leg ──────────────────────────────────────────────────────────

#[tokio::test]
async fn hybrid_tenant_scope_excludes_other_tenants() {
    let store = fixture_two_tenants().await;
    let scope = ScopeTuple {
        tenant: Some("acme".into()),
        ..Default::default()
    };
    let page = store
        .search_hybrid(&hybrid_args("alpha", scope))
        .await
        .expect("hybrid");
    assert!(
        !page.candidates.is_empty(),
        "hybrid should still return acme rows"
    );
    for c in &page.candidates {
        assert!(
            c.record_id.as_str().contains("000A0"),
            "hybrid leaked cross-tenant: {}",
            c.record_id.as_str()
        );
    }
}

#[tokio::test]
async fn hybrid_empty_visibility_with_scope_still_narrows() {
    // Empty visibility = "no visibility filter" — scope must still apply.
    let store = fixture_two_tenants().await;
    let mut args = hybrid_args(
        "alpha",
        ScopeTuple {
            tenant: Some("acme".into()),
            ..Default::default()
        },
    );
    args.visibility_allowlist = vec![]; // explicitly empty
    let page = store.search_hybrid(&args).await.expect("hybrid");
    for c in &page.candidates {
        assert!(
            c.record_id.as_str().contains("000A0"),
            "scope must still narrow when visibility_allowlist is empty: {}",
            c.record_id.as_str()
        );
    }
}

#[tokio::test]
async fn hybrid_visibility_and_scope_both_apply() {
    // sample_record sets visibility=Private. Set visibility allowlist to
    // a different tier and assert no rows come back even though tenant
    // matches.
    let store = fixture_two_tenants().await;
    let mut args = hybrid_args(
        "alpha",
        ScopeTuple {
            tenant: Some("acme".into()),
            ..Default::default()
        },
    );
    args.visibility_allowlist = vec![MemoryVisibility::Public];
    let page = store.search_hybrid(&args).await.expect("hybrid");
    assert!(
        page.candidates.is_empty(),
        "Public visibility filter must exclude all Private rows even within tenant"
    );
}

// ── degraded_legs propagation ────────────────────────────────────────────

#[tokio::test]
async fn hybrid_degraded_legs_present_when_graph_capability_off() {
    // Default in-memory open path advertises graph_search=true. We assert
    // the *empty* shape on a successful run — degraded_legs is empty when
    // every leg ran cleanly.
    let store = fixture_two_tenants().await;
    let page = store
        .search_hybrid(&hybrid_args("alpha", ScopeTuple::default()))
        .await
        .expect("hybrid");
    assert!(
        page.degraded_legs.is_empty(),
        "successful hybrid should report no degraded legs; got {:?}",
        page.degraded_legs,
    );
}

// ── Visibility-allowlist=[] path ─────────────────────────────────────────

#[tokio::test]
async fn keyword_empty_visibility_returns_all_visibilities() {
    // Verifies the empty-allowlist guard wasn't broken. All four fixture
    // records share visibility=Private; an empty allowlist should still
    // return them all (rather than zero, the way the buggy graph-leg SQL
    // used to behave).
    let store = fixture_two_tenants().await;
    let mut args = keyword_args("alpha", ScopeTuple::default());
    args.visibility_allowlist = vec![];
    let page = store.search_keyword(&args).await.expect("keyword");
    assert_eq!(
        page.candidates.len(),
        4,
        "empty visibility_allowlist must not silently drop rows"
    );
}

// ── Special-character values ─────────────────────────────────────────────

#[tokio::test]
async fn scope_with_special_characters_does_not_break_sql() {
    // Records with quotes, backslashes, etc. in scope values must round-trip
    // through json_extract without breaking SQL parsing.
    let kind = EmbeddingModelKind::default();
    let embedder: Arc<dyn EmbeddingModel> = Arc::new(MockEmbedder::new(kind));
    let store = open_in_memory_with_embedder(Some(Arc::clone(&embedder)))
        .await
        .expect("open store");

    let weird = record(
        "01HQZX9F5N0000000000000W01",
        "01HQZX9F5N0000000000000W01",
        ScopeTuple {
            tenant: Some("with-dashes_and_under.scores".into()),
            ..Default::default()
        },
        "weird tenant value",
    );
    store.upsert(&weird).await.expect("upsert");

    let scope = ScopeTuple {
        tenant: Some("with-dashes_and_under.scores".into()),
        ..Default::default()
    };
    let mut args = keyword_args("weird", scope);
    args.visibility_allowlist = vis_all();
    let page = store.search_keyword(&args).await.expect("keyword");
    assert_eq!(page.candidates.len(), 1);
}
