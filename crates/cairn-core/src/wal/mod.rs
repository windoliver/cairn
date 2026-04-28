//! WAL module stub for #46.
//!
//! Defines `ApplyToken` — the zero-sized witness type that the WAL executor
//! must present to call write methods. The constructor is `pub(super)` here
//! so only code in this module (or the executor that lands in #8) can mint
//! one. The type is re-exported through
//! `contract::memory_store::apply::ApplyToken`.
//!
//! The WAL executor itself lands in #8; this stub only provides the token
//! construction site and the test-util helper.

/// Witness that the caller is the WAL state-machine executor.
///
/// Constructable only from within `cairn_core::wal` (via
/// `pub(super) fn __new()`). The visibility modifier blocks external
/// construction at compile time, turning non-WAL writes into a compile
/// error rather than a runtime failure.
pub struct ApplyToken {
    pub(super) _private: (),
}

impl ApplyToken {
    /// **Internal.** Only code within `cairn_core::wal` can call this.
    pub(super) fn __new() -> Self {
        Self { _private: () }
    }
}

/// **Tests only.** Constructs an [`ApplyToken`] without involving the WAL
/// executor.
///
/// Gated behind `#[cfg(any(test, feature = "test-util"))]` so non-test
/// production builds cannot mint one outside the WAL module.
#[cfg(any(test, feature = "test-util"))]
#[must_use]
pub fn test_apply_token() -> ApplyToken {
    ApplyToken::__new()
}
