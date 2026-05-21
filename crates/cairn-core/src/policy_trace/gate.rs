//! Closed gate vocabulary. `Display` emits the stable wire string used by
//! `PolicyTraceEntry::to_wire` and surfaced verbatim on the
//! `ResponsePolicyTrace.gate` field.

use std::fmt;

/// Every gate that can fire across all eight verbs (brief §5.2, §6.3,
/// §4.2, §14, §8). Adding a variant is a `#[non_exhaustive]` minor
/// change. A vocabulary *break* (rename, semantic shift) travels with
/// the MCP contract version per the design doc §8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PolicyGate {
    /// Pre-persist PII / secret redaction (brief §5.2, §14).
    PresidioRedaction,
    /// Prompt-injection fencing wrap (brief §5.2).
    PromptInjectionFence,
    /// Filter `should_memorize` decision (brief §5.2).
    FilterShouldMemorize,
    /// Default visibility resolution (brief §6.3).
    VisibilityFloor,
    /// Scope tuple check (brief §4.2).
    ScopeCheck,
    /// Forget capability check on `forget` (brief §8).
    ForgetCapability,
    /// Atomic consent journal append (brief §14, §5.6).
    ConsentJournalAppend,
    /// Read-path relevance filter (brief §5.1; --explain only).
    ReadFilterRelevance,
    /// Read-path staleness filter (brief §5.1; --explain only).
    ReadFilterStaleness,
    /// Read-path duplicate filter (brief §5.1; --explain only).
    ReadFilterDedup,
    /// Search auth-scope gate.
    SearchScope,
    /// Search mode/capability gate.
    SearchCapability,
    /// Search read-filter aggregation gate.
    SearchReadFilter,
    /// `ReBAC` relation gate for shared-tier reads and writes.
    Rebac,
    /// Consent receipt gate for shared-tier promotion.
    ConsentReceipt,
    /// Signed share-link grant gate.
    ShareLink,
    /// Local sensor consent gate.
    SensorConsent,
}

impl PolicyGate {
    /// Wire-format identifier. Stable across surfaces.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PresidioRedaction => "presidio_redaction",
            Self::PromptInjectionFence => "prompt_injection_fence",
            Self::FilterShouldMemorize => "filter_should_memorize",
            Self::VisibilityFloor => "visibility_floor",
            Self::ScopeCheck => "scope_check",
            Self::ForgetCapability => "forget_capability",
            Self::ConsentJournalAppend => "consent_journal_append",
            Self::ReadFilterRelevance => "read_filter_relevance",
            Self::ReadFilterStaleness => "read_filter_staleness",
            Self::ReadFilterDedup => "read_filter_dedup",
            Self::SearchScope => "search.scope",
            Self::SearchCapability => "search.capability",
            Self::SearchReadFilter => "search.read_filter",
            Self::Rebac => "rebac",
            Self::ConsentReceipt => "consent_receipt",
            Self::ShareLink => "share_link",
            Self::SensorConsent => "sensor_consent",
        }
    }
}

impl fmt::Display for PolicyGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
