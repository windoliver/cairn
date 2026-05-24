//! Integration test for the `ConnectorConsentJournal` trait (issue #130,
//! brief §14 + §19 v0.3). Verifies the full grant → lookup → revoke
//! lifecycle using an in-process stub implementation.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Mutex;

use cairn_core::contract::connector_consent::{
    ConnectorConsentJournal, ConsentGrant, ConsentGrantId, ConnectorConsentLookup,
};
use cairn_core::domain::Identity;

#[derive(Default)]
struct StubJournal {
    grants: Mutex<Vec<ConsentGrant>>,
}

#[async_trait::async_trait]
impl ConnectorConsentJournal for StubJournal {
    async fn put_grant(&self, grant: ConsentGrant) -> Result<ConsentGrantId, String> {
        let id = ConsentGrantId::new(format!("gnt:{}", grant.connector));
        self.grants.lock().unwrap().push(grant);
        Ok(id)
    }
    async fn lookup(
        &self,
        connector: &str,
        _scope_key: &str,
    ) -> Result<ConnectorConsentLookup, String> {
        let g = self.grants.lock().unwrap();
        Ok(if g.iter().any(|g| g.connector == connector) {
            ConnectorConsentLookup::Granted
        } else {
            ConnectorConsentLookup::Revoked
        })
    }
    async fn revoke(&self, _id: &ConsentGrantId) -> Result<(), String> {
        self.grants.lock().unwrap().clear();
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
