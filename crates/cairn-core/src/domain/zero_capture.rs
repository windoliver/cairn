//! Zero-capture session audit helpers.
//!
//! This module decides whether a session with meaningful activity but no
//! successful Cairn writes should surface a retrospective reminder.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::domain::SessionId;

/// Reminder timing chosen by the future consumer integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZeroCaptureTrigger {
    /// Evaluate at session stop.
    Stop,
    /// Evaluate at the next safe hook point.
    SafeHookPoint,
}

/// Input to the zero-capture audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZeroCaptureAuditInput {
    /// Session being evaluated.
    pub session_id: SessionId,
    /// Count of meaningful user/tool activity observed by the consumer.
    pub activity_count: u64,
    /// Successful `ingest` writes in the session.
    pub successful_ingest_writes: u64,
    /// Successful `capture_trace` writes in the session.
    pub successful_capture_trace_writes: u64,
    /// Config gate for the reminder behavior.
    pub nudges_enabled: bool,
    /// Policy/consent gate for reminder visibility.
    pub reminder_allowed: bool,
    /// Hook timing chosen by the caller.
    pub trigger: ZeroCaptureTrigger,
}

impl ZeroCaptureAuditInput {
    /// Total successful writes observed for this session.
    #[must_use]
    pub fn successful_write_count(&self) -> u64 {
        self.successful_ingest_writes + self.successful_capture_trace_writes
    }
}

/// Reasons a reminder is suppressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZeroCaptureSuppression {
    /// No meaningful activity occurred in the session.
    NoMeaningfulActivity,
    /// At least one successful write was already recorded.
    WritesPresent,
    /// Config explicitly disabled the reminder behavior.
    DisabledByConfig,
    /// Policy or consent prevented the reminder from surfacing.
    PolicyBlocked,
}

/// Body-free reminder payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZeroCaptureNudge {
    /// Session that should receive the reminder.
    pub session_id: SessionId,
    /// Count of meaningful activity in the session.
    pub activity_count: u64,
    /// Derived successful write count.
    pub successful_write_count: u64,
    /// Timing at which the consumer should surface the reminder.
    pub trigger: ZeroCaptureTrigger,
}

/// Final audit decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ZeroCaptureDecision {
    /// No reminder should be emitted.
    NoNudge {
        /// Reason the reminder was suppressed.
        reason: ZeroCaptureSuppression,
    },
    /// Emit a reminder through the consumer-visible channel.
    EmitNudge(ZeroCaptureNudge),
}

/// Serial form used by future reporting surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZeroCaptureDecisionCode {
    /// Session had no meaningful activity.
    NoMeaningfulActivity,
    /// Session already had a successful write.
    WritesPresent,
    /// Reminder suppressed by config.
    DisabledByConfig,
    /// Reminder suppressed by policy.
    PolicyBlocked,
    /// Reminder should be emitted.
    EmitNudge,
}

impl ZeroCaptureDecisionCode {
    /// Stable `snake_case` label for report text and other human-readable surfaces.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoMeaningfulActivity => "no_meaningful_activity",
            Self::WritesPresent => "writes_present",
            Self::DisabledByConfig => "disabled_by_config",
            Self::PolicyBlocked => "policy_blocked",
            Self::EmitNudge => "emit_nudge",
        }
    }
}

/// Body-free report shape for future `lint` or dogfood reporting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZeroCaptureReport {
    /// Session evaluated by the audit.
    pub session_id: SessionId,
    /// Count of meaningful activity.
    pub activity_count: u64,
    /// Derived successful write count.
    pub successful_write_count: u64,
    /// Compact decision code.
    pub decision: ZeroCaptureDecisionCode,
}

/// Aggregate counts for a batch of zero-capture reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ZeroCaptureReportSummary {
    /// Total number of reports aggregated.
    pub total: u64,
    /// Reports that should emit a reminder.
    pub emit_nudge: u64,
    /// Reports suppressed because no activity occurred.
    pub no_meaningful_activity: u64,
    /// Reports suppressed because writes were already present.
    pub writes_present: u64,
    /// Reports suppressed by config.
    pub disabled_by_config: u64,
    /// Reports suppressed by policy.
    pub policy_blocked: u64,
}

