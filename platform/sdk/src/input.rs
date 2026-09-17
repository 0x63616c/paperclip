//! Pointer input in canvas space.
//!
//! Stage 1 plumbs a mouse. That is enough to prove coordinates arrive in the
//! right place and that a tile the size of a fingertip is hit when it is
//! pressed. It proves nothing about the pen: pressure, tilt, palm rejection
//! and latency are device facts, and this type carries no field pretending
//! otherwise until WWW-1 says what the device actually reports.

use crate::geometry::Point;

/// Which input device produced an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Pointer {
    /// A finger on the touchscreen.
    Touch,
    /// The stylus.
    Pen,
    /// A mouse, which only exists in the desktop preview.
    Mouse,
}

/// Where in a press-drag-release sequence an event sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointerPhase {
    /// Contact began.
    Down,
    /// Contact moved.
    Moved,
    /// Contact ended normally.
    Up,
    /// Contact was taken away — window lost focus, palm rejected, app switched.
    /// Never treat this as a tap.
    Cancelled,
}

/// A pointer event already mapped into canvas space.
///
/// If you are holding one of these, the coordinates are on the canvas: events
/// that landed in the letterbox were dropped by
/// [`DisplayMapping`](crate::DisplayMapping) before this type existed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerEvent {
    /// Where the event landed, in canvas space.
    pub at: Point,
    /// What happened.
    pub phase: PointerPhase,
    /// What produced it.
    pub pointer: Pointer,
}

impl PointerEvent {
    /// Builds an event.
    pub const fn new(at: Point, phase: PointerPhase, pointer: Pointer) -> Self {
        Self { at, phase, pointer }
    }

    /// Whether this event completes a tap — the moment to act on a button.
    pub const fn is_tap(self) -> bool {
        matches!(self.phase, PointerPhase::Up)
    }
}

#[cfg(test)]
mod tests {
    use super::{Pointer, PointerEvent, PointerPhase};
    use crate::geometry::Point;

    #[test]
    fn only_a_clean_release_counts_as_a_tap() {
        let at = Point::new(10.0, 10.0);
        assert!(PointerEvent::new(at, PointerPhase::Up, Pointer::Touch).is_tap());
        assert!(!PointerEvent::new(at, PointerPhase::Down, Pointer::Touch).is_tap());
        assert!(!PointerEvent::new(at, PointerPhase::Moved, Pointer::Pen).is_tap());
        assert!(!PointerEvent::new(at, PointerPhase::Cancelled, Pointer::Pen).is_tap());
    }
}
