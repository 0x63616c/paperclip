//! Wall-clock time, behind a seam.
//!
//! Deliberately separate from [`Clock`](crate::Clock): that trait exists for
//! measuring durations against a monotonic instant with no fixed origin, and
//! reusing it for "what time is it" would mean adding a wall-clock method to
//! every caller that only ever wanted to measure an interval.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// Reading the wall clock, behind a seam so a test can fix it.
pub trait WallClock: fmt::Debug {
    /// Milliseconds since the Unix epoch.
    fn now_unix_millis(&self) -> u64;
}

/// The real one: `SystemTime::now()`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now_unix_millis(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            // A clock reading before the epoch is not a duration this device
            // can be running with, so a saturated zero is the honest answer
            // rather than a panic over a clock that is merely unset.
            .map_or(0, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_wall_clock_reads_a_plausible_unix_time() {
        let millis = SystemWallClock.now_unix_millis();
        // 2024-01-01T00:00:00Z, as a sanity floor rather than an exact value.
        assert!(millis > 1_704_067_200_000);
    }
}
