//! Normalized pointer input.
//!
//! One event type covers finger, pen, eraser and the desktop preview's mouse.
//! Every value on it is already in a space an app can use: position in canvas
//! pixels, pressure as a fraction, tilt in degrees. Nothing here carries a
//! digitizer count, a kernel slot or an `ABS_` code — those belong to
//! `paper_device`, which is the only crate allowed to know them (§9).
//!
//! ## Absent means absent
//!
//! Optional fields are `None` when the hardware did not report them, and there
//! is no other reason for a `None`. A finger reports no pressure, so touch
//! events carry `pressure: None` — not `Some(1.0)`, which would be a fact
//! nobody measured. This is the rule the whole input model is built on: a
//! plausible default is indistinguishable from a reading, and an app that
//! wants a fallback can write one itself where the choice is visible.
//!
//! Hover *distance* is the same rule applied to a field that is therefore
//! missing entirely. The pen reports `ABS_DISTANCE`, but the range those
//! counts span is not established (WWW-1 recorded the axis, not its scale), so
//! there is no honest way to normalize it. [`PointerPhase::Hover`] carries the
//! part that is known — the pen is in range and not touching — and the
//! distance arrives, as a new optional field, when something measures it.
//!
//! ## Facts this model is shaped by
//!
//! Established on the device by WWW-1 and WWW-20, not re-derived here:
//! multitouch protocol B with ten slots and a tracking id per contact, pen
//! pressure over 0–4096, tilt in hundredths of a degree over ±9000, and the
//! eraser as a tool-type switch on the one pen device rather than a second
//! device.

use crate::geometry::Point;

/// The pen's full-scale pressure reading.
///
/// The device reports `ABS_PRESSURE` over `0..=4096` (WWW-1). Apps never see
/// this: it is the divisor [`Pressure::from_raw`] uses, and it is public so
/// the device adapter does not have to repeat the number.
pub const PEN_PRESSURE_FULL_SCALE: i32 = 4096;

/// The pen's full-scale tilt reading, in hundredths of a degree.
///
/// `ABS_TILT_X` and `ABS_TILT_Y` span `-9000..=9000` (WWW-1), which is ±90°.
pub const PEN_TILT_FULL_SCALE: i32 = 9000;

/// Which implement produced an event.
///
/// The eraser is its own variant rather than a flag because that is how apps
/// use it — `Pointer::Eraser` is the question a drawing app asks. It is *not*
/// a separate device: the tablet switches `BTN_TOOL_PEN` for
/// `BTN_TOOL_RUBBER` on the one pen node, so an eraser stroke is the same
/// contact continuing. [`Self::is_stylus`] is the predicate for "came from the
/// pen", and it is true for both.
///
/// `#[non_exhaustive]`: an input device this build has never heard of is
/// safely ignorable by an app, so adding one is an additive change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Pointer {
    /// A finger on the touchscreen.
    Touch,
    /// The stylus, nib down.
    Pen,
    /// The stylus, reversed.
    Eraser,
    /// A mouse, which only exists in the desktop preview.
    Mouse,
}

impl Pointer {
    /// Whether this came from the stylus, either way up.
    pub const fn is_stylus(self) -> bool {
        matches!(self, Self::Pen | Self::Eraser)
    }

    /// Whether this contact is one of potentially several at once.
    ///
    /// Only the touchscreen reports concurrent contacts — ten slots of them.
    /// The pen is one contact and the preview's mouse is one contact, so an
    /// app that only wants to track a single gesture can ignore
    /// [`PointerEvent::contact`] entirely for those.
    pub const fn is_multitouch(self) -> bool {
        matches!(self, Self::Touch)
    }
}

