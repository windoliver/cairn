//! RFC3339 timestamp newtype.
//!
//! Domain timestamps are wall-clock instants written with the
//! `YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM]` form per the brief (§4.2 envelope).
//! We avoid pulling `chrono`/`time` into core; a focused validator is enough
//! to reject obviously wrong strings before they reach the store layer.

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

/// Validated RFC3339 timestamp. Wire form is the original string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
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
    let month = u8_digits(&date[5..7]);
    let day = u8_digits(&date[8..10]);
    if !(1..=12).contains(&month) {
        return Err("month out of range");
    }
    if !(1..=31).contains(&day) {
        return Err("day out of range");
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
    // RFC3339 allows leap second 60 inside :60.
    if second > 60 {
        return Err("second out of range");
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
}