impl ZeroCaptureReportSummary {
    /// Aggregate a slice of zero-capture reports.
    #[must_use]
    pub fn from_reports(reports: &[ZeroCaptureReport]) -> Self {
        let mut summary = Self::default();
        for report in reports {
            summary.total += 1;
            match report.decision {
                ZeroCaptureDecisionCode::NoMeaningfulActivity => {
                    summary.no_meaningful_activity += 1;
                }
                ZeroCaptureDecisionCode::WritesPresent => {
                    summary.writes_present += 1;
                }
                ZeroCaptureDecisionCode::DisabledByConfig => {
                    summary.disabled_by_config += 1;
                }
                ZeroCaptureDecisionCode::PolicyBlocked => {
                    summary.policy_blocked += 1;
                }
                ZeroCaptureDecisionCode::EmitNudge => {
                    summary.emit_nudge += 1;
                }
            }
        }
        summary
    }
}

impl ZeroCaptureReport {
    /// Build a report from a previously computed decision.
    #[must_use]
    pub fn from_decision(input: &ZeroCaptureAuditInput, decision: &ZeroCaptureDecision) -> Self {
        let decision = match decision {
            ZeroCaptureDecision::NoNudge { reason } => match reason {
                ZeroCaptureSuppression::NoMeaningfulActivity => {
                    ZeroCaptureDecisionCode::NoMeaningfulActivity
                }
                ZeroCaptureSuppression::WritesPresent => ZeroCaptureDecisionCode::WritesPresent,
                ZeroCaptureSuppression::DisabledByConfig => {
                    ZeroCaptureDecisionCode::DisabledByConfig
                }
                ZeroCaptureSuppression::PolicyBlocked => ZeroCaptureDecisionCode::PolicyBlocked,
            },
            ZeroCaptureDecision::EmitNudge(_) => ZeroCaptureDecisionCode::EmitNudge,
        };
        Self {
            session_id: input.session_id.clone(),
            activity_count: input.activity_count,
            successful_write_count: input.successful_write_count(),
            decision,
        }
    }
}

/// Render a markdown dogfood report for a batch of zero-capture reports.
#[must_use]
pub fn render_zero_capture_report(reports: &[ZeroCaptureReport]) -> String {
    let summary = ZeroCaptureReportSummary::from_reports(reports);
    let mut out = String::new();
    out.push_str("# Zero-capture report\n\n");
    let _ = writeln!(out, "- total: {}", summary.total);
    let _ = writeln!(out, "- emit_nudge: {}", summary.emit_nudge);
    let _ = writeln!(
        out,
        "- no_meaningful_activity: {}",
        summary.no_meaningful_activity
    );
    let _ = writeln!(out, "- writes_present: {}", summary.writes_present);
    let _ = writeln!(out, "- disabled_by_config: {}", summary.disabled_by_config);
    let _ = writeln!(out, "- policy_blocked: {}\n", summary.policy_blocked);

    if reports.is_empty() {
        out.push_str("_no sessions_\n");
        return out;
    }

    out.push_str("## sessions\n\n");
    for report in reports {
        let _ = writeln!(
            out,
            "- session: {}\n  - decision: {}\n  - activity_count: {}\n  - successful_write_count: {}",
            report.session_id,
            report.decision.as_str(),
            report.activity_count,
            report.successful_write_count
        );
    }
    out
}

