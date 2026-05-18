//! Second-pass cosine re-rank over the top-K RRF survivors.

use std::collections::HashMap;
use std::hash::BuildHasher;

use crate::domain::RecordId;

use super::rrf::RrfCandidate;

/// Output of [`cosine_rerank`].
#[derive(Debug, Clone, PartialEq)]
pub struct RerankedCandidate {
    /// Record id.
    pub record_id: RecordId,
    /// Original RRF score (preserved for diagnostics).
    pub rrf_score: f64,
    /// Cosine similarity between query and doc vectors. `None` if the
    /// vector for this record was not supplied to the rerank.
    pub cosine: Option<f64>,
    /// `blend * normalize(rrf) + (1 - blend) * cosine`. When `cosine`
    /// is `None`, equals `rrf_score / max_rrf`.
    pub final_score: f64,
}

/// Cosine similarity of two same-length f32 vectors. Returns 0.0 when
/// either vector has zero norm (degenerate but not panic-worthy).
#[must_use]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    debug_assert_eq!(a.len(), b.len(), "cosine: vectors must be same length");
    let mut dot = 0.0_f64;
    let mut na = 0.0_f64;
    let mut nb = 0.0_f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let xf = f64::from(*x);
        let yf = f64::from(*y);
        dot += xf * yf;
        na += xf * xf;
        nb += yf * yf;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Re-rank `rrf` using cosine similarity to a query vector.
///
/// `doc_vectors` may be a subset of `rrf` — if a record's vector is
/// missing, its cosine contribution is treated as `0.0` and the final
/// score equals `blend * normalize(rrf)`.
///
/// `blend` is the weight on the normalized RRF term; cosine gets `1 - blend`.
/// Both should be in `[0.0, 1.0]`. Out-of-range values are clamped.
///
/// Returns descending by `final_score`, ties broken by record id.
#[must_use]
pub fn cosine_rerank<S: BuildHasher>(
    rrf: &[RrfCandidate],
    doc_vectors: &HashMap<RecordId, Vec<f32>, S>,
    query_vector: &[f32],
    blend: f32,
) -> Vec<RerankedCandidate> {
    // `blend` is a weight in [0, 1]; widening f32→f64 is lossless, but the
    // workspace pedantic lints flag the cast generically.
    #[allow(clippy::cast_precision_loss)] // f32→f64 widening of a bounded weight is lossless
    let blend = f64::from(blend.clamp(0.0, 1.0));
    let max_rrf = rrf.iter().map(|c| c.rrf_score).fold(0.0_f64, f64::max);

    let mut out: Vec<RerankedCandidate> = rrf
        .iter()
        .map(|c| {
            let normalized = if max_rrf == 0.0 {
                0.0
            } else {
                c.rrf_score / max_rrf
            };
            let cosine = doc_vectors
                .get(&c.record_id)
                .map(|v| cosine_similarity(query_vector, v));
            let cos_term = cosine.unwrap_or(0.0);
            let final_score = blend * normalized + (1.0 - blend) * cos_term;
            RerankedCandidate {
                record_id: c.record_id.clone(),
                rrf_score: c.rrf_score,
                cosine,
                final_score,
            }
        })
        .collect();

    out.sort_by(|a, b| {
        b.final_score
            .partial_cmp(&a.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.record_id.as_str().cmp(b.record_id.as_str()))
    });
    out
}

/// Where a candidate originated, for [`cosine_rerank_tagged`].
///
/// `Lexical` candidates have a doc vector and the cosine term blends into
/// the final score. `GraphOnly` candidates surfaced via 1-hop entity-graph
/// expansion only — there is no lexical match and the cosine term is
/// dropped (rather than substituted by a neutral 0.5, which would bias
/// graph-only candidates upward in the cosine-only blend).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateOrigin {
    /// Surfaced by the keyword and/or semantic leg; has a doc vector.
    Lexical,
    /// Surfaced only by the graph leg; no lexical match.
    GraphOnly,
}

/// One [`RrfCandidate`] tagged with its origin for [`cosine_rerank_tagged`].
#[derive(Debug, Clone, PartialEq)]
pub struct OriginTaggedCandidate {
    /// The fused candidate.
    pub inner: RrfCandidate,
    /// Whether this candidate has a doc vector eligible for cosine.
    pub origin: CandidateOrigin,
}

