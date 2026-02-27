//! Hybrid Logical Clock (HLC) for causal ordering.
//!
//! An HLC timestamp combines a wall-clock millisecond time with a 16-bit
//! logical counter to guarantee strict monotonicity and causal ordering even
//! when the physical clock goes backwards (e.g. NTP correction, clock skew).
//!
//! ## Encoding
//!
//! An [`HlcTimestamp`] is a 64-bit unsigned integer:
//! - High 48 bits: milliseconds since Unix epoch (wall clock)
//! - Low 16 bits: logical counter (ensures uniqueness within the same ms)
//!
//! ## Usage
//!
//! ```rust,ignore
//! let hlc = Hlc::new();
//! let ts = hlc.now();       // monotonically increasing
//! ```

use chrono::Utc;
use std::sync::Mutex;

/// A packed HLC timestamp (`wall_ms << 16 | logical`).
pub type HlcTimestamp = u64;

/// Extract the wall-clock milliseconds component.
///
/// The upper 48 bits of the packed timestamp encode milliseconds since the
/// Unix epoch.  The result is a raw `u64`; callers that need to verify the
/// value represent a plausible timestamp should compare it against a
/// reasonable range (e.g. 2020-01-01 .. 2100-01-01).
pub fn hlc_wall_ms(ts: HlcTimestamp) -> u64 {
    ts >> 16
}

/// Extract the logical counter component.
pub fn hlc_logical(ts: HlcTimestamp) -> u16 {
    ts as u16
}

fn pack(wall_ms: u64, logical: u16) -> HlcTimestamp {
    (wall_ms << 16) | (logical as u64)
}

/// Thread-safe Hybrid Logical Clock.
///
/// Call [`Hlc::now`] to obtain a monotonically-increasing timestamp suitable
/// for causal ordering across distributed nodes.
pub struct Hlc {
    last: Mutex<HlcTimestamp>,
    /// Optional fixed wall clock for deterministic testing.
    #[cfg(test)]
    mock_wall_ms: Option<u64>,
}

impl Hlc {
    /// Create a new HLC initialised to zero.
    pub const fn new() -> Self {
        Self {
            last: Mutex::new(0),
            #[cfg(test)]
            mock_wall_ms: None,
        }
    }

    /// Create a new HLC with a fixed wall clock for testing.
    #[cfg(test)]
    pub fn with_mock_wall(ms: u64) -> Self {
        Self {
            last: Mutex::new(0),
            mock_wall_ms: Some(ms),
        }
    }

    /// Advance the clock and return a new HLC timestamp.
    ///
    /// Guarantees:
    /// 1. The returned timestamp is strictly greater than all previously
    ///    returned timestamps from this instance.
    /// 2. The wall-clock component is always ≥ the current physical time.
    pub fn now(&self) -> HlcTimestamp {
        #[cfg(not(test))]
        let wall_ms = Utc::now().timestamp_millis() as u64;
        #[cfg(test)]
        let wall_ms = self
            .mock_wall_ms
            .unwrap_or_else(|| Utc::now().timestamp_millis() as u64);

        let mut last = self.last.lock().unwrap();
        let last_wall = hlc_wall_ms(*last);
        let new_ts = if wall_ms > last_wall {
            pack(wall_ms, 0)
        } else {
            // Wall clock did not advance – increment logical counter.
            let next_logical = hlc_logical(*last).saturating_add(1);
            pack(last_wall, next_logical)
        };
        *last = new_ts;
        new_ts
    }

    /// Update the clock given a remote HLC timestamp (receive event).
    ///
    /// Ensures the local clock is at least as large as `remote`, then
    /// increments to produce a new timestamp strictly greater than both.
    pub fn update_with_remote(&self, remote: HlcTimestamp) -> HlcTimestamp {
        #[cfg(not(test))]
        let wall_ms = Utc::now().timestamp_millis() as u64;
        #[cfg(test)]
        let wall_ms = self
            .mock_wall_ms
            .unwrap_or_else(|| Utc::now().timestamp_millis() as u64);

        let mut last = self.last.lock().unwrap();
        let last_wall = hlc_wall_ms(*last);
        let remote_wall = hlc_wall_ms(remote);
        let max_wall = wall_ms.max(last_wall).max(remote_wall);
        let logical = if max_wall == last_wall && max_wall == remote_wall {
            hlc_logical(*last)
                .max(hlc_logical(remote))
                .saturating_add(1)
        } else if max_wall == last_wall {
            hlc_logical(*last).saturating_add(1)
        } else if max_wall == remote_wall {
            hlc_logical(remote).saturating_add(1)
        } else {
            0
        };
        let new_ts = pack(max_wall, logical);
        *last = new_ts;
        new_ts
    }
}

impl Default for Hlc {
    fn default() -> Self {
        Self::new()
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hlc_is_monotonically_increasing() {
        let clock = Hlc::new();
        let mut prev = 0u64;
        for _ in 0..100 {
            let ts = clock.now();
            assert!(ts > prev, "timestamp {ts} should be > {prev}");
            prev = ts;
        }
    }

    #[test]
    fn hlc_logical_increments_when_wall_is_same() {
        // Use a fixed wall clock to ensure deterministic behavior.
        // Without this, the wall clock might advance between calls on slow runners.
        let wall = 1_700_000_000_000;
        let clock = Hlc::with_mock_wall(wall);

        let ts1 = clock.now();

        // Even with update_with_remote using the same wall time,
        // strict monotonicity demands the logical counter increments.
        let same_wall_remote = pack(wall, 0);
        let ts2 = clock.update_with_remote(same_wall_remote);

        assert!(ts2 > ts1);
        assert_eq!(hlc_wall_ms(ts2), wall);
        assert!(hlc_logical(ts2) > hlc_logical(ts1));
    }

    #[test]
    fn hlc_update_with_future_remote() {
        let clock = Hlc::new();
        let _ = clock.now();
        // Remote is 10 seconds in the future
        let future_wall = Utc::now().timestamp_millis() as u64 + 10_000;
        let future_ts = pack(future_wall, 0);
        let ts = clock.update_with_remote(future_ts);
        assert!(ts > future_ts, "local should exceed remote after update");
        assert_eq!(hlc_wall_ms(ts), future_wall);
        assert_eq!(hlc_logical(ts), 1);
    }

    #[test]
    fn pack_unpack_roundtrip() {
        let wall = 1_700_000_000_000u64;
        let logical = 42u16;
        let ts = pack(wall, logical);
        assert_eq!(hlc_wall_ms(ts), wall);
        assert_eq!(hlc_logical(ts), logical);
    }
}
