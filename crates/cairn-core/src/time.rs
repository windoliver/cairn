//! Date/time + ULID minting helpers shared across surfaces.
//!
//! Pre-issue-53 these were duplicated in `cairn-cli/src/verbs/envelope.rs`,
//! `cairn-sdk/src/stub.rs`, and `cairn-mcp/src/handler.rs`. Centralised here
//! so a future fix touches one site.

use crate::generated::common::Ulid;

/// Return the current UTC time as `YYYY-MM-DDTHH:MM:SSZ` (RFC-3339 with
/// second precision, no fractional second, always UTC).
///
/// Saturating: a misconfigured clock set before `UNIX_EPOCH` yields the epoch
/// string (`"1970-01-01T00:00:00Z"`) instead of panicking. `status`,
/// `handshake`, and `initialize` use this field for human/log triage only —
/// it is volatile and non-critical — so a degraded host must not crash.
///
/// The pre-issue-53 stubs used `.unwrap_or_default()` (saturating to 0 on
/// pre-1970 clocks). This function restores that property.
#[must_use]
pub fn now_rfc3339_seconds() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (y, mo, d, h, mi, s) = secs_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Decompose epoch seconds into UTC `(year, month, day, hour, minute, second)`.
///
/// Pure function; safe across platforms with `time` feature off.
#[must_use]
pub fn secs_to_ymdhms(mut s: u64) -> (u64, u64, u64, u64, u64, u64) {
    let sec = s % 60;
    s /= 60;
    let min = s % 60;
    s /= 60;
    let hour = s % 24;
    s /= 24;
    let mut days = s;
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let leap = is_leap(year);
    let months = [
        31u64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u64;
    for &m in &months {
        if days < m {
            break;
        }
        days -= m;
        month += 1;
    }
    (year, month, days + 1, hour, min, sec)
}

/// Gregorian leap year predicate.
#[must_use]
pub fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

/// Mint a fresh ULID for use as `operation_id` / `incarnation`.
#[must_use]
#[allow(clippy::expect_used)]
pub fn new_operation_id() -> Ulid {
    Ulid(ulid::Ulid::new().to_string())
}

/// Current Unix epoch time in milliseconds.
///
/// Saturating: a misconfigured clock set before `UNIX_EPOCH` yields `0` instead
/// of panicking (same fallback the pre-issue-53 `stub.rs::now_ms` used).
/// Milliseconds overflow into `u64` is not possible until the year
/// 584554051223 — at that point the value saturates to `u64::MAX`.
#[must_use]
pub fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX) // saturate on overflow at year 584554051223
}

/// Build profile (`"debug"` or `"release"`) for status `server_info.build`.
#[must_use]
pub fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secs_to_ymdhms_epoch() {
        assert_eq!(secs_to_ymdhms(0), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn secs_to_ymdhms_y2k_leap() {
        let (y, mo, d, _, _, _) = secs_to_ymdhms(951_782_400);
        assert_eq!((y, mo, d), (2000, 2, 29));
    }

    #[test]
    fn is_leap_known_values() {
        assert!(is_leap(2000));
        assert!(!is_leap(1900));
        assert!(is_leap(2004));
        assert!(!is_leap(2001));
    }

    #[test]
    fn now_rfc3339_seconds_shape() {
        let s = now_rfc3339_seconds();
        assert_eq!(s.len(), 20);
        assert!(s.ends_with('Z'));
        assert!(s.contains('T'));
    }

    #[test]
    fn new_operation_id_is_26_char_crockford() {
        let id = new_operation_id();
        assert_eq!(id.0.len(), 26);
        assert!(id.0.bytes().all(|b| matches!(
            b,
            b'0'..=b'9'
                | b'A'..=b'H'
                | b'J'
                | b'K'
                | b'M'
                | b'N'
                | b'P'..=b'T'
                | b'V'..=b'Z'
        )));
    }

    #[test]
    fn now_ms_is_nonzero() {
        assert!(now_ms() > 0);
    }

    /// Regression baseline for the saturating-clock fix (round-4 review).
    ///
    /// Note: Rust's `SystemTime` offers no safe way to synthesize a
    /// pre-UNIX_EPOCH instant in a unit test without unsafe time-mocking, so
    /// this test asserts the structural shape — either the real post-2024 clock
    /// or the epoch fallback (0) — rather than exercising the `unwrap_or_default`
    /// branch directly. The panic-prevention property is encoded in the use of
    /// `unwrap_or_default()` in the implementation.
    #[test]
    fn now_rfc3339_seconds_returns_valid_string_under_normal_clock() {
        let s = now_rfc3339_seconds();
        assert_eq!(s.len(), 20, "must be 20 chars: {s}");
        assert!(s.ends_with('Z'), "must end with Z: {s}");
        // Either a real post-2026 timestamp or the epoch fallback.
        assert!(
            s.as_str() >= "2026-01-01T00:00:00Z" || s == "1970-01-01T00:00:00Z",
            "must be a real timestamp or epoch fallback; got {s}"
        );
    }

    /// Regression baseline for the saturating-clock fix (round-4 review).
    ///
    /// See `now_rfc3339_seconds_returns_valid_string_under_normal_clock` for
    /// the same caveat about not being able to force a pre-epoch clock value
    /// without unsafe mocking.
    #[test]
    fn now_ms_returns_finite_value_under_normal_clock() {
        let m = now_ms();
        // Either real time (post-2024 → > 1.7e12 ms) or epoch fallback (0).
        assert!(
            m == 0 || m > 1_700_000_000_000u64,
            "must be epoch fallback or post-2024; got {m}"
        );
    }

    #[test]
    fn build_profile_is_debug_or_release() {
        let p = build_profile();
        assert!(p == "debug" || p == "release");
    }
}