/// Origin-aware variant of [`cosine_rerank`].
///
/// For [`CandidateOrigin::Lexical`] candidates the formula is
/// `blend * rrf_norm + (1 - blend) * cos_norm` where
/// `cos_norm = (cosine_raw + 1) / 2` puts cosine in `[0, 1]` so it is
/// commensurable with `rrf_norm`. For [`CandidateOrigin::GraphOnly`] the
/// cosine term is dropped: `final = blend * rrf_norm`. This is symmetric
/// — both origins use the same `blend` factor — and prevents graph-only
/// candidates from inheriting an undue cosine boost that they did not
/// earn lexically.
///
/// Returns descending by `final_score`, ties broken by record id.
#[must_use]
pub fn cosine_rerank_tagged<S: BuildHasher>(
    candidates: &[OriginTaggedCandidate],
    doc_vectors: &HashMap<RecordId, Vec<f32>, S>,
    query_vector: &[f32],
    blend: f32,
) -> Vec<RerankedCandidate> {
    let blend = f64::from(blend.clamp(0.0, 1.0));
    let max_rrf = candidates
        .iter()
        .map(|c| c.inner.rrf_score)
        .fold(0.0_f64, f64::max);

    let mut out: Vec<RerankedCandidate> = candidates
        .iter()
        .map(|tc| {
            let rrf_norm = if max_rrf < f64::EPSILON {
                0.0
            } else {
                tc.inner.rrf_score / max_rrf
            };
            let raw_cos = doc_vectors
                .get(&tc.inner.record_id)
                .map(|v| cosine_similarity(query_vector, v));
            let (final_score, cosine) = match tc.origin {
                CandidateOrigin::Lexical => {
                    let cos_norm = raw_cos.map_or(0.5, |c| f64::midpoint(c, 1.0));
                    (blend * rrf_norm + (1.0 - blend) * cos_norm, raw_cos)
                }
                CandidateOrigin::GraphOnly => (blend * rrf_norm, None),
            };
            RerankedCandidate {
                record_id: tc.inner.record_id.clone(),
                rrf_score: tc.inner.rrf_score,
                cosine,
                final_score,
            }
        })
        .collect();

    out.sort_by(|a, b| {
        b.final_score
            .partial_cmp(&a.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.record_id.as_str().cmp(b.record_id.as_str()))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rid(s: &str) -> RecordId {
        // Prefix is 24 chars; `s` is a 2-char suffix → 26-char ULID.
        RecordId::parse(format!("01HQZX9F5N00000000000000{s}")).expect("valid record id")
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = [1.0_f32, 0.0];
        let b = [0.0_f32, 1.0];
        let c = cosine_similarity(&a, &b);
        assert!(c.abs() < 1e-12);
    }

    #[test]
    fn cosine_identical_is_one() {
        let a = [1.0_f32, 2.0, 3.0];
        let c = cosine_similarity(&a, &a);
        assert!((c - 1.0).abs() < 1e-12);
    }

    #[test]
    fn cosine_zero_norm_returns_zero() {
        let a = [0.0_f32, 0.0];
        let b = [1.0_f32, 1.0];
        // Function returns the literal `0.0` for the zero-norm branch, so
        // comparing with a tiny epsilon is exact-equivalent and avoids the
        // pedantic float_cmp lint.
        assert!(cosine_similarity(&a, &b).abs() < f64::EPSILON);
    }

    #[test]
    fn rerank_blend_one_preserves_rrf_order() {
        let rrf = vec![
            RrfCandidate {
                record_id: rid("0A"),
                rrf_score: 0.9,
            },
            RrfCandidate {
                record_id: rid("0B"),
                rrf_score: 0.1,
            },
        ];
        let mut docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        // Cosine prefers 0B if used, but blend=1.0 ignores cosine.
        docs.insert(rid("0A"), vec![0.0, 1.0]);
        docs.insert(rid("0B"), vec![1.0, 0.0]);
        let q = vec![1.0_f32, 0.0];
        let out = cosine_rerank(&rrf, &docs, &q, 1.0);
        assert_eq!(out[0].record_id, rid("0A"));
        assert_eq!(out[1].record_id, rid("0B"));
    }

    #[test]
    fn rerank_blend_zero_uses_cosine_only() {
        let rrf = vec![
            RrfCandidate {
                record_id: rid("0A"),
                rrf_score: 0.9,
            },
            RrfCandidate {
                record_id: rid("0B"),
                rrf_score: 0.1,
            },
        ];
        let mut docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        docs.insert(rid("0A"), vec![0.0, 1.0]); // cosine 0.0 vs query [1,0]
        docs.insert(rid("0B"), vec![1.0, 0.0]); // cosine 1.0 vs query [1,0]
        let q = vec![1.0_f32, 0.0];
        let out = cosine_rerank(&rrf, &docs, &q, 0.0);
        assert_eq!(out[0].record_id, rid("0B"));
        assert_eq!(out[1].record_id, rid("0A"));
    }

    #[test]
    fn rerank_blend_default_balances_signals() {
        let rrf = vec![
            RrfCandidate {
                record_id: rid("0A"),
                rrf_score: 1.0,
            },
            RrfCandidate {
                record_id: rid("0B"),
                rrf_score: 0.5,
            },
        ];
        let mut docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        docs.insert(rid("0A"), vec![0.0, 1.0]); // cosine 0.0
        docs.insert(rid("0B"), vec![1.0, 0.0]); // cosine 1.0
        let q = vec![1.0_f32, 0.0];
        let out = cosine_rerank(&rrf, &docs, &q, 0.7);
        // 0A: 0.7*1.0 + 0.3*0.0 = 0.70
        // 0B: 0.7*0.5 + 0.3*1.0 = 0.65
        // Tolerance is f32-grade because `blend: f32` is widened to f64;
        // 0.7_f32 → 0.6999999881 in f64.
        assert_eq!(out[0].record_id, rid("0A"));
        assert!((out[0].final_score - 0.70).abs() < 1e-6);
        assert!((out[1].final_score - 0.65).abs() < 1e-6);
    }

    #[test]
    fn rerank_missing_vector_treated_as_zero_cosine() {
        let rrf = vec![RrfCandidate {
            record_id: rid("0A"),
            rrf_score: 1.0,
        }];
        let docs: HashMap<RecordId, Vec<f32>> = HashMap::new(); // empty
        let q = vec![1.0_f32, 0.0];
        let out = cosine_rerank(&rrf, &docs, &q, 0.7);
        assert_eq!(out.len(), 1);
        assert!(out[0].cosine.is_none());
        // 0.7 * (1.0 / 1.0) + 0.3 * 0.0 = 0.70
        // Tolerance is f32-grade (see `rerank_blend_default_balances_signals`).
        assert!((out[0].final_score - 0.70).abs() < 1e-6);
    }

    #[test]
    fn graph_only_ties_lexical_at_cosine_minus_one_equal_rrf() {
        // Lexical doc has cosine_raw = -1 → cos_norm = 0; graph-only drops
        // cosine. With equal RRF and same blend, both should produce the
        // same final_score = blend * rrf_norm.
        let mut docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        docs.insert(rid("0A"), vec![0.0_f32, 1.0]);
        let cands = vec![
            OriginTaggedCandidate {
                inner: RrfCandidate {
                    record_id: rid("0A"),
                    rrf_score: 0.5,
                },
                origin: CandidateOrigin::Lexical,
            },
            OriginTaggedCandidate {
                inner: RrfCandidate {
                    record_id: rid("0B"),
                    rrf_score: 0.5,
                },
                origin: CandidateOrigin::GraphOnly,
            },
        ];
        let q = vec![0.0_f32, -1.0];
        let out = cosine_rerank_tagged(&cands, &docs, &q, 0.7);
        assert!(
            (out[0].final_score - out[1].final_score).abs() < 1e-9,
            "lexical-cos-(-1) must tie graph-only"
        );
    }

    #[test]
    fn graph_only_loses_to_strong_lexical_at_equal_rrf() {
        let mut docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        docs.insert(rid("0A"), vec![1.0_f32, 0.0]);
        let cands = vec![
            OriginTaggedCandidate {
                inner: RrfCandidate {
                    record_id: rid("0A"),
                    rrf_score: 0.5,
                },
                origin: CandidateOrigin::Lexical,
            },
            OriginTaggedCandidate {
                inner: RrfCandidate {
                    record_id: rid("0B"),
                    rrf_score: 0.5,
                },
                origin: CandidateOrigin::GraphOnly,
            },
        ];
        let q = vec![1.0_f32, 0.0];
        let out = cosine_rerank_tagged(&cands, &docs, &q, 0.7);
        assert_eq!(out[0].record_id, rid("0A"));
    }

    #[test]
    fn graph_only_cosine_field_is_none() {
        let docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        let cands = vec![OriginTaggedCandidate {
            inner: RrfCandidate {
                record_id: rid("0A"),
                rrf_score: 1.0,
            },
            origin: CandidateOrigin::GraphOnly,
        }];
        let q = vec![1.0_f32, 0.0];
        let out = cosine_rerank_tagged(&cands, &docs, &q, 0.7);
        assert!(out[0].cosine.is_none());
    }

    #[test]
    fn rerank_zero_max_rrf_does_not_panic() {
        let rrf = vec![
            RrfCandidate {
                record_id: rid("0A"),
                rrf_score: 0.0,
            },
            RrfCandidate {
                record_id: rid("0B"),
                rrf_score: 0.0,
            },
        ];
        let mut docs: HashMap<RecordId, Vec<f32>> = HashMap::new();
        docs.insert(rid("0A"), vec![1.0, 0.0]);
        docs.insert(rid("0B"), vec![0.0, 1.0]);
        let q = vec![1.0_f32, 0.0];
        let out = cosine_rerank(&rrf, &docs, &q, 0.7);
        assert_eq!(out[0].record_id, rid("0A"));
    }
}
