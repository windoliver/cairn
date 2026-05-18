//! §3 projection-drift findings. Pure — caller supplies pre-computed statuses.
//!
//! On-disk markdown projection drift is a warning, not an error: the DB is
//! authoritative and `lint --fix-markdown` rebuilds the projection on demand.

use crate::domain::projection::ProjectionStatus;
use crate::generated::common::Ulid;
use crate::generated::verbs::lint::{Finding, Kind, Severity, Target};

/// Build a finding for a single (`record_id`, `projection_path`, `status`) tuple.
///
/// Returns `None` when the status is `Match`.
#[must_use]
pub fn finding_for(
    record_id: &str,
    projection_path: &str,
    status: &ProjectionStatus,
) -> Option<Finding> {
    // The CLI smuggles non-auto-repairable conditions through the
    // `actual_body_hash` field with a sentinel prefix:
    //   * `unsafe-projection-path:` — symlink / non-regular file at
    //     the projection path. `--fix-markdown` refuses to write
    //     through those, so the operator must clean the path first.
    //   * `read-error:` — the projection file exists but cannot be
    //     read (permission denied, invalid UTF-8, hardware I/O
    //     error). `--fix-markdown` will overwrite the file IF its
    //     parent dir is writeable, but the failure usually points
    //     at a real integrity problem the operator must triage.
    // Both must escalate to Error severity so `cairn lint` exits
    // non-zero — a green CI run that ships unrepairable drift is
    // strictly worse than a noisy warning.
    let (unsafe_path, read_error) = match status {
        ProjectionStatus::Drift {
            actual_body_hash, ..
        } => (
            actual_body_hash.starts_with("unsafe-projection-path:"),
            actual_body_hash.starts_with("read-error:"),
        ),
        _ => (false, false),
    };
    let needs_manual_attention = unsafe_path || read_error;
    let (kind, message) = match status {
        ProjectionStatus::Match => return None,
        ProjectionStatus::Drift {
            expected_body_hash,
            actual_body_hash,
        } => (
            Kind::ProjectionDrift,
            format!(
                "projection at {projection_path} drifts from db; \
                 expected_body_hash={expected_body_hash} \
                 actual_body_hash={actual_body_hash}",
            ),
        ),
        ProjectionStatus::Missing { expected_body_hash } => (
            Kind::ProjectionMissing,
            format!(
                "projection at {projection_path} is missing; \
                 expected_body_hash={expected_body_hash}",
            ),
        ),
    };
    let suggested_fix = if unsafe_path {
        Some(
            "remove the symlink or non-regular file at the projection path, \
             then re-run `cairn lint --fix-markdown`"
                .to_owned(),
        )
    } else if read_error {
        Some(
            "investigate the I/O failure (permissions, encoding, hardware) \
             before re-running `cairn lint --fix-markdown`"
                .to_owned(),
        )
    } else {
        Some("run `cairn lint --fix-markdown`".to_owned())
    };
    let severity = if needs_manual_attention {
        Severity::Error
    } else {
        Severity::Warning
    };
    Some(Finding {
        entities: None,
        kind,
        message,
        severity,
        suggested_fix,
        target: Some(Target {
            operation_id: None,
            path: Some(projection_path.to_owned()),
            record_id: Some(Ulid(record_id.to_owned())),
        }),
        tracking_issue: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_returns_none() {
        assert!(finding_for("01HQZ", "raw/x.md", &ProjectionStatus::Match).is_none());
    }

    #[test]
    fn drift_emits_projectiondrift_warning_with_both_hashes() {
        let f = finding_for(
            "01HQZ",
            "raw/x.md",
            &ProjectionStatus::Drift {
                expected_body_hash: "aaaa".into(),
                actual_body_hash: "bbbb".into(),
            },
        )
        .unwrap();
        assert!(matches!(f.kind, Kind::ProjectionDrift));
        assert!(matches!(f.severity, Severity::Warning));
        assert!(f.message.contains("expected_body_hash=aaaa"));
        assert!(f.message.contains("actual_body_hash=bbbb"));
        assert_eq!(
            f.target.as_ref().and_then(|t| t.path.as_deref()),
            Some("raw/x.md")
        );
    }

    #[test]
    fn missing_emits_projectionmissing_warning_with_expected_hash() {
        let f = finding_for(
            "01HQZ",
            "raw/x.md",
            &ProjectionStatus::Missing {
                expected_body_hash: "aaaa".into(),
            },
        )
        .unwrap();
        assert!(matches!(f.kind, Kind::ProjectionMissing));
        assert!(matches!(f.severity, Severity::Warning));
        assert!(f.message.contains("expected_body_hash=aaaa"));
        assert!(!f.message.contains("actual_body_hash"));
    }

    #[test]
    fn finding_carries_record_id_in_target() {
        let f = finding_for(
            "01HQZ12345",
            "raw/x.md",
            &ProjectionStatus::Missing {
                expected_body_hash: "aa".into(),
            },
        )
        .unwrap();
        let t = f.target.as_ref().expect("target");
        assert!(t.record_id.is_some());
    }

    #[test]
    fn drift_includes_suggested_fix() {
        let f = finding_for(
            "01HQZ",
            "raw/x.md",
            &ProjectionStatus::Drift {
                expected_body_hash: "aa".into(),
                actual_body_hash: "bb".into(),
            },
        )
        .unwrap();
        assert_eq!(
            f.suggested_fix.as_deref(),
            Some("run `cairn lint --fix-markdown`")
        );
    }

    #[test]
    fn unsafe_projection_path_escalates_to_error_with_manual_fix() {
        let f = finding_for(
            "01HQZ",
            "raw/x.md",
            &ProjectionStatus::Drift {
                expected_body_hash: "aa".into(),
                actual_body_hash: "unsafe-projection-path: symlink".into(),
            },
        )
        .unwrap();
        assert!(matches!(f.severity, Severity::Error));
        assert!(
            f.suggested_fix
                .as_deref()
                .unwrap()
                .contains("remove the symlink")
        );
    }

    #[test]
    fn read_error_escalates_to_error_with_manual_fix() {
        let f = finding_for(
            "01HQZ",
            "raw/x.md",
            &ProjectionStatus::Drift {
                expected_body_hash: "aa".into(),
                actual_body_hash: "read-error: permission denied".into(),
            },
        )
        .unwrap();
        assert!(matches!(f.severity, Severity::Error));
        assert!(
            f.suggested_fix
                .as_deref()
                .unwrap()
                .contains("investigate the I/O failure")
        );
    }

    #[test]
    fn missing_includes_suggested_fix() {
        let f = finding_for(
            "01HQZ",
            "raw/x.md",
            &ProjectionStatus::Missing {
                expected_body_hash: "aa".into(),
            },
        )
        .unwrap();
        assert_eq!(
            f.suggested_fix.as_deref(),
            Some("run `cairn lint --fix-markdown`")
        );
    }
}