/// Decide whether a zero-capture reminder should be surfaced.
#[must_use]
pub fn decide_zero_capture_nudge(input: &ZeroCaptureAuditInput) -> ZeroCaptureDecision {
    if input.activity_count == 0 {
        return ZeroCaptureDecision::NoNudge {
            reason: ZeroCaptureSuppression::NoMeaningfulActivity,
        };
    }
    if !input.nudges_enabled {
        return ZeroCaptureDecision::NoNudge {
            reason: ZeroCaptureSuppression::DisabledByConfig,
        };
    }
    if !input.reminder_allowed {
        return ZeroCaptureDecision::NoNudge {
            reason: ZeroCaptureSuppression::PolicyBlocked,
        };
    }
    let successful_write_count = input.successful_write_count();
    if successful_write_count > 0 {
        return ZeroCaptureDecision::NoNudge {
            reason: ZeroCaptureSuppression::WritesPresent,
        };
    }
    ZeroCaptureDecision::EmitNudge(ZeroCaptureNudge {
        session_id: input.session_id.clone(),
        activity_count: input.activity_count,
        successful_write_count,
        trigger: input.trigger,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SessionId;

    fn input() -> ZeroCaptureAuditInput {
        ZeroCaptureAuditInput {
            session_id: SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV")
                .expect("invariant: valid session id"),
            activity_count: 3,
            successful_ingest_writes: 0,
            successful_capture_trace_writes: 0,
            nudges_enabled: true,
            reminder_allowed: true,
            trigger: ZeroCaptureTrigger::Stop,
        }
    }

    #[test]
    fn emit_nudge_for_activity_and_zero_writes() {
        let decision = decide_zero_capture_nudge(&input());
        assert!(matches!(decision, ZeroCaptureDecision::EmitNudge(_)));
    }

    #[test]
    fn suppress_when_any_ingest_write_present() {
        let mut input = input();
        input.successful_ingest_writes = 1;
        let decision = decide_zero_capture_nudge(&input);
        assert!(matches!(
            decision,
            ZeroCaptureDecision::NoNudge {
                reason: ZeroCaptureSuppression::WritesPresent
            }
        ));
    }

    #[test]
    fn suppress_when_any_capture_trace_write_present() {
        let mut input = input();
        input.successful_capture_trace_writes = 1;
        let decision = decide_zero_capture_nudge(&input);
        assert!(matches!(
            decision,
            ZeroCaptureDecision::NoNudge {
                reason: ZeroCaptureSuppression::WritesPresent
            }
        ));
    }

    #[test]
    fn suppress_when_disabled_in_config() {
        let mut input = input();
        input.nudges_enabled = false;
        let decision = decide_zero_capture_nudge(&input);
        assert!(matches!(
            decision,
            ZeroCaptureDecision::NoNudge {
                reason: ZeroCaptureSuppression::DisabledByConfig
            }
        ));
    }

    #[test]
    fn suppress_when_policy_blocked() {
        let mut input = input();
        input.reminder_allowed = false;
        let decision = decide_zero_capture_nudge(&input);
        assert!(matches!(
            decision,
            ZeroCaptureDecision::NoNudge {
                reason: ZeroCaptureSuppression::PolicyBlocked
            }
        ));
    }

    #[test]
    fn suppress_when_no_activity() {
        let mut input = input();
        input.activity_count = 0;
        let decision = decide_zero_capture_nudge(&input);
        assert!(matches!(
            decision,
            ZeroCaptureDecision::NoNudge {
                reason: ZeroCaptureSuppression::NoMeaningfulActivity
            }
        ));
    }

    #[test]
    fn emit_nudge_report_is_body_free_and_derived() {
        let input = input();
        let decision = decide_zero_capture_nudge(&input);
        let report = ZeroCaptureReport::from_decision(&input, &decision);
        assert_eq!(report.activity_count, 3);
        assert_eq!(report.successful_write_count, 0);
        assert_eq!(report.decision, ZeroCaptureDecisionCode::EmitNudge);
    }

    #[test]
    fn summary_counts_reports_by_decision() {
        let emit = ZeroCaptureReport::from_decision(&input(), &decide_zero_capture_nudge(&input()));
        let mut writes_input = input();
        writes_input.successful_ingest_writes = 1;
        let writes = ZeroCaptureReport::from_decision(
            &writes_input,
            &decide_zero_capture_nudge(&writes_input),
        );
        let summary = ZeroCaptureReportSummary::from_reports(&[emit, writes]);
        assert_eq!(summary.total, 2);
        assert_eq!(summary.emit_nudge, 1);
        assert_eq!(summary.writes_present, 1);
        assert_eq!(summary.no_meaningful_activity, 0);
    }

    #[test]
    fn markdown_report_renders_summary_and_sessions() {
        let report =
            ZeroCaptureReport::from_decision(&input(), &decide_zero_capture_nudge(&input()));
        let markdown = render_zero_capture_report(&[report]);
        assert!(markdown.contains("# Zero-capture report"));
        assert!(markdown.contains("- emit_nudge: 1"));
        assert!(markdown.contains("session: 01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        assert!(markdown.contains("decision: emit_nudge"));
        assert!(markdown.contains("successful_write_count: 0"));
    }
}
