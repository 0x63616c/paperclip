//! The e-ink facts that matter (WWW-46): frames presented, damage area per
//! present, which waveform was chosen, how often a partial escalated to a
//! full refresh, and how long a present took.
//!
//! A full refresh in particular is worth counting on its own: it is the
//! visibly slow, flashing path, and a session that took far more of them than
//! its damage rectangles would suggest is a session where something is
//! escalating refreshes it should not be (`platform/device`'s `waveform`
//! module decides that escalation; this only counts how often it happened).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// One presented frame, as reported to [`EinkCounters::record_present`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresentedFrame<'a> {
    /// Pixels touched by the swap rectangle (width × height).
    pub damage_area: u64,
    /// The waveform's name, e.g. `"INK"` or `"UI"` — a string rather than
    /// `platform/device`'s `Waveform` type, so this crate never has to depend
    /// on it.
    pub waveform: &'a str,
    /// Whether the vendor engine escalated this swap to a full refresh.
    pub full_refresh: bool,
    /// How long the swap itself took.
    pub latency: Duration,
}

/// A point-in-time read of every counter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EinkSnapshot {
    /// Total presents recorded.
    pub frames_presented: u64,
    /// Sum of every recorded damage area.
    pub damage_area_total: u64,
    /// How many of those presents were a full refresh.
    pub full_refresh_count: u64,
    /// The most recently recorded present's latency.
    pub last_present_latency: Duration,
    /// How many times each waveform was chosen.
    pub waveform_counts: Vec<(String, u64)>,
}

/// Process-wide counters for presented frames.
///
/// A single set of atomics plus one small mutex for the waveform tally —
/// there is one panel per process (§9), so there is exactly one of these to
/// keep.
#[derive(Debug, Default)]
pub struct EinkCounters {
    frames_presented: AtomicU64,
    damage_area_total: AtomicU64,
    full_refresh_count: AtomicU64,
    last_present_latency_micros: AtomicU64,
    waveform_counts: Mutex<HashMap<String, u64>>,
}

impl EinkCounters {
    /// Every counter at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one presented frame and emits a matching trace event, so a
    /// `journalctl` filter on `paper_telemetry::eink` sees the same facts a
    /// [`EinkCounters::snapshot`] would report in aggregate.
    pub fn record_present(&self, frame: PresentedFrame<'_>) {
        self.frames_presented.fetch_add(1, Ordering::Relaxed);
        self.damage_area_total
            .fetch_add(frame.damage_area, Ordering::Relaxed);
        if frame.full_refresh {
            self.full_refresh_count.fetch_add(1, Ordering::Relaxed);
        }
        self.last_present_latency_micros.store(
            u64::try_from(frame.latency.as_micros()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        // Recovers from a poisoned lock rather than panicking: an unrelated
        // panic elsewhere while this mutex happened to be held must not turn
        // a best-effort counter into a reason the device stops presenting.
        *self
            .waveform_counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(frame.waveform.to_owned())
            .or_insert(0) += 1;

        tracing::debug!(
            target: "paper_telemetry::eink",
            damage_area = frame.damage_area,
            waveform = frame.waveform,
            full_refresh = frame.full_refresh,
            latency_us = frame.latency.as_micros() as u64,
            "frame presented"
        );
    }

    /// A point-in-time read of every counter.
    pub fn snapshot(&self) -> EinkSnapshot {
        EinkSnapshot {
            frames_presented: self.frames_presented.load(Ordering::Relaxed),
            damage_area_total: self.damage_area_total.load(Ordering::Relaxed),
            full_refresh_count: self.full_refresh_count.load(Ordering::Relaxed),
            last_present_latency: Duration::from_micros(
                self.last_present_latency_micros.load(Ordering::Relaxed),
            ),
            waveform_counts: self
                .waveform_counts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .map(|(name, count)| (name.clone(), *count))
                .collect(),
        }
    }
}

/// The process-wide instance every present goes through.
pub static EINK: std::sync::LazyLock<EinkCounters> = std::sync::LazyLock::new(EinkCounters::new);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_a_present_updates_every_counter() {
        let counters = EinkCounters::new();
        counters.record_present(PresentedFrame {
            damage_area: 400,
            waveform: "INK",
            full_refresh: false,
            latency: Duration::from_millis(12),
        });
        counters.record_present(PresentedFrame {
            damage_area: 100,
            waveform: "UI",
            full_refresh: true,
            latency: Duration::from_millis(80),
        });

        let snapshot = counters.snapshot();
        assert_eq!(snapshot.frames_presented, 2);
        assert_eq!(snapshot.damage_area_total, 500);
        assert_eq!(snapshot.full_refresh_count, 1);
        assert_eq!(snapshot.last_present_latency, Duration::from_millis(80));
        let mut waveforms = snapshot.waveform_counts;
        waveforms.sort();
        assert_eq!(waveforms, vec![("INK".to_owned(), 1), ("UI".to_owned(), 1)]);
    }
}
