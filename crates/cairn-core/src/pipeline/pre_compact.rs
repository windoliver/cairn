//! Typed core models and pure budget math for the future pre-compaction hook.

use crate::domain::SessionId;

/// Input snapshot for a pre-compaction render attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreCompactEvent {
    /// Session being compacted.
    pub session_id: SessionId,
    /// Token count before compaction starts.
    pub token_count_before: u32,
    /// Token target the runtime intends to compact away.
    pub compaction_target: u32,
    /// Last user-visible turn index at the time the hook fires.
    pub last_user_turn_index: u64,
}

/// Output of a pre-compaction render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreCompactOutput {
    /// Rendered reinjection payload text.
    pub reinjection_text: String,
    /// Byte length of `reinjection_text`.
    pub output_bytes: u64,
    /// Maximum bytes budgeted for reinjection.
    pub budget_bytes: u64,
    /// Recipe identifier used to render the output.
    pub recipe: String,
}

/// Compute the reinjection budget from the compaction target and safety ratio.
#[must_use]
pub fn compute_budget(compaction_target: u32, max_bytes: u32, ratio: f64) -> u64 {
    let hinted = (f64::from(compaction_target) * ratio).floor() as u64;
    hinted.min(u64::from(max_bytes))
}

#[cfg(test)]
mod tests {
    use super::compute_budget;

    #[test]
    fn computes_budget_from_target_and_ratio() {
        let budget = compute_budget(8_000, 25_600, 0.30);
        assert_eq!(budget, 2_400);
    }

    #[test]
    fn caps_budget_at_hot_memory_max_bytes() {
        let budget = compute_budget(8_000, 1_000, 0.30);
        assert_eq!(budget, 1_000);
    }

    #[test]
    fn zero_target_yields_zero_budget() {
        let budget = compute_budget(0, 25_600, 0.30);
        assert_eq!(budget, 0);
    }
}
