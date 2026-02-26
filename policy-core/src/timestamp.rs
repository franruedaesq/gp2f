//! Unix timestamp normalization utilities.
//!
//! The GP2F policy engine represents all datetime values as **Unix timestamps
//! (`i64` seconds since the epoch)**.  This module provides helpers to enforce
//! that invariant at the boundary between external data and the evaluator,
//! regardless of what format the caller uses.
//!
//! ## Rationale
//!
//! Using a single canonical representation (Unix `i64`) guarantees:
//! - **No timezone drift**: timezone-aware structs are normalised to UTC.
//! - **No platform divergence**: no locale-dependent formatting.
//! - **Determinism**: the same logical instant always maps to the same `i64`.
//!
//! The `proto/gp2f.proto` contract already specifies `int64` for timestamp
//! fields; this module enforces the same convention inside the Rust evaluator.

// In no_std + alloc builds, bring heap types into scope.
#[cfg(not(feature = "std"))]
extern crate alloc;

/// Normalise an ISO-8601 / RFC-3339 datetime string to a Unix timestamp
/// (seconds since the epoch, UTC).
///
/// Accepts:
/// - Plain Unix timestamp strings (`"1704067200"`)
/// - RFC-3339 / ISO-8601 strings with a timezone offset or `Z`
///   (`"2024-01-01T00:00:00Z"`)
///
/// Returns `None` if the input cannot be parsed.
///
/// # Platform determinism
///
/// This function must return the same value on every platform and OS.  It
/// never reads the system clock and ignores the local timezone.
pub fn normalize_timestamp(input: &str) -> Option<i64> {
    let s = input.trim();

    // Fast path: plain integer string.
    if let Ok(ts) = s.parse::<i64>() {
        return Some(ts);
    }

    // Slow path: ISO-8601 / RFC-3339 string.
    // Only available when `chrono` is enabled (requires std).
    #[cfg(feature = "std")]
    {
        use chrono::{DateTime, FixedOffset};
        if let Ok(dt) = DateTime::<FixedOffset>::parse_from_rfc3339(s) {
            return Some(dt.timestamp());
        }
    }

    None
}

/// Normalise a raw `i64` value that may already be in milliseconds (Unix ms)
/// or seconds (Unix s) to **seconds**.
///
/// Heuristic: values larger than `2^31` (≈ year 2038 in seconds) are assumed
/// to be milliseconds and are divided by 1 000.  This is consistent with
/// common JavaScript `Date.now()` usage.
pub fn coerce_timestamp_to_seconds(raw: i64) -> i64 {
    // Values above ~2^31 are treated as milliseconds.
    if raw.unsigned_abs() > 2_147_483_647 {
        raw / 1_000
    } else {
        raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_integer_passthrough() {
        assert_eq!(normalize_timestamp("1704067200"), Some(1_704_067_200));
    }

    #[test]
    fn negative_integer() {
        assert_eq!(normalize_timestamp("-86400"), Some(-86_400));
    }

    #[test]
    fn invalid_returns_none() {
        assert_eq!(normalize_timestamp("not-a-date"), None);
    }

    #[test]
    fn empty_returns_none() {
        assert_eq!(normalize_timestamp(""), None);
    }

    #[cfg(feature = "std")]
    #[test]
    fn rfc3339_utc() {
        // 2024-01-01 00:00:00 UTC = 1704067200
        assert_eq!(
            normalize_timestamp("2024-01-01T00:00:00Z"),
            Some(1_704_067_200)
        );
    }

    #[cfg(feature = "std")]
    #[test]
    fn rfc3339_with_offset_normalises_to_utc() {
        // 2024-01-01 01:00:00+01:00 == 2024-01-01 00:00:00 UTC
        assert_eq!(
            normalize_timestamp("2024-01-01T01:00:00+01:00"),
            Some(1_704_067_200)
        );
    }

    #[test]
    fn coerce_seconds_unchanged() {
        assert_eq!(coerce_timestamp_to_seconds(1_704_067_200), 1_704_067_200);
    }

    #[test]
    fn coerce_millis_to_seconds() {
        assert_eq!(
            coerce_timestamp_to_seconds(1_704_067_200_000),
            1_704_067_200
        );
    }
}
