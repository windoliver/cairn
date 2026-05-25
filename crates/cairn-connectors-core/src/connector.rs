//! `Connector` trait + plugin marker (issue #130, brief §9.1 source sensors, §19 v0.3).
//!
//! `CONTRACT_VERSION` is the authoritative contract version constant for the
//! `Connector` surface. It is re-exported from `crate` root, replacing the
//! scaffold placeholder.

use std::sync::Arc;

use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::Identity;

use crate::credential::CredentialHandle;
use crate::error::ConnectorError;
use crate::event::ConnectorEvent;
use crate::manifest::ConnectorManifest;
use crate::webhook::WebhookRequest;

/// Contract version for the `Connector` trait surface.
/// Bumped when the trait API changes in a breaking way.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 1, 0);

/// Which optional capabilities this connector instance supports.
///
/// The framework gates `poll` / `ingest_webhook` calls behind the
/// corresponding bool; connectors that advertise `false` must still
/// implement the method body (return `ConnectorError::Transient` or
/// an empty vec), but the framework will never call them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
// Three independent capability dimensions — each bool has a distinct meaning
// and collapsing them into bitflags would lose that clarity.
#[allow(clippy::struct_excessive_bools)]
pub struct ConnectorCapabilities {
    /// Connector supports periodic poll-based ingestion.
    pub poll: bool,
    /// Connector accepts inbound webhook pushes.
    pub webhook: bool,
    /// Connector supports historical backfill (cursor rewound to epoch).
    pub backfill: bool,
}

/// Per-call context handed to [`Connector::poll`].
pub struct PollContext {
    /// Credentials for the connector's OAuth scope (placeholder until T9).
    pub credentials: Arc<CredentialHandle>,
    /// Opaque cursor from the previous successful poll, if any.
    pub last_cursor: Option<String>,
    /// How many items remain in the per-scope budget for this call.
    pub budget_remaining_items: u32,
}

/// Per-call context handed to [`Connector::ingest_webhook`].
pub struct WebhookContext {
    /// Credentials for the connector's OAuth scope (placeholder until T9).
    pub credentials: Arc<CredentialHandle>,
    /// How many items remain in the per-scope budget for this call.
    pub budget_remaining_items: u32,
}

/// Result of one [`Connector::poll`] invocation.
///
/// The framework persists `next_cursor` in the registry entry's state so the
/// subsequent poll starts where this one left off. `rate_limit_hint` lets the
/// connector signal an upstream backoff without going through an error path.
#[derive(Debug, Default)]
pub struct PollOutcome {
    /// Events produced in this poll window.
    pub events: Vec<ConnectorEvent>,
    /// Opaque cursor to hand back on the next poll, or `None` if the
    /// connector has reached the head and wants the scheduler to decide.
    pub next_cursor: Option<String>,
    /// Optional upstream-requested delay before the next poll.
    pub rate_limit_hint: Option<std::time::Duration>,
}

/// Core async trait every connector adapter must implement.
///
/// Object-safe (`Box<dyn Connector>` compiles). Implementations must be
/// `Send + Sync` so the framework can hold them across await points and
/// hand them to a tokio task.
#[async_trait::async_trait]
pub trait Connector: Send + Sync {
    /// Stable identifier matching the manifest `[connector] name` field.
    fn name(&self) -> &str;

    /// Connector-supplied manifest describing capabilities, labels, budgets,
    /// OAuth requirements, and payload limits.
    fn manifest(&self) -> &ConnectorManifest;

    /// Which optional capabilities this instance actually supports.
    fn capabilities(&self) -> &ConnectorCapabilities;

    /// `Identity` registered with `cairn-core` for traceability (brief §9.1).
    fn sensor_identity(&self) -> &Identity;

    /// Range of `CONTRACT_VERSION` values this connector was compiled against.
    fn supported_contract_versions(&self) -> VersionRange;

    /// Poll the upstream for new events since `cx.last_cursor`.
    ///
    /// Only called by the framework when `capabilities().poll == true`.
    async fn poll(&self, cx: &PollContext) -> Result<PollOutcome, ConnectorError>;

    /// Convert an inbound webhook HTTP request into zero or more events.
    ///
    /// Only called by the framework when `capabilities().webhook == true`.
    /// Signature verification is performed by the framework *before* calling
    /// this method; adapters must not re-verify.
    async fn ingest_webhook(
        &self,
        req: &WebhookRequest,
        cx: &WebhookContext,
    ) -> Result<Vec<ConnectorEvent>, ConnectorError>;
}

/// Marker trait for statically-registered connector plugins.
///
/// Every concrete adapter type implements this in addition to [`Connector`].
/// The framework registry uses the associated constants for compile-time
/// compatibility checks.
pub trait ConnectorPlugin: Connector + Sized {
    /// Same as `Connector::name` but available without an instance.
    const NAME: &'static str;
    /// Range of contract versions this plugin was compiled to support.
    const SUPPORTED_VERSIONS: VersionRange;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub;

    #[async_trait::async_trait]
    impl Connector for Stub {
        fn name(&self) -> &'static str {
            "stub"
        }
        fn manifest(&self) -> &crate::manifest::ConnectorManifest {
            unimplemented!()
        }
        fn capabilities(&self) -> &ConnectorCapabilities {
            static C: ConnectorCapabilities = ConnectorCapabilities {
                poll: true,
                webhook: false,
                backfill: false,
            };
            &C
        }
        fn sensor_identity(&self) -> &cairn_core::domain::Identity {
            unimplemented!()
        }
        fn supported_contract_versions(&self) -> cairn_core::contract::version::VersionRange {
            <Self as ConnectorPlugin>::SUPPORTED_VERSIONS
        }
        async fn poll(&self, _: &PollContext) -> Result<PollOutcome, crate::error::ConnectorError> {
            Ok(PollOutcome::default())
        }
        async fn ingest_webhook(
            &self,
            _: &WebhookRequest,
            _: &WebhookContext,
        ) -> Result<Vec<crate::event::ConnectorEvent>, crate::error::ConnectorError> {
            Ok(vec![])
        }
    }

    impl ConnectorPlugin for Stub {
        const NAME: &'static str = "stub";
        const SUPPORTED_VERSIONS: cairn_core::contract::version::VersionRange =
            cairn_core::contract::version::VersionRange::new(
                cairn_core::contract::version::ContractVersion::new(0, 1, 0),
                cairn_core::contract::version::ContractVersion::new(0, 2, 0),
            );
    }

    #[test]
    fn dyn_compatible() {
        let _b: Box<dyn Connector> = Box::new(Stub);
    }
}
