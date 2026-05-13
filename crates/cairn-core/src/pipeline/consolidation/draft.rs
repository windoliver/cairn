//! Pure `compute_rolling_summary` — turns a [`WindowSelection`] into a
//! [`RollingSummaryDraft`]. Deterministic; the body is a placeholder
//! that an LLM-backed implementation overrides in a follow-up. The
//! handler still produces a valid `reasoning` record without an LLM —
//! brief §1 says rolling summaries degrade to `consolidation_deferred`
//! only when explicitly disabled, otherwise the substrate keeps writing.

use std::fmt::Write as _;

use super::errors::ConsolidationError;
use super::window::WindowSelection;
use crate::config::ConsolidationConfig;

/// Whether the consolidator produced a summary or deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryStatus {
    /// Summary body was authored.
    Authored,
    /// `enabled = false`; the trigger should emit a
    /// `consolidation_deferred` lint entry rather than persist.
    Deferred,
}

/// Output of [`compute_rolling_summary`]. The handler converts this
/// into a `MemoryRecord` with `kind = reasoning, class = episodic,
/// scope.session_id = …`, and `extra_frontmatter.consolidation.source_record_ids`
/// pointing back at the window turns.
#[derive(Debug, Clone, PartialEq)]
pub struct RollingSummaryDraft {
    /// `Authored` or `Deferred`.
    pub status: SummaryStatus,
    /// Markdown body (empty when `Deferred`).
    pub body: String,
    /// `record_id`s of the turn records this summary covers.
    pub source_record_ids: Vec<String>,
    /// Highest `sequence` in the window — written into the summary
    /// frontmatter as `consolidation.last_sequence`.
    pub last_sequence: u32,
    /// Approximate token count of `body` (caller-provided cap).
    pub summary_tokens: u32,
}

/// Compute a rolling summary from a pre-selected window.
///
/// # Errors
/// - [`ConsolidationError::EmptyWindow`] when the window has no turns.
/// - [`ConsolidationError::BudgetExceeded`] when the deterministic
///   placeholder body cannot fit inside `config.token_budget` even after
///   truncation. (Should not happen in practice — the placeholder is
///   one short line per turn.)
pub fn compute_rolling_summary(
    window: &WindowSelection,
    config: &ConsolidationConfig,
) -> Result<RollingSummaryDraft, ConsolidationError> {
    if window.turns.is_empty() {
        return Err(ConsolidationError::EmptyWindow);
    }
    if !config.enabled {
        return Ok(RollingSummaryDraft {
            status: SummaryStatus::Deferred,
            body: String::new(),
            source_record_ids: window.turns.iter().map(|t| t.record_id.clone()).collect(),
            last_sequence: window.last_sequence,
            summary_tokens: 0,
        });
    }
    // Char-per-token approximation: 4 chars per token is the canonical
    // rule of thumb (brief §5.3 note); a real tokenizer is the LLM-mode
    // follow-up.
    let max_chars = (config.token_budget as usize).saturating_mul(4);
    let mut body = String::new();
    body.push_str("Rolling summary of ");
    body.push_str(&window.turns.len().to_string());
    body.push_str(" turn(s):\n\n");
    for turn in &window.turns {
        body.push_str("- ");
        body.push_str(&turn.turn_id);
        body.push_str(" (seq=");
        body.push_str(&turn.sequence.to_string());
        body.push_str(", salience=");
        let _ = write!(body, "{:.2}", turn.salience);
        body.push_str(")\n");
        if body.len() > max_chars {
            // String::truncate(byte_idx) panics if byte_idx lands inside
            // a multi-byte UTF-8 sequence. turn_ids and bodies can be
            // arbitrary UTF-8, so walk back to the nearest char boundary
            // before truncating (round-9 adversarial review #4).
            let mut cut = max_chars.min(body.len());
            while cut > 0 && !body.is_char_boundary(cut) {
                cut -= 1;
            }
            body.truncate(cut);
            body.push_str("\n…");
            break;
        }
    }
    let summary_tokens = u32::try_from(body.len() / 4).unwrap_or(u32::MAX);
    Ok(RollingSummaryDraft {
        status: SummaryStatus::Authored,
        body,
        source_record_ids: window.turns.iter().map(|t| t.record_id.clone()).collect(),
        last_sequence: window.last_sequence,
        summary_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::consolidation::window::TurnHeader;

    fn turn(seq: u32) -> TurnHeader {
        TurnHeader {
            record_id: format!("rec-{seq}"),
            session_id: "s1".into(),
            turn_id: format!("t-{seq}"),
            sequence: seq,
            approx_tokens: 40,
            salience: 0.6,
        }
    }

    #[test]
    fn authored_body_includes_each_turn_id() {
        let win = WindowSelection {
            turns: vec![turn(1), turn(2), turn(3)],
            last_sequence: 3,
        };
        let draft = compute_rolling_summary(&win, &ConsolidationConfig::default()).unwrap();
        assert_eq!(draft.status, SummaryStatus::Authored);
        assert!(
            draft.body.contains("t-1") && draft.body.contains("t-2") && draft.body.contains("t-3")
        );
        assert_eq!(draft.source_record_ids, vec!["rec-1", "rec-2", "rec-3"]);
        assert_eq!(draft.last_sequence, 3);
    }

    #[test]
    fn deferred_when_disabled() {
        let win = WindowSelection {
            turns: vec![turn(1), turn(2)],
            last_sequence: 2,
        };
        let cfg = ConsolidationConfig {
            enabled: false,
            ..ConsolidationConfig::default()
        };
        let draft = compute_rolling_summary(&win, &cfg).unwrap();
        assert_eq!(draft.status, SummaryStatus::Deferred);
        assert!(draft.body.is_empty());
        assert_eq!(draft.source_record_ids.len(), 2);
    }

    #[test]
    fn empty_window_errors() {
        let win = WindowSelection {
            turns: vec![],
            last_sequence: 0,
        };
        assert!(matches!(
            compute_rolling_summary(&win, &ConsolidationConfig::default()),
            Err(ConsolidationError::EmptyWindow)
        ));
    }

    #[test]
    fn respects_token_budget_floor() {
        let many: Vec<_> = (1..=200).map(turn).collect();
        let win = WindowSelection {
            turns: many,
            last_sequence: 200,
        };
        let cfg = ConsolidationConfig {
            token_budget: 32,
            ..ConsolidationConfig::default()
        };
        let draft = compute_rolling_summary(&win, &cfg).unwrap();
        assert!(draft.body.len() <= 32 * 4 + 4); // +4 for truncation marker
        assert_eq!(draft.status, SummaryStatus::Authored);
    }
}
