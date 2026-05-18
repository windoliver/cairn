//! Uniform admissibility predicate (issue #82, brief §6, §7).
//!
//! Every source delegates to [`admit`] before its own ranking. This
//! re-checks visibility / scope / confidence / body invariants the
//! adapter is supposed to have applied — defense-in-depth at the
//! trust boundary so a buggy or malicious adapter cannot smuggle a
//! record into the hot prefix.

use crate::domain::record::MemoryRecord;
use crate::domain::scope::ScopeTuple;
use crate::domain::taxonomy::MemoryVisibility;

use super::inclusion::ExclusionReason;

/// Confidence floor — matches `pipeline::profile::synthesize::CONFIDENCE_FLOOR`.
pub const CONFIDENCE_FLOOR: f32 = 0.3;

/// Returns `Ok(())` if the record passes the uniform admissibility
/// gates, otherwise the typed exclusion reason.
///
/// Gate order, cheapest check first: body → scope → visibility →
/// confidence. The first failing gate wins; later gates are not
/// evaluated. Tombstoned rows never reach this predicate (the adapter
/// filters them at the store boundary).
pub fn admit(
    record: &MemoryRecord,
    authorized_scope: &ScopeTuple,
    authorized_visibility: &[MemoryVisibility],
) -> Result<(), ExclusionReason> {
    if record.body.is_empty() {
        return Err(ExclusionReason::EmptyBody);
    }
    if !scope_satisfied(authorized_scope, &record.scope) {
        return Err(ExclusionReason::OutOfScope);
    }
    if !authorized_visibility.contains(&record.visibility) {
        return Err(ExclusionReason::VisibilityDenied);
    }
    if record.confidence.is_nan() || record.confidence < CONFIDENCE_FLOOR {
        return Err(ExclusionReason::BelowConfidenceFloor);
    }
    Ok(())
}

/// Every set dimension on `authorized` must equal the record's value
/// on the same dimension. The record may carry additional dimensions
/// (e.g. `session_id`) — those are not narrowed.
fn scope_satisfied(authorized: &ScopeTuple, record: &ScopeTuple) -> bool {
    let pairs: [(&Option<String>, &Option<String>); 7] = [
        (&authorized.tenant, &record.tenant),
        (&authorized.workspace, &record.workspace),
        (&authorized.project, &record.project),
        (&authorized.session_id, &record.session_id),
        (&authorized.entity, &record.entity),
        (&authorized.user, &record.user),
        (&authorized.agent, &record.agent),
    ];
    for (auth, rec) in pairs {
        if let Some(a) = auth
            && Some(a) != rec.as_ref()
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::record::tests_export::sample_record;

    fn allow_private() -> Vec<MemoryVisibility> {
        vec![MemoryVisibility::Private]
    }

    fn matching_scope() -> ScopeTuple {
        ScopeTuple {
            user: Some("hmn:tafeng".to_owned()),
            ..ScopeTuple::default()
        }
    }

    #[test]
    fn admits_canonical_sample_record() {
        let r = sample_record();
        assert_eq!(admit(&r, &matching_scope(), &allow_private()), Ok(()));
    }

    #[test]
    fn rejects_empty_body() {
        let mut r = sample_record();
        r.body = String::new();
        assert_eq!(
            admit(&r, &matching_scope(), &allow_private()),
            Err(ExclusionReason::EmptyBody)
        );
    }

    #[test]
    fn rejects_visibility_not_in_allowlist() {
        let r = sample_record();
        let only_public = vec![MemoryVisibility::Public];
        assert_eq!(
            admit(&r, &matching_scope(), &only_public),
            Err(ExclusionReason::VisibilityDenied)
        );
    }

    #[test]
    fn rejects_below_confidence_floor() {
        let mut r = sample_record();
        r.confidence = 0.29;
        assert_eq!(
            admit(&r, &matching_scope(), &allow_private()),
            Err(ExclusionReason::BelowConfidenceFloor)
        );
    }

    #[test]
    fn rejects_scope_mismatch() {
        let r = sample_record();
        let other_user = ScopeTuple {
            user: Some("hmn:someoneelse".to_owned()),
            ..ScopeTuple::default()
        };
        assert_eq!(
            admit(&r, &other_user, &allow_private()),
            Err(ExclusionReason::OutOfScope)
        );
    }

    #[test]
    fn empty_authorized_scope_admits_any_scope() {
        let r = sample_record();
        let empty = ScopeTuple::default();
        assert_eq!(admit(&r, &empty, &allow_private()), Ok(()));
    }
}