/// Where in a contact's life an event sits.
///
/// **Deliberately not `#[non_exhaustive]`.** Every other type in this module
/// is, because an unknown field or an unknown device can be ignored without
/// consequence. A phase cannot: it is a state transition, and an app that
/// silently drops one it does not recognise is an app holding a contact that
/// never ends. So a new phase is a compile error at every app, and the minor
/// protocol bump that carries it is what tells the App Store those apps need
/// rebuilding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PointerPhase {
    /// In range but not touching. Pen only, and only while it is close enough
    /// for the digitizer to see it.
    ///
    /// An app may ignore hover completely; nothing depends on handling it. A
    /// hover is never part of a press, so a `Hover` is never followed by an
    /// [`Up`](Self::Up) for the same movement — the pen either lands, giving a
    /// [`Down`](Self::Down), or leaves range, giving a
    /// [`Cancelled`](Self::Cancelled).
    Hover,
    /// Contact began.
    Down,
    /// Contact moved.
    Moved,
    /// Contact ended normally.
    Up,
    /// Contact was taken away — palm rejected, app suspended, window lost
    /// focus, kernel event buffer overflowed. Never treat this as a tap.
    Cancelled,
}

impl PointerPhase {
    /// Whether this phase means the implement is touching the glass.
    pub const fn is_contact(self) -> bool {
        matches!(self, Self::Down | Self::Moved)
    }

    /// Whether this phase ends a contact, however it ended.
    ///
    /// The phase to drop per-contact state on. Matching only
    /// [`Up`](Self::Up) here is the bug that leaves a cancelled contact stuck.
    pub const fn ends_contact(self) -> bool {
        matches!(self, Self::Up | Self::Cancelled)
    }
}

/// Which contact an event belongs to.
///
/// Allocated by the host, monotonically, and **never reused within a session**.
/// The kernel's own tracking ids are reused as soon as a finger lifts, which
/// makes "same id, therefore same finger" wrong exactly when two fingers swap
/// in the same frame. Renumbering at the host means an app can use this as a
/// map key and delete the entry on [`PointerPhase::ends_contact`] without ever
/// being handed a stale one.
///
/// Ids are unique per session and mean nothing across sessions.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct ContactId(u64);

impl ContactId {
    /// The first id a session hands out.
    pub const FIRST: ContactId = ContactId(0);

    /// Wraps a raw value. For the host's allocator and for tests.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw value, for logging and for use as a map key.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next id after this one.
    ///
    /// Saturating rather than wrapping: at one contact per microsecond a
    /// `u64` lasts about six hundred thousand years, so reaching the end means
    /// something is wrong, and repeating the last id forever is a visible bug
    /// where wrapping to zero would be a silently aliased contact.
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Pen pressure as a fraction of full scale, `0.0..=1.0`.
///
/// Normalized because `4096` is a fact about this digitizer and an app that
/// hard-codes it is an app that breaks on the next one. The raw count is not
/// carried: it is the numerator of a division the app does not need to see,
/// and nothing in v1 wants counts.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Pressure(f32);

impl Pressure {
    /// Builds a pressure from a raw reading and the axis's full scale.
    ///
    /// `None` when the reading is outside `0..=full_scale`, or when
    /// `full_scale` is not positive. A reading the axis says is impossible is
    /// not evidence of anything, and clamping it would turn a broken digitizer
    /// into a confident `1.0`.
    pub fn from_raw(raw: i32, full_scale: i32) -> Option<Self> {
        if full_scale <= 0 || raw < 0 || raw > full_scale {
            return None;
        }
        Some(Self(raw as f32 / full_scale as f32))
    }

    /// The fraction, `0.0..=1.0`.
    pub const fn get(self) -> f32 {
        self.0
    }
}

/// Pen tilt in degrees from vertical, on each axis.
///
/// Positive `x` leans right, positive `y` leans towards the bottom of the
/// screen, matching the canvas axes rather than the digitizer's.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tilt {
    /// Lean along `x`, in degrees.
    pub x_degrees: f32,
    /// Lean along `y`, in degrees.
    pub y_degrees: f32,
}

