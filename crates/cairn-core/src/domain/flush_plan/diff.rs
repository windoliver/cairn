//! Renders a [`super::FlushPlan`] to a human-readable markdown document
//! for `--human-review` mode. Stable output for `insta` snapshots.

use std::fmt::Write as _;

use super::{FlushPlan, PlannedMutation};

/// Maximum body excerpt length per mutation, characters.
pub const MAX_BODY_EXCERPT: usize = 4096;

/// Render the plan to markdown. Deterministic — same plan in, same bytes
/// out (byte-stable for snapshot tests).
#[must_use]
pub fn render(plan: &FlushPlan) -> String {
    let mut out = String::with_capacity(1024);
    writeln!(&mut out, "# FlushPlan {}", plan.operation_id.0).ok();
    writeln!(&mut out).ok();
    writeln!(&mut out, "- **Mode:** `{:?}`", plan.mode).ok();
    writeln!(&mut out, "- **Issuer:** `{}`", plan.issuer.as_str()).ok();
    if let Some(p) = &plan.principal {
        writeln!(&mut out, "- **Principal:** `{}`", p.as_str()).ok();
    }
    writeln!(&mut out, "- **Issued:** {}", plan.issued_at).ok();
    writeln!(&mut out, "- **Expires:** {}", plan.expires_at).ok();
    writeln!(&mut out, "- **Reason:** `{:?}`", plan.reason).ok();
    writeln!(&mut out, "- **Mutations:** {}", plan.mutations.len()).ok();
    writeln!(&mut out).ok();
    for (i, m) in plan.mutations.iter().enumerate() {
        writeln!(&mut out, "## Mutation {i}").ok();
        writeln!(&mut out).ok();
        match m {
            PlannedMutation::Upsert {
                record,
                prior_version,
            } => {
                writeln!(&mut out, "- **Kind:** upsert").ok();
                // record is Box<MemoryRecord> — auto-deref for field access.
                writeln!(&mut out, "- **Target:** `{}`", record.target_id.as_str()).ok();
                if let Some(v) = prior_version {
                    writeln!(&mut out, "- **Prior version:** {v}").ok();
                } else {
                    writeln!(&mut out, "- **Prior version:** _new record_").ok();
                }
                writeln!(&mut out).ok();
                writeln!(&mut out, "```").ok();
                let body = &record.body;
                if body.len() > MAX_BODY_EXCERPT {
                    // Floor to nearest UTF-8 char boundary at or below
                    // MAX_BODY_EXCERPT — a multibyte codepoint that
                    // straddles the byte threshold would panic on a raw
                    // slice. Walk down at most 3 bytes (UTF-8 codepoint
                    // length cap) to land on a boundary.
                    let mut cut = MAX_BODY_EXCERPT;
                    while cut > 0 && !body.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    out.push_str(&body[..cut]);
                    writeln!(&mut out, "\n…[truncated; {} more bytes]", body.len() - cut).ok();
                } else {
                    out.push_str(body);
                    writeln!(&mut out).ok();
                }
                writeln!(&mut out, "```").ok();
            }
            PlannedMutation::Delete {
                target,
                prior_version,
            } => {
                writeln!(&mut out, "- **Kind:** delete").ok();
                writeln!(&mut out, "- **Target:** `{}`", target.as_str()).ok();
                writeln!(&mut out, "- **Prior version:** {prior_version}").ok();
            }
            PlannedMutation::Promote {
                from,
                to_kind,
                evidence,
            } => {
                writeln!(&mut out, "- **Kind:** promote").ok();
                writeln!(&mut out, "- **From:** `{}`", from.as_str()).ok();
                writeln!(&mut out, "- **To kind:** `{to_kind:?}`").ok();
                writeln!(&mut out, "- **Evidence count:** {}", evidence.len()).ok();
            }
            PlannedMutation::Expire { target, reason } => {
                writeln!(&mut out, "- **Kind:** expire").ok();
                writeln!(&mut out, "- **Target:** `{}`", target.as_str()).ok();
                writeln!(&mut out, "- **Reason:** `{reason:?}`").ok();
            }
            PlannedMutation::ForgetSession { session } => {
                writeln!(&mut out, "- **Kind:** forget_session").ok();
                writeln!(&mut out, "- **Session:** `{}`", session.as_str()).ok();
            }
            PlannedMutation::ForgetRecord { target } => {
                writeln!(&mut out, "- **Kind:** forget_record").ok();
                writeln!(&mut out, "- **Target:** `{}`", target.as_str()).ok();
            }
            PlannedMutation::Evolve { skill, diff_ref } => {
                writeln!(&mut out, "- **Kind:** evolve").ok();
                writeln!(&mut out, "- **Skill:** `{}`", skill.as_str()).ok();
                writeln!(&mut out, "- **Diff ref:** `{}`", diff_ref.display()).ok();
            }
        }
        writeln!(&mut out).ok();
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::domain::{
        FlushMode, FlushPlan, Identity, PlanReason, PlannedMutation, ScopeTuple, TargetId,
    };
    use crate::generated::common::Ulid;

    /// Round 2 (#54): `MAX_BODY_EXCERPT` truncation must walk back to a
    /// UTF-8 char boundary rather than panic when a multibyte codepoint
    /// straddles the byte threshold.
    #[test]
    fn upsert_truncation_handles_multibyte_at_boundary() {
        // 4-byte UTF-8 (😀, U+1F600). Build a body so that the codepoint
        // straddles MAX_BODY_EXCERPT (4096): pad to len 4094 with ASCII,
        // then append the emoji whose 4 bytes occupy positions 4094..4098
        // — byte 4096 lands in the middle of it.
        let mut body = "x".repeat(MAX_BODY_EXCERPT - 2);
        body.push('😀');
        // Pad with more ASCII so we exceed the threshold and trigger
        // the truncation branch.
        body.push_str(&"y".repeat(64));

        let mut record = crate::domain::record::tests_export::sample_record();
        record.body = body;
        let plan = FlushPlan {
            operation_id: Ulid("01HQZK000000000000000000VP".into()),
            issued_at: "2026-05-04T12:00:00Z".into(),
            issuer: Identity::parse("agt:claude-code:opus-4-7:reviewer:v1").unwrap(),
            principal: None,
            scope: ScopeTuple::default(),
            mode: FlushMode::HumanReview,
            mutations: vec![PlannedMutation::Upsert {
                record: Box::new(record),
                prior_version: None,
            }],
            reason: PlanReason::UserIngest,
            source_events: vec![],
            target_hashes: BTreeMap::default(),
            dependencies: vec![],
            expires_at: "2026-05-04T12:05:00Z".into(),
            placeholder: false,
        };
        // Must not panic when slicing the body at the byte boundary.
        let md = render(&plan);
        assert!(md.contains("[truncated;"), "expected truncation marker");
    }

    #[test]
    fn renders_delete_mutation() {
        let plan = FlushPlan {
            operation_id: Ulid("01HQZK000000000000000000VP".into()),
            issued_at: "2026-05-04T12:00:00Z".into(),
            issuer: Identity::parse("agt:claude-code:opus-4-7:reviewer:v1").unwrap(),
            principal: None,
            scope: ScopeTuple::default(),
            mode: FlushMode::HumanReview,
            mutations: vec![PlannedMutation::Delete {
                target: TargetId::parse("01HQZX9F5N0000000000000000").unwrap(),
                prior_version: 3,
            }],
            reason: PlanReason::UserIngest,
            source_events: vec![],
            target_hashes: BTreeMap::default(),
            dependencies: vec![],
            expires_at: "2026-05-04T12:05:00Z".into(),
            placeholder: false,
        };
        let md = render(&plan);
        assert!(md.contains("# FlushPlan 01HQZK"));
        assert!(md.contains("- **Kind:** delete"));
        assert!(md.contains("- **Target:** `01HQZX9F5N0000000000000000`"));
        assert!(md.contains("- **Prior version:** 3"));
    }
}
