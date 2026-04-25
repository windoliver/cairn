//! `FrontendAdapter` contract — P1 forward stub (brief §4 row 7).
//!
//! Surface frozen here so the registry has a slot for editor / desktop /
//! plugin adapters. Full method surface and conformance suite ship with
//! v0.2 in #113.

use crate::contract::version::{ContractVersion, VersionRange};

/// Contract version for `FrontendAdapter`. Bumps when the trait surface changes.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 0, 1);

/// Static capability declaration for a `FrontendAdapter` impl.
// Three flags cover distinct frontend projection dimensions; a state machine
// adds indirection with no clarity gain here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrontendAdapterCapabilities {
    /// Whether the adapter can project memory records as rendered markdown.
    pub markdown_projection: bool,
    /// Whether the adapter supports live event push to the frontend.
    pub live_events: bool,
    /// Whether the adapter can apply reverse edits back into the vault.
    pub reverse_edits: bool,
}

/// Frontend adapter contract — projects vault state into editor / desktop UIs.
///
/// Brief §4 row 7: P1 forward stub. Method surface and conformance suite
/// ship in #113.
#[async_trait::async_trait]
pub trait FrontendAdapter: Send + Sync {
    /// Stable identifier of the registered plugin instance.
    fn name(&self) -> &str;

    /// Static capability advertisement (brief §4.1).
    fn capabilities(&self) -> &FrontendAdapterCapabilities;

    /// Range of `FrontendAdapter::CONTRACT_VERSION` values this impl accepts.
    fn supported_contract_versions(&self) -> VersionRange;
}