impl Tilt {
    /// Builds a tilt from the kernel's hundredths of a degree.
    ///
    /// `None` when either axis is outside `±full_scale` — same rule as
    /// [`Pressure::from_raw`], and for the same reason.
    pub fn from_hundredths(x: i32, y: i32, full_scale: i32) -> Option<Self> {
        if full_scale <= 0 || x.abs() > full_scale || y.abs() > full_scale {
            return None;
        }
        Some(Self {
            x_degrees: x as f32 / 100.0,
            y_degrees: y as f32 / 100.0,
        })
    }
}

/// A pointer event already mapped into the app's viewport.
///
/// If you are holding one of these, the coordinates are in your viewport:
/// events that landed outside it were dropped before this type existed, and
/// the origin is your surface's top-left, not the panel's.
///
/// `#[non_exhaustive]`, and built through [`Self::new`] plus the `with_*`
/// builders, so an axis the device turns out to report is a field addition and
/// a protocol minor bump rather than a break at every call site.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct PointerEvent {
    /// Where the event landed, in viewport space.
    pub at: Point,
    /// What happened.
    pub phase: PointerPhase,
    /// What produced it.
    pub pointer: Pointer,
    /// Which contact this belongs to.
    pub contact: ContactId,
    /// How hard, if the device said. `None` for touch and mouse, which do not
    /// report pressure at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure: Option<Pressure>,
    /// How the pen was leaning, if the device said. `None` for touch and mouse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt: Option<Tilt>,
}

impl PointerEvent {
    /// Builds an event with no optional axes reported.
    pub const fn new(at: Point, phase: PointerPhase, pointer: Pointer, contact: ContactId) -> Self {
        Self {
            at,
            phase,
            pointer,
            contact,
            pressure: None,
            tilt: None,
        }
    }

    /// The same event carrying a pressure reading.
    pub const fn with_pressure(mut self, pressure: Option<Pressure>) -> Self {
        self.pressure = pressure;
        self
    }

    /// The same event carrying a tilt reading.
    pub const fn with_tilt(mut self, tilt: Option<Tilt>) -> Self {
        self.tilt = tilt;
        self
    }

    /// Whether this event completes a tap — the moment to act on a button.
    ///
    /// A cancelled contact is not a tap, and neither is a hover.
    pub const fn is_tap(self) -> bool {
        matches!(self.phase, PointerPhase::Up)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContactId, PEN_PRESSURE_FULL_SCALE, PEN_TILT_FULL_SCALE, Pointer, PointerEvent,
        PointerPhase, Pressure, Tilt,
    };
    use crate::geometry::Point;

    fn at() -> Point {
        Point::new(10.0, 10.0)
    }

    #[test]
    fn only_a_clean_release_counts_as_a_tap() {
        for phase in [
            PointerPhase::Hover,
            PointerPhase::Down,
            PointerPhase::Moved,
            PointerPhase::Cancelled,
        ] {
            assert!(!PointerEvent::new(at(), phase, Pointer::Touch, ContactId::FIRST).is_tap());
        }
        assert!(
            PointerEvent::new(at(), PointerPhase::Up, Pointer::Touch, ContactId::FIRST).is_tap()
        );
    }

    /// The phase an app drops per-contact state on has to include the one it
    /// did not ask for, or a rejected palm leaves a contact alive forever.
    #[test]
    fn cancellation_ends_a_contact_just_as_a_release_does() {
        assert!(PointerPhase::Up.ends_contact());
        assert!(PointerPhase::Cancelled.ends_contact());
        assert!(!PointerPhase::Down.ends_contact());
        assert!(!PointerPhase::Moved.ends_contact());
        assert!(!PointerPhase::Hover.ends_contact());
    }

    #[test]
    fn hover_is_not_contact() {
        assert!(!PointerPhase::Hover.is_contact());
        assert!(PointerPhase::Down.is_contact());
        assert!(PointerPhase::Moved.is_contact());
        assert!(!PointerPhase::Up.is_contact());
    }

