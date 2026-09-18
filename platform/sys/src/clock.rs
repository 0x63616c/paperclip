//! Time, behind a seam.
//!
//! Moved from `platform/updater`'s `health` module (WWW-46), where it was
//! proved out: a [`Clock`] is what let 34 tests grade a candidate against a
//! 30-second deadline without a single one of them taking 30 seconds.

use std::fmt;
use std::time::{Duration, Instant};

/// The clock a caller waits against.
///
/// A trait so a test can advance time by calling [`Clock::sleep`] instead of
/// waiting on the wall clock. The device and the VM both use [`SystemClock`].
pub trait Clock: fmt::Debug {
    /// Now.
    fn now(&self) -> Instant;
    /// Waits.
    fn sleep(&self, duration: Duration);
}

/// The real one: `Instant::now()` and `std::thread::sleep`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_clock_advances_while_it_sleeps() {
        let clock = SystemClock;
        let before = clock.now();
        clock.sleep(Duration::from_millis(5));
        assert!(clock.now() >= before + Duration::from_millis(5));
    }
}
