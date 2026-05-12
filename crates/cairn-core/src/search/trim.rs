//! Char-count proxy for token-budget trimming of search pages.
//!
//! Sums `snippet.len()` across candidates in order; stops appending once
//! the running total would exceed `max_chars`. Trims the parallel `explain`
//! block in lockstep so record-id alignment is preserved.
//!
//! This is a deliberately deterministic char-count approximation, not a
//! tokenizer-accurate count. The token-accurate variant is P1, gated on
//! hot-memory assembly (brief §11) actually consuming search output.

use crate::contract::memory_store::SearchCandidate;
use crate::search::explain::ScoreExplain;

/// Trim `candidates` so the total `snippet.len()` does not exceed
/// `max_chars`. The first candidate is always kept even if it exceeds the
/// budget alone — a search must return at least one hit when the leg
/// produced one. `explain`, when supplied, is truncated to the same length
/// as the trimmed candidate vector — the upstream contract guarantees that
/// `explain[i].record_id == candidates[i].record_id`, so positional
/// truncation preserves alignment.
///
/// `max_chars == 0` is treated as "no trim".
#[must_use]
pub fn token_budget_trim(
    candidates: Vec<SearchCandidate>,
    explain: Option<Vec<ScoreExplain>>,
    max_chars: usize,
) -> (Vec<SearchCandidate>, Option<Vec<ScoreExplain>>) {
    if max_chars == 0 || candidates.is_empty() {
        return (candidates, explain);
    }

    let mut running: usize = 0;
    let mut cut_at: usize = candidates.len();
    for (i, c) in candidates.iter().enumerate() {
        let next = running.saturating_add(c.snippet.len());
        if i > 0 && next > max_chars {
            cut_at = i;
            break;
        }
        running = next;
    }

    let trimmed_candidates: Vec<SearchCandidate> = candidates.into_iter().take(cut_at).collect();
    let trimmed_explain = explain.map(|exps| exps.into_iter().take(cut_at).collect());
    (trimmed_candidates, trimmed_explain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
    use crate::domain::{RecordId, ScopeTuple, TargetId};

    fn rid(s: &str) -> RecordId {
        RecordId::parse(format!("01HQZX9F5N00000000000000{s}")).expect("valid record id")
    }

    fn cand(id: &str, snippet: &str) -> SearchCandidate {
        SearchCandidate {
            record_id: rid(id),
            target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("target"),
            scope: ScopeTuple::default(),
            kind: MemoryKind::Fact,
            class: MemoryClass::Episodic,
            visibility: MemoryVisibility::Private,
            bm25: 0.0,
            recency_seconds: 0,
            confidence: 1.0,
            salience: 1.0,
            staleness_seconds: 0,
            snippet: snippet.to_owned(),
            record_json: "{}".to_owned(),
            semantic_distance: None,
        }
    }

    fn explain(id: &str) -> ScoreExplain {
        ScoreExplain {
            record_id: rid(id),
            bm25_rank: Some(1),
            semantic_rank: Some(1),
            rrf_score: 0.0,
            cosine: None,
            final_score: 0.0,
        }
    }

    #[test]
    fn empty_candidates_returns_empty() {
        let (c, e) = token_budget_trim(vec![], None, 100);
        assert!(c.is_empty());
        assert!(e.is_none());
    }

    #[test]
    fn zero_budget_skips_trim() {
        let cands = vec![cand("0A", "hello"), cand("0B", "world")];
        let (c, _) = token_budget_trim(cands, None, 0);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn all_fits_returns_all() {
        let cands = vec![cand("0A", "abc"), cand("0B", "de")];
        let (c, _) = token_budget_trim(cands, None, 100);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn overflow_midway_truncates() {
        let cands = vec![
            cand("0A", "12345"),
            cand("0B", "67890"),
            cand("0C", "abcde"),
        ];
        let (c, _) = token_budget_trim(cands, None, 8);
        // 5 + 5 = 10 > 8, so keep only "12345" (idx 0)
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].record_id, rid("0A"));
    }

    #[test]
    fn first_oversized_candidate_kept() {
        // Single oversized candidate: must be kept (return at least one hit).
        let cands = vec![cand("0A", &"x".repeat(1000))];
        let (c, _) = token_budget_trim(cands, None, 10);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn explain_positional_alignment_preserved() {
        // The contract is positional: explain[i] aligns with candidates[i].
        // Even if explain entries' record_ids don't match the candidates'
        // record_ids (a contract violation upstream), this function trims
        // by position — it does NOT reorder or filter by record_id.
        let cands = vec![cand("0A", "12345"), cand("0B", "67890")];
        let exps = Some(vec![explain("0B"), explain("0A")]); // reversed order
        let (c, e) = token_budget_trim(cands, exps, 6);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].record_id, rid("0A"));
        let e = e.expect("explain present");
        assert_eq!(e.len(), 1);
        // Positional: explain[0] survives regardless of its record_id
        assert_eq!(e[0].record_id, rid("0B"));
    }

    #[test]
    fn explain_trimmed_in_lockstep() {
        let cands = vec![cand("0A", "12345"), cand("0B", "67890")];
        let exps = Some(vec![explain("0A"), explain("0B")]);
        let (c, e) = token_budget_trim(cands, exps, 6);
        assert_eq!(c.len(), 1);
        let e = e.expect("explain present");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].record_id, rid("0A"));
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use crate::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
    use crate::domain::{RecordId, ScopeTuple, TargetId};
    use proptest::prelude::*;

    fn cand_proptest(id_suffix: &str, snippet_len: usize) -> SearchCandidate {
        SearchCandidate {
            record_id: RecordId::parse(format!("01HQZX9F5N00000000000000{id_suffix}"))
                .expect("valid"),
            target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("target"),
            scope: ScopeTuple::default(),
            kind: MemoryKind::Fact,
            class: MemoryClass::Episodic,
            visibility: MemoryVisibility::Private,
            bm25: 0.0,
            recency_seconds: 0,
            confidence: 1.0,
            salience: 1.0,
            staleness_seconds: 0,
            snippet: "x".repeat(snippet_len),
            record_json: "{}".to_owned(),
            semantic_distance: None,
        }
    }

    proptest! {
        #[test]
        fn trim_is_monotone_in_size(
            sizes in prop::collection::vec(1usize..50, 1..16),
            budget in 0usize..400,
        ) {
            let cands: Vec<_> = sizes.iter().enumerate().map(|(i, n)| {
                let suffix = format!("{i:02X}");
                cand_proptest(&suffix, *n)
            }).collect();
            let n_in = cands.len();
            let (out, _) = token_budget_trim(cands, None, budget);
            prop_assert!(out.len() <= n_in);
            prop_assert!(!out.is_empty() || n_in == 0);
        }
    }
}