    #[test]
    fn the_eraser_is_the_stylus() {
        assert!(Pointer::Pen.is_stylus());
        assert!(Pointer::Eraser.is_stylus());
        assert!(!Pointer::Touch.is_stylus());
        assert!(!Pointer::Mouse.is_stylus());
        assert!(Pointer::Touch.is_multitouch());
        assert!(!Pointer::Pen.is_multitouch());
    }

    #[test]
    fn pressure_normalises_over_the_devices_full_scale() {
        let full = Pressure::from_raw(PEN_PRESSURE_FULL_SCALE, PEN_PRESSURE_FULL_SCALE).unwrap();
        assert!((full.get() - 1.0).abs() < f32::EPSILON);
        let none = Pressure::from_raw(0, PEN_PRESSURE_FULL_SCALE).unwrap();
        assert_eq!(none.get(), 0.0);
        let half = Pressure::from_raw(2048, PEN_PRESSURE_FULL_SCALE).unwrap();
        assert!((half.get() - 0.5).abs() < 1e-6);
    }

    /// A reading the axis says cannot happen is absent, not clamped. Clamping
    /// would turn a misreported axis into a confident full-pressure stroke.
    #[test]
    fn an_impossible_reading_is_absent_rather_than_clamped() {
        assert_eq!(Pressure::from_raw(-1, PEN_PRESSURE_FULL_SCALE), None);
        assert_eq!(
            Pressure::from_raw(PEN_PRESSURE_FULL_SCALE + 1, PEN_PRESSURE_FULL_SCALE),
            None
        );
        assert_eq!(Pressure::from_raw(10, 0), None);
        assert_eq!(Tilt::from_hundredths(9001, 0, PEN_TILT_FULL_SCALE), None);
        assert_eq!(Tilt::from_hundredths(0, -9001, PEN_TILT_FULL_SCALE), None);
    }

    #[test]
    fn tilt_is_degrees_not_hundredths() {
        let tilt = Tilt::from_hundredths(-4500, 9000, PEN_TILT_FULL_SCALE).unwrap();
        assert!((tilt.x_degrees + 45.0).abs() < 1e-6);
        assert!((tilt.y_degrees - 90.0).abs() < 1e-6);
    }

    /// Touch reports no pressure, so a touch event must not have one. The rule
    /// is enforced by the constructor defaulting to `None`, not by a check.
    #[test]
    fn a_touch_event_reports_no_pressure_by_default() {
        let event = PointerEvent::new(at(), PointerPhase::Down, Pointer::Touch, ContactId::FIRST);
        assert_eq!(event.pressure, None);
        assert_eq!(event.tilt, None);
    }

    #[test]
    fn contact_ids_do_not_wrap_around_to_an_existing_contact() {
        assert_eq!(ContactId::FIRST.next().get(), 1);
        assert_eq!(ContactId::new(u64::MAX).next().get(), u64::MAX);
    }

    /// Absent axes are absent on the wire too — a `None` pressure is a missing
    /// key, not `null` and certainly not `0.0`.
    #[test]
    fn absent_axes_do_not_appear_on_the_wire() {
        let touch = PointerEvent::new(at(), PointerPhase::Down, Pointer::Touch, ContactId::FIRST);
        let json = serde_json::to_string(&touch).unwrap();
        assert!(!json.contains("pressure"), "{json}");
        assert!(!json.contains("tilt"), "{json}");
        assert_eq!(serde_json::from_str::<PointerEvent>(&json).unwrap(), touch);

        let pen = PointerEvent::new(at(), PointerPhase::Moved, Pointer::Pen, ContactId::new(7))
            .with_pressure(Pressure::from_raw(1024, PEN_PRESSURE_FULL_SCALE))
            .with_tilt(Tilt::from_hundredths(100, -200, PEN_TILT_FULL_SCALE));
        let json = serde_json::to_string(&pen).unwrap();
        assert!(json.contains("pressure"), "{json}");
        assert_eq!(serde_json::from_str::<PointerEvent>(&json).unwrap(), pen);
    }
}
