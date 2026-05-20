//! Body-free metadata attached to a [`super::PolicyTraceEntry`]. Every
//! variant carries only counts, codes, or enum tags; raw bytes,
//! source content, and record bodies never appear here.
//!
//! Mirrors the body-free pattern used by
//! [`crate::pipeline::filter::audit::BlockedAuditEntry`].

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::domain::MemoryVisibility;
use crate::pipeline::filter::{DiscardReason, RedactionTag};
use crate::rebac::{RebacAction, RebacDecisionKind};

/// Body-free metadata for a gate outcome. Serializes via
/// [`Self::to_wire_string`] into a short stable code; the empty string
/// represents [`Self::None`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PolicyDetail {
    /// Gate ran with no extra metadata to surface.
    None,
    /// Filter `should_memorize` returned this discard reason.
    DiscardReason(DiscardReason),
    /// Redactor stripped these tag → count pairs.
    RedactionTagCounts(BTreeMap<RedactionTag, u32>),
    /// Visibility floor resolution chose this tier.
    VisibilityFloor(MemoryVisibility),
    /// Scope check denied; only the *required* tier is echoed (never
    /// the caller's actual scope, which is privileged).
    ScopeMismatch {
        /// Minimum visibility tier the operation required.
        required_tier: MemoryVisibility,
    },
    /// `ReBAC` decision metadata for a shared-tier read or write.
    Rebac {
        /// Operation that was evaluated.
        action: RebacAction,
        /// Visibility tier that was evaluated.
        tier: MemoryVisibility,
        /// Body-free allow/deny reason.
        reason: RebacDecisionKind,
    },
    /// Stable error code. Construction is validated; a free-text
    /// message cannot be slipped in via `&'static str`.
    ErrorCode(PolicyErrorCode),
}

/// A validated error code for [`PolicyDetail::ErrorCode`]. The code
/// MUST match `[a-z][a-z0-9_]{0,63}` — short, lowercase, `snake_case`.
/// This rules out spaces, punctuation, and casing that would let a
/// caller smuggle free-text body content into a policy trace.
///
/// Use the associated `const` shortcuts (`Self::WAL_FAILURE` …) for
/// known codes; fall back to [`Self::from_static`] for new codes
/// declared at the call site as `&'static str` literals — that
/// constructor panics if the code is malformed, fail-closing the
/// body-free invariant in §14 of the brief.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyErrorCode(&'static str);

impl PolicyErrorCode {
    /// Persistent WAL apply failure.
    pub const WAL_FAILURE: Self = Self("wal_failure");
    /// Signed-intent verification failed at the envelope.
    pub const SIGNATURE_INVALID: Self = Self("signature_invalid");
    /// Consent journal append failed (broken chain or I/O).
    pub const CONSENT_APPEND_FAILED: Self = Self("consent_append_failed");

    /// Construct from a `'static` literal. Panics if the code is not
    /// `[a-z][a-z0-9_]{0,63}` — fail-closed against accidental free
    /// text in `policy_trace.detail`.
    #[must_use]
    pub fn from_static(code: &'static str) -> Self {
        assert!(
            Self::is_valid(code),
            "PolicyErrorCode must match [a-z][a-z0-9_]{{0,63}}; got {code:?}"
        );
        Self(code)
    }

    /// `true` if `code` is a well-formed `snake_case` error code.
    #[must_use]
    pub const fn is_valid(code: &str) -> bool {
        let bytes = code.as_bytes();
        if bytes.is_empty() || bytes.len() > 64 {
            return false;
        }
        if !bytes[0].is_ascii_lowercase() {
            return false;
        }
        let mut i = 1;
        while i < bytes.len() {
            let b = bytes[i];
            if !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
                return false;
            }
            i += 1;
        }
        true
    }

    /// The code as a `&'static str`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl PolicyDetail {
    /// Short, stable, human-readable wire form. Body-free by
    /// construction.
    #[must_use]
    pub fn to_wire_string(&self) -> String {
        match self {
            Self::None => String::new(),
            Self::DiscardReason(r) => format!("discard:{}", r.as_str()),
            Self::RedactionTagCounts(counts) => {
                let mut out = String::from("redacted:");
                let mut first = true;
                for (tag, count) in counts {
                    if !first {
                        out.push(',');
                    }
                    let _ = write!(out, "{}={count}", tag.as_str());
                    first = false;
                }
                out
            }
            Self::VisibilityFloor(v) => format!("floor:{}", v.as_str()),
            Self::ScopeMismatch { required_tier } => {
                format!("scope_required:{}", required_tier.as_str())
            }
            Self::Rebac {
                action,
                tier,
                reason,
            } => format!(
                "rebac:{}:{}:{}",
                action.as_str(),
                tier.as_str(),
                reason.as_str()
            ),
            Self::ErrorCode(c) => format!("error:{}", c.as_str()),
        }
    }
}
