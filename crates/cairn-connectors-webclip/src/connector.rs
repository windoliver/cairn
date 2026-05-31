//! `WebClipConnector` — `Connector` + `ConnectorPlugin` for web clips.
//!
//! Webhook-only and stateless: `poll` is never called (the manifest declares
//! `poll = false`) and there is no credential cache or HTTP client.

use async_trait::async_trait;
use cairn_connectors_core::{
    CONTRACT_VERSION, Connector, ConnectorCapabilities, ConnectorError, ConnectorEvent,
    ConnectorManifest, ConnectorPlugin, ContractVersion, Identity, PollContext, PollOutcome,
    VersionRange, WebhookContext, WebhookRequest,
};

use crate::MANIFEST_TOML;
use crate::clip;

/// Generic web-clipper connector. Construct once per Cairn process.
pub struct WebClipConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
}

impl WebClipConnector {
    /// Construct a new web-clipper connector from the bundled manifest.
    ///
    /// # Errors
    /// Returns [`ConnectorError::Fatal`] if the bundled manifest or sensor
    /// identity fails to parse (a compile-time invariant, covered by tests).
    pub fn new() -> Result<Self, ConnectorError> {
        let manifest = ConnectorManifest::parse_toml(MANIFEST_TOML)
            .map_err(|e| ConnectorError::fatal_msg(format!("webclip manifest: {e}")))?;
        let sensor = Identity::parse("snr:local:connector:webclip:v1")
            .map_err(|e| ConnectorError::fatal_msg(format!("webclip sensor identity: {e:?}")))?;
        Ok(Self { manifest, sensor })
    }
}

#[async_trait]
impl Connector for WebClipConnector {
    fn name(&self) -> &str {
        self.manifest.name()
    }

    fn manifest(&self) -> &ConnectorManifest {
        &self.manifest
    }

    fn capabilities(&self) -> &ConnectorCapabilities {
        static C: ConnectorCapabilities = ConnectorCapabilities {
            poll: false,
            webhook: true,
            backfill: false,
        };
        &C
    }

    fn sensor_identity(&self) -> &Identity {
        &self.sensor
    }

    fn supported_contract_versions(&self) -> VersionRange {
        <Self as ConnectorPlugin>::SUPPORTED_VERSIONS
    }

    // `poll = false` in the manifest, so the registry never calls this. The
    // trait still requires a body; return an empty outcome.
    async fn poll(&self, _cx: &PollContext) -> Result<PollOutcome, ConnectorError> {
        Ok(PollOutcome::default())
    }

    async fn ingest_webhook(
        &self,
        req: &WebhookRequest,
        _cx: &WebhookContext,
    ) -> Result<Vec<ConnectorEvent>, ConnectorError> {
        // The substrate has already verified the HMAC signature; we use the
        // header value as the surrogate signature_id (same pattern as GitHub).
        let signature_id = req
            .header("X-Cairn-Signature-256")
            .unwrap_or("unverified")
            .to_owned();
        Ok(vec![clip::parse_request(req, &signature_id)?])
    }
}

impl ConnectorPlugin for WebClipConnector {
    const NAME: &'static str = "webclip";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(CONTRACT_VERSION, ContractVersion::new(0, 2, 0));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn constructs_with_expected_identity() {
        let c = WebClipConnector::new().expect("constructs");
        assert_eq!(c.name(), "webclip");
        assert_eq!(
            c.sensor_identity().as_str(),
            "snr:local:connector:webclip:v1"
        );
    }

    #[test]
    fn capabilities_are_webhook_only() {
        let c = WebClipConnector::new().unwrap();
        let caps = c.capabilities();
        assert!(!caps.poll && caps.webhook && !caps.backfill);
    }

    #[test]
    fn is_arc_dyn_connector() {
        let c: Arc<dyn Connector> = Arc::new(WebClipConnector::new().unwrap());
        assert_eq!(c.name(), "webclip");
    }
}
