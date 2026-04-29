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
    /// Stable static error code (never a free message).
    ErrorCode(&'static str),
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
            Self::ErrorCode(c) => format!("error:{c}"),
        }
    }
}
