//! RFC3339 timestamp newtype.
//!
//! Domain timestamps are wall-clock instants written with the
//! `YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM]` form per the brief (§4.2 envelope).
//! We avoid pulling `chrono`/`time` into core; a focused validator is enough
//! to reject obviously wrong strings before they reach the store layer.

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

/// Validated RFC3339 timestamp. Wire form is the original string.
///
/// `PartialOrd`/`Ord` are deliberately **not** implemented: lexical
/// comparison of the raw string disagrees with chronological order once
/// timezone offsets are involved (`2026-04-22T15:00:00+02:00` happens
/// before `2026-04-22T14:00:00Z` chronologically but sorts after it
/// lexically). Callers that need chronological ordering should parse the
/// underlying string into a real datetime type at the boundary where the
/// dependency is acceptable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Rfc3339Timestamp(String);

impl Rfc3339Timestamp {
    /// Parse a timestamp. Validates structure (date + 'T' + time + zone) and
    /// component ranges (month 1–12, day 1–31, hour 0–23, etc.). Returns
    /// [`DomainError::InvalidTimestamp`] on failure.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        validate(&raw).map_err(|message| DomainError::InvalidTimestamp {
            message: format!("`{raw}`: {message}"),
        })?;
        Ok(Self(raw))
    }

    /// Underlying RFC3339 string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Rfc3339Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Rfc3339Timestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

fn validate(s: &str) -> Result<(), &'static str> {
    let bytes = s.as_bytes();
    if bytes.len() < 20 {
        return Err("too short for RFC3339");
    }
    let date = &bytes[..10];
    if date[4] != b'-' || date[7] != b'-' {
        return Err("date separators must be `-`");
    }
    if !date[..4].iter().all(u8::is_ascii_digit)
        || !date[5..7].iter().all(u8::is_ascii_digit)
        || !date[8..10].iter().all(u8::is_ascii_digit)
    {
        return Err("date components must be digits");
    }
    let year = u16_digits(&date[..4]);
    let month = u8_digits(&date[5..7]);
    let day = u8_digits(&date[8..10]);
    if !(1..=12).contains(&month) {
        return Err("month out of range");
    }
    let max_day = days_in_month(year, month);
    if day < 1 || day > max_day {
        return Err("day out of range for month");
    }
    if bytes[10] != b'T' && bytes[10] != b't' {
        return Err("expected `T` between date and time");
    }
    let time = &bytes[11..19];
    if time[2] != b':' || time[5] != b':' {
        return Err("time separators must be `:`");
    }
    if !time[..2].iter().all(u8::is_ascii_digit)
        || !time[3..5].iter().all(u8::is_ascii_digit)
        || !time[6..8].iter().all(u8::is_ascii_digit)
    {
        return Err("time components must be digits");
    }
    let hour = u8_digits(&time[..2]);
    let minute = u8_digits(&time[3..5]);
    let second = u8_digits(&time[6..8]);
    if hour > 23 {
        return Err("hour out of range");
    }
    if minute > 59 {
        return Err("minute out of range");
    }
    // RFC3339 permits `:60` for leap-second instants, but Cairn has no
    // leap-second table to validate when one is real. Reject `:60` rather
    // than accept arbitrary mid-day leap seconds — once a real datetime
    // parser is wired in at the store layer it can relax this.
    if second > 59 {
        return Err("second out of range (`:60` leap seconds not supported)");
    }

    let mut idx = 19;
    if idx < bytes.len() && bytes[idx] == b'.' {
        idx += 1;
        let frac_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        if idx == frac_start {
            return Err("fractional seconds must contain at least one digit");
        }
        // Cap at 9 digits — the ordering parser uses ns precision and
        // would silently truncate longer tails, hiding chronological
        // inversions.
        if idx - frac_start > 9 {
            return Err("fractional seconds must be at most 9 digits (nanosecond precision)");
        }
    }

    if idx >= bytes.len() {
        return Err("missing timezone designator");
    }

    match bytes[idx] {
        b'Z' | b'z' => {
            if idx + 1 != bytes.len() {
                return Err("trailing data after `Z`");
            }
        }
        b'+' | b'-' => {
            if idx + 6 != bytes.len() {
                return Err("offset must be `±HH:MM`");
            }
            let off = &bytes[idx + 1..idx + 6];
            if off[2] != b':' {
                return Err("offset separator must be `:`");
            }
            if !off[..2].iter().all(u8::is_ascii_digit) || !off[3..5].iter().all(u8::is_ascii_digit)
            {
                return Err("offset components must be digits");
            }
            let oh = u8_digits(&off[..2]);
            let om = u8_digits(&off[3..5]);
            if oh > 23 || om > 59 {
                return Err("offset out of range");
            }
        }
        _ => return Err("expected `Z` or `±HH:MM` timezone designator"),
    }
    Ok(())
}

