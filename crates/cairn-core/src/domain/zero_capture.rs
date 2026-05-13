//! Zero-capture session audit helpers.
//!
//! This module decides whether a session with meaningful activity but no
//! successful Cairn writes should surface a retrospective reminder.

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

impl ZeroCaptureReport {
    /// Build a report from a previously computed decision.
    #[must_use]
    pub fn from_decision(
        input: &ZeroCaptureAuditInput,
        decision: &ZeroCaptureDecision,
    ) -> Self {
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
}
