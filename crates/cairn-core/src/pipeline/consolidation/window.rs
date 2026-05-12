//! Pure windowing: pick the next N turns to summarize from a session's
//! trace stream. No I/O — input is a sorted slice of trace headers.

use serde::{Deserialize, Serialize};

/// Lightweight header for one trace turn record. The handler builds these
/// from `MemoryRecord` headers; tests construct them directly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnHeader {
    /// `record_id` of the `turn_summary` record.
    pub record_id: String,
    /// Session identifier.
    pub session_id: String,
    /// Stable turn id (`trace.turn_id`).
    pub turn_id: String,
    /// Monotonic ordering within the session.
    pub sequence: u32,
    /// Estimated token count of the turn body. The handler approximates
    /// with `body.chars().len() / 4`.
    pub approx_tokens: u32,
    /// Computed salience for ranking; `1.0` for explicit user "remember"
    /// triggers, `0.5` baseline, lower for noise.
    pub salience: f32,
}

/// Result of [`pick_window`].
#[derive(Debug, Clone, PartialEq)]
pub struct WindowSelection {
    /// Selected turns, in ascending sequence order.
    pub turns: Vec<TurnHeader>,
    /// Sequence number of the last turn covered, for next-watermark math.
    pub last_sequence: u32,
}

/// Choose the next window to summarize.
///
/// `since_sequence` is the highest `sequence` already covered by a prior
/// summary (0 means "no prior summary"). The function returns at most
/// `window_size` turns whose salience clears `salience_floor`. Returns
/// `None` when fewer than `min_for_trigger` eligible turns are
/// available.
#[must_use]
pub fn pick_window(
    candidates: &[TurnHeader],
    since_sequence: u32,
    window_size: u32,
    min_for_trigger: u32,
    salience_floor: f32,
) -> Option<WindowSelection> {
    let mut filtered: Vec<TurnHeader> = candidates
        .iter()
        .filter(|t| t.sequence > since_sequence && t.salience >= salience_floor)
        .cloned()
        .collect();
    filtered.sort_by_key(|t| t.sequence);
    // len fits u32; cap saturates
    if u32::try_from(filtered.len()).unwrap_or(u32::MAX) < min_for_trigger {
        return None;
    }
    let take = (window_size as usize).min(filtered.len());
    let turns = filtered.into_iter().take(take).collect::<Vec<_>>();
    let last_sequence = turns.last().map(|t| t.sequence)?;
    Some(WindowSelection { turns, last_sequence })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(seq: u32, sal: f32) -> TurnHeader {
        TurnHeader {
            record_id: format!("rec-{seq}"),
            session_id: "s1".into(),
            turn_id: format!("t-{seq}"),
            sequence: seq,
            approx_tokens: 40,
            salience: sal,
        }
    }

    #[test]
    fn picks_ascending_window() {
        let pool: Vec<_> = (1..=10).map(|s| turn(s, 0.7)).collect();
        let sel = pick_window(&pool, 0, 4, 2, 0.4).expect("eligible");
        assert_eq!(sel.turns.iter().map(|t| t.sequence).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
        assert_eq!(sel.last_sequence, 4);
    }

    #[test]
    fn skips_below_floor() {
        let pool = vec![turn(1, 0.2), turn(2, 0.6), turn(3, 0.7), turn(4, 0.1), turn(5, 0.8)];
        let sel = pick_window(&pool, 0, 8, 2, 0.4).expect("eligible");
        assert_eq!(sel.turns.iter().map(|t| t.sequence).collect::<Vec<_>>(), vec![2, 3, 5]);
    }

    #[test]
    fn returns_none_below_min() {
        let pool = vec![turn(1, 0.9)];
        assert!(pick_window(&pool, 0, 8, 2, 0.4).is_none());
    }

    #[test]
    fn skips_already_covered() {
        let pool: Vec<_> = (1..=6).map(|s| turn(s, 0.7)).collect();
        let sel = pick_window(&pool, 3, 4, 2, 0.4).expect("eligible");
        assert_eq!(sel.turns.iter().map(|t| t.sequence).collect::<Vec<_>>(), vec![4, 5, 6]);
    }
}