fn u8_digits(bytes: &[u8]) -> u8 {
    let mut acc: u16 = 0;
    for b in bytes {
        acc = acc * 10 + u16::from(b - b'0');
    }
    u8::try_from(acc.min(255)).unwrap_or(255)
}

fn u16_digits(bytes: &[u8]) -> u16 {
    let mut acc: u32 = 0;
    for b in bytes {
        acc = acc * 10 + u32::from(b - b'0');
    }
    u16::try_from(acc.min(u32::from(u16::MAX))).unwrap_or(u16::MAX)
}

const fn is_leap(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_z_form() {
        Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid");
    }

    #[test]
    fn accepts_offset_form() {
        Rfc3339Timestamp::parse("2026-04-22T14:02:11+02:00").expect("valid");
    }

    #[test]
    fn accepts_fractional() {
        Rfc3339Timestamp::parse("2026-04-22T14:02:11.123Z").expect("valid");
    }

    #[test]
    fn rejects_no_zone() {
        let err = Rfc3339Timestamp::parse("2026-04-22T14:02:11").unwrap_err();
        assert!(matches!(err, DomainError::InvalidTimestamp { .. }));
    }

    #[test]
    fn rejects_bad_month() {
        let err = Rfc3339Timestamp::parse("2026-13-22T14:02:11Z").unwrap_err();
        assert!(matches!(err, DomainError::InvalidTimestamp { .. }));
    }

    #[test]
    fn rejects_bad_offset() {
        let err = Rfc3339Timestamp::parse("2026-04-22T14:02:11+0200").unwrap_err();
        assert!(matches!(err, DomainError::InvalidTimestamp { .. }));
    }

    #[test]
    fn json_round_trip() {
        let ts = Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid");
        let s = serde_json::to_string(&ts).expect("ser");
        let back: Rfc3339Timestamp = serde_json::from_str(&s).expect("de");
        assert_eq!(back, ts);
    }

    #[test]
    fn rejects_feb_30() {
        let err = Rfc3339Timestamp::parse("2026-02-30T00:00:00Z").unwrap_err();
        assert!(matches!(err, DomainError::InvalidTimestamp { .. }));
    }

    #[test]
    fn rejects_apr_31() {
        let err = Rfc3339Timestamp::parse("2026-04-31T00:00:00Z").unwrap_err();
        assert!(matches!(err, DomainError::InvalidTimestamp { .. }));
    }

    #[test]
    fn rejects_feb_29_non_leap() {
        let err = Rfc3339Timestamp::parse("2025-02-29T00:00:00Z").unwrap_err();
        assert!(matches!(err, DomainError::InvalidTimestamp { .. }));
    }

    #[test]
    fn accepts_feb_29_leap() {
        Rfc3339Timestamp::parse("2024-02-29T00:00:00Z").expect("leap year");
    }

    #[test]
    fn rejects_century_non_leap() {
        let err = Rfc3339Timestamp::parse("1900-02-29T00:00:00Z").unwrap_err();
        assert!(matches!(err, DomainError::InvalidTimestamp { .. }));
    }

    #[test]
    fn accepts_400_year_leap() {
        Rfc3339Timestamp::parse("2000-02-29T00:00:00Z").expect("400-year leap");
    }

    #[test]
    fn rejects_leap_second_60() {
        let err = Rfc3339Timestamp::parse("2026-04-22T14:02:60Z").unwrap_err();
        assert!(matches!(err, DomainError::InvalidTimestamp { .. }));
    }
}
