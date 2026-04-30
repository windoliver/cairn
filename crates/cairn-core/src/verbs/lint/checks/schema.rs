//! Stub — populated by Task 10.

use crate::generated::verbs::lint::Finding;
use crate::verbs::lint::LintInputs;

/// Run the stale-schema check. Stub — always returns no findings.
#[must_use]
pub fn run(_inputs: &LintInputs<'_>) -> Vec<Finding> {
    Vec::new()
}
