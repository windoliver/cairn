//! Conformance cases for `MCPServer` plugins (filled in Task 4).

use crate::contract::conformance::CaseOutcome;
use crate::contract::registry::{PluginName, PluginRegistry};

/// Run the conformance suite for this contract. Filled in Task 4.
#[must_use]
pub fn run(_registry: &PluginRegistry, _name: &PluginName) -> Vec<CaseOutcome> {
    Vec::new()
}
