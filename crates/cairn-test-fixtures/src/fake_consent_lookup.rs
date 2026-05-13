//! In-memory `ConsentLookup` for tests — Issue #253.
//!
//! Map-backed `ConsentLookup` so verb-layer tests (notably the lint
//! `consent` sub-check matrix) can exercise covering-grant resolution
//! without touching `cairn-store-sqlite`.

use std::collections::HashMap;

use async_trait::async_trait;

use cairn_core::contract::consent_lookup::{ConsentLookup, ConsentLookupError};
use cairn_core::domain::consent_timeline::ConsentTimelineEvent;

/// Map-backed `ConsentLookup`. Tests seed it with a `Vec<ConsentTimelineEvent>`
/// per `consent_ref` and pass it as `&dyn ConsentLookup` into `LintInputs`.
#[derive(Debug, Default, Clone)]
pub struct FakeConsentLookup {
    by_ref: HashMap<String, Vec<ConsentTimelineEvent>>,
}

impl FakeConsentLookup {
    /// Empty lookup — every `consent_ref` resolves to no events.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed `consent_ref` with `events`. Replaces any prior entry.
    /// Builder-style.
    #[must_use]
    pub fn with(
        mut self,
        consent_ref: impl Into<String>,
        events: Vec<ConsentTimelineEvent>,
    ) -> Self {
        self.by_ref.insert(consent_ref.into(), events);
        self
    }

    /// Append events to an existing or new `consent_ref` entry.
    pub fn extend(&mut self, consent_ref: impl Into<String>, events: Vec<ConsentTimelineEvent>) {
        self.by_ref
            .entry(consent_ref.into())
            .or_default()
            .extend(events);
    }
}

#[async_trait]
impl ConsentLookup for FakeConsentLookup {
    async fn timeline(
        &self,
        consent_ref: &str,
    ) -> Result<Vec<ConsentTimelineEvent>, ConsentLookupError> {
        Ok(self.by_ref.get(consent_ref).cloned().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::domain::consent_timeline::ConsentTimelineEventKind;
    use cairn_core::domain::{Rfc3339Timestamp, SensorLabel};

    fn ev(consent_ref: &str) -> ConsentTimelineEvent {
        ConsentTimelineEvent {
            consent_ref: consent_ref.to_owned(),
            seq: 1,
            kind: ConsentTimelineEventKind::Issued,
            sensor_id: SensorLabel::parse("local:s:h:v1").expect("invariant: valid sensor"),
            scope: "private".to_owned(),
            decided_at: Rfc3339Timestamp::parse("2026-01-01T00:00:00Z")
                .expect("invariant: valid timestamp"),
            expires_at: None,
        }
    }

    #[tokio::test]
    async fn empty_lookup_returns_empty_vec() {
        let lk = FakeConsentLookup::new();
        let out = lk.timeline("c:missing").await.expect("ok");
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn with_seeds_consent_ref() {
        let lk = FakeConsentLookup::new().with("c:1", vec![ev("c:1")]);
        let out = lk.timeline("c:1").await.expect("ok");
        assert_eq!(out.len(), 1);
        let other = lk.timeline("c:2").await.expect("ok");
        assert!(other.is_empty());
    }

    #[tokio::test]
    async fn extend_appends_to_existing_ref() {
        let mut lk = FakeConsentLookup::new().with("c:1", vec![ev("c:1")]);
        let mut second = ev("c:1");
        second.seq = 2;
        lk.extend("c:1", vec![second]);
        let out = lk.timeline("c:1").await.expect("ok");
        assert_eq!(out.len(), 2);
    }
}
