//! Integration test for the `ConnectorConsentJournal` trait (issue #130,
//! brief §14 + §19 v0.3). Verifies the full grant → lookup → revoke
//! lifecycle using an in-process stub implementation.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use cairn_core::contract::connector_consent::{
    ConnectorConsentJournal, ConnectorConsentLookup, ConsentGrant, ConsentGrantId,
};
use cairn_core::domain::Identity;

#[derive(Default)]
struct StubJournal {
    grants: Mutex<HashMap<ConsentGrantId, ConsentGrant>>,
    counter: Mutex<u64>,
}

#[async_trait::async_trait]
impl ConnectorConsentJournal for StubJournal {
    async fn put_grant(&self, grant: ConsentGrant) -> Result<ConsentGrantId, String> {
        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        let id = ConsentGrantId::new(format!("gnt:{}:{}", grant.connector, *counter));
        self.grants.lock().unwrap().insert(id.clone(), grant);
        Ok(id)
    }

    async fn lookup(
        &self,
        connector: &str,
        _scope_key: &str,
    ) -> Result<ConnectorConsentLookup, String> {
        let g = self.grants.lock().unwrap();
        Ok(if g.values().any(|g| g.connector == connector) {
            ConnectorConsentLookup::Granted
        } else {
            ConnectorConsentLookup::Revoked
        })
    }

    async fn revoke(&self, id: &ConsentGrantId) -> Result<(), String> {
        self.grants.lock().unwrap().remove(id);
        Ok(())
    }
}

#[tokio::test]
async fn grant_then_lookup_then_revoke() {
    // Verify `Box<dyn ConnectorConsentJournal>` compiles (object-safety check).
    let _: Box<dyn ConnectorConsentJournal> = Box::new(StubJournal::default());

    let journal: Arc<dyn ConnectorConsentJournal> = Arc::new(StubJournal::default());
    let grant = ConsentGrant::new(
        "fixture",
        "h1",
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".into()],
        0,
        Identity::parse("hmn:alice").expect("valid human identity"),
    );
    let id = journal.put_grant(grant).await.unwrap();
    assert_eq!(
        journal.lookup("fixture", "project:any").await.unwrap(),
        ConnectorConsentLookup::Granted
    );
    journal.revoke(&id).await.unwrap();
    assert_eq!(
        journal.lookup("fixture", "project:any").await.unwrap(),
        ConnectorConsentLookup::Revoked
    );
}

#[tokio::test]
async fn revoke_only_targets_the_named_grant() {
    let journal: Arc<dyn ConnectorConsentJournal> = Arc::new(StubJournal::default());

    let grant_a = ConsentGrant::new(
        "fixture-a",
        "h-a",
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".into()],
        0,
        Identity::parse("hmn:alice").expect("valid human identity"),
    );
    let grant_b = ConsentGrant::new(
        "fixture-b",
        "h-b",
        BTreeSet::from(["note".to_string()]),
        vec!["project:*".into()],
        0,
        Identity::parse("hmn:alice").expect("valid human identity"),
    );

    let id_a = journal.put_grant(grant_a).await.unwrap();
    let _id_b = journal.put_grant(grant_b).await.unwrap();

    journal.revoke(&id_a).await.unwrap();

    assert_eq!(
        journal.lookup("fixture-a", "project:any").await.unwrap(),
        ConnectorConsentLookup::Revoked
    );
    assert_eq!(
        journal.lookup("fixture-b", "project:any").await.unwrap(),
        ConnectorConsentLookup::Granted
    );
}
