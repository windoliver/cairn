//! WAL module stub for #46.
//!
//! Defines `ApplyToken` — the zero-sized witness type that the WAL executor
//! must present to call write methods. The constructor is private so only
//! code within `cairn_core::wal` itself can mint one. The type is
//! re-exported through `contract::memory_store::apply::ApplyToken`.
//!
//! The WAL executor itself lands in #8; this stub only provides the token
//! construction site and the test-util helper.

/// Witness that the caller is the WAL state-machine executor.
///
/// Constructable only from within `cairn_core::wal` (via the private
/// `new()`). The private `_private` field blocks external struct-literal
/// construction at compile time, turning non-WAL writes into a compile
/// error rather than a runtime failure.
pub struct ApplyToken {
    _private: (),
}

impl ApplyToken {
    /// **Internal.** Only code within `cairn_core::wal` can call this.
    /// Private visibility confines minting to this module; `pub(super)`
    /// would expose it to all of `cairn-core` because `wal` is at the
    /// crate root.
    ///
    /// `dead_code` is suppressed here because the sole caller
    /// (`test_apply_token`) is cfg-gated behind `test`/`test-util` and
    /// the WAL executor (the production caller) lands in issue #8.
    #[allow(dead_code)] // sole caller is cfg(test/test-util); WAL executor lands in #8
    fn new() -> Self {
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
    ApplyToken::new()
}
