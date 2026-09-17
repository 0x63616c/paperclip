//! Decoding `/dev/input/event*` without Qt in the way.
//!
//! Both nodes are readable concurrently and neither is `EVIOCGRAB`bed by the
//! vendor stack (WWW-1), so reading evdev directly in Rust costs nothing and
//! removes an entire toolkit from the input path. Nothing here opens a file:
//! this module turns bytes into events, which is the part that can be tested
//! on a Mac against captured traces.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use paper_protocol::{PEN_PRESSURE_FULL_SCALE, PEN_TILT_FULL_SCALE};
use paper_sdk::{ContactId, Pointer, PointerEvent, PointerPhase, Pressure, Tilt};

use super::transform::PointerTransform;

/// `struct input_event` on 64-bit Linux: two 64-bit `timeval` fields, then
/// `type`, `code`, `value`.
pub const EVENT_BYTES: usize = 24;

/// How many simultaneous contacts the touchscreen reports (WWW-1).
pub const MAX_TOUCH_SLOTS: usize = 10;

// Only the codes this decoder acts on. Named rather than inlined so a wrong
// one is a wrong constant rather than a wrong magic number.
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;

const SYN_REPORT: u16 = 0x00;
const SYN_DROPPED: u16 = 0x03;

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_PRESSURE: u16 = 0x18;
const ABS_DISTANCE: u16 = 0x19;
const ABS_TILT_X: u16 = 0x1A;
const ABS_TILT_Y: u16 = 0x1B;
const ABS_MT_SLOT: u16 = 0x2F;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;

const BTN_TOUCH: u16 = 0x14A;
const BTN_TOOL_PEN: u16 = 0x140;
const BTN_TOOL_RUBBER: u16 = 0x141;

/// One raw kernel input event, timestamp discarded.
///
/// The timestamp is dropped on purpose. It is the kernel's clock, a latency
/// measurement wants the moment the *host* saw the event, and carrying a field
/// nothing reads invites someone to trust it for something it cannot answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawEvent {
    /// `type` in the kernel's naming — `EV_ABS`, `EV_KEY`, `EV_SYN`.
    pub kind: u16,
    /// The axis or button.
    pub code: u16,
    /// The value, signed: tracking ids and tilts are negative in normal use.
    pub value: i32,
}

impl RawEvent {
    /// Decodes one event from the 24 bytes the kernel writes.
    pub fn from_bytes(bytes: &[u8; EVENT_BYTES]) -> Self {
        // Little-endian: the tablet is aarch64 and so is every host that will
        // ever replay a captured trace through this decoder.
        Self {
            kind: u16::from_le_bytes([bytes[16], bytes[17]]),
            code: u16::from_le_bytes([bytes[18], bytes[19]]),
            value: i32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
        }
    }

    /// Decodes as many whole events as `bytes` contains, ignoring a partial
    /// tail.
    ///
    /// A `read` on an event node returns whole events, so a remainder means
    /// the caller has mis-framed the stream; the remainder is returned rather
    /// than dropped so a reader can carry it into the next read.
    pub fn decode_all(bytes: &[u8]) -> (Vec<Self>, usize) {
        let whole = bytes.len() / EVENT_BYTES;
        let mut events = Vec::with_capacity(whole);
        for chunk in bytes.chunks_exact(EVENT_BYTES) {
            let frame: &[u8; EVENT_BYTES] = chunk.try_into().expect("chunks_exact yields 24");
            events.push(Self::from_bytes(frame));
        }
        (events, bytes.len() % EVENT_BYTES)
    }
}

/// Hands out [`ContactId`]s that are never reused.
///
/// One allocator serves both decoders, because a contact id is unique across a
/// whole session and the pen and the touchscreen are two nodes of one session.
/// Giving each decoder its own counter would have a finger and the pen share
/// an id, which is exactly the collision the ids exist to prevent.
///
/// Cheap to clone and safe to share: the two nodes are read independently, and
/// a decoder that needed a lock for this would be a decoder holding one on the
/// input path.
#[derive(Debug, Clone)]
pub struct ContactIds {
    next: Arc<AtomicU64>,
}

impl Default for ContactIds {
    fn default() -> Self {
        Self::new()
    }
}

impl ContactIds {
    /// A fresh allocator, starting at [`ContactId::FIRST`].
    pub fn new() -> Self {
        Self {
            next: Arc::new(AtomicU64::new(ContactId::FIRST.get())),
        }
    }

    /// The next id, never one already handed out.
    pub fn allocate(&self) -> ContactId {
        ContactId::new(self.next.fetch_add(1, Ordering::Relaxed))
    }
}

/// Turns touchscreen reports into contact events.
///
/// Multitouch protocol B: the kernel names a slot, then describes it, and a
/// `SYN_REPORT` ends the frame. Everything is deferred to the `SYN_REPORT`
/// because a half-described frame is not a position anyone should act on.
#[derive(Debug)]
pub struct TouchDecoder {
    transform: PointerTransform,
    ids: ContactIds,
    slots: [Slot; MAX_TOUCH_SLOTS],
    current: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    tracking: Option<i32>,
    /// The id this contact was given when it began.
    ///
    /// Not the kernel's tracking id: that one is reused the moment a finger
    /// lifts, so two fingers swapping within a frame would look like one
    /// finger teleporting.
    contact: Option<ContactId>,
    x: i32,
    y: i32,
    down: bool,
    dirty: bool,
    lifted: bool,
}

impl TouchDecoder {
    /// A decoder using `transform` to reach panel coordinates, drawing contact
    /// ids from `ids`.
    pub fn new(transform: PointerTransform, ids: ContactIds) -> Self {
        Self {
            transform,
            ids,
            slots: [Slot::default(); MAX_TOUCH_SLOTS],
            current: 0,
        }
    }

    /// Feeds one raw event, returning whatever frame it completed.
    pub fn feed(&mut self, event: RawEvent) -> Vec<PointerEvent> {
        match (event.kind, event.code) {
            (EV_SYN, SYN_REPORT) => return self.flush(),
            // The kernel's input buffer overflowed and events were lost. Every
            // contact's state is now a guess, so end all of them rather than
            // let a stuck finger drag something across the screen.
            (EV_SYN, SYN_DROPPED) => return self.resync(),
            (EV_ABS, ABS_MT_SLOT) => {
                self.current = usize::try_from(event.value)
                    .ok()
                    .filter(|slot| *slot < MAX_TOUCH_SLOTS)
                    // A slot the kernel should never report. Park it on the
                    // last slot rather than index out of bounds; the frame is
                    // wrong either way and dropping it silently is worse.
                    .unwrap_or(MAX_TOUCH_SLOTS - 1);
            }
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                let slot = &mut self.slots[self.current];
                if event.value < 0 {
                    if slot.down {
                        slot.lifted = true;
                        slot.dirty = true;
                    }
                    slot.tracking = None;
                } else {
                    if slot.tracking.is_none() {
                        slot.contact = Some(self.ids.allocate());
                    }
                    slot.tracking = Some(event.value);
                    slot.dirty = true;
                }
            }
            (EV_ABS, ABS_MT_POSITION_X) => {
                let slot = &mut self.slots[self.current];
                slot.x = event.value;
                slot.dirty = true;
            }
            (EV_ABS, ABS_MT_POSITION_Y) => {
                let slot = &mut self.slots[self.current];
                slot.y = event.value;
                slot.dirty = true;
            }
            _ => {}
        }
        Vec::new()
    }

    fn flush(&mut self) -> Vec<PointerEvent> {
        let mut out = Vec::new();
        for index in 0..MAX_TOUCH_SLOTS {
            let slot = self.slots[index];
            if !slot.dirty {
                continue;
            }
            let at = self.transform.map(slot.x, slot.y);
            let phase = if slot.lifted {
                PointerPhase::Up
            } else if slot.down {
                PointerPhase::Moved
            } else if slot.tracking.is_some() {
                PointerPhase::Down
            } else {
                // Dirtied, then released inside the same frame without ever
                // having been down. Nothing happened.
                self.slots[index] = Slot::default();
                continue;
            };

            // A slot that was dirtied without a tracking id is a kernel frame
            // we cannot attribute; give it an id rather than drop the contact.
            let contact = slot.contact.unwrap_or_else(|| self.ids.allocate());
            self.slots[index].contact = Some(contact);
            out.push(PointerEvent::new(at, phase, Pointer::Touch, contact));

            let slot = &mut self.slots[index];
            slot.dirty = false;
            if slot.lifted {
                *slot = Slot::default();
            } else {
                slot.down = true;
            }
        }
        out
    }

    fn resync(&mut self) -> Vec<PointerEvent> {
        let mut out = Vec::new();
        for index in 0..MAX_TOUCH_SLOTS {
            let slot = self.slots[index];
            if slot.down {
                let contact = slot.contact.unwrap_or_else(|| self.ids.allocate());
                out.push(PointerEvent::new(
                    self.transform.map(slot.x, slot.y),
                    PointerPhase::Cancelled,
                    Pointer::Touch,
                    contact,
                ));
            }
            self.slots[index] = Slot::default();
        }
        self.current = 0;
        out
    }
}

/// Turns pen reports into pointer events.
///
/// Hovering is reported now that there is a phase for it.
/// [`PointerPhase::Hover`] means the digitizer can see the pen and the pen is
/// not touching the glass — the state `BTN_TOOL_PEN` (or `BTN_TOOL_RUBBER`)
/// set with `BTN_TOUCH` clear. An earlier version of this decoder dropped
/// those frames because the SDK had no phase for them; WWW-5 added one.
///
/// What is still dropped is `ABS_DISTANCE`, the *number*. The axis exists and
/// the pen reports it, but the range those counts span was never established,
/// so there is no honest way to normalize it and an invented scale would be a
/// measurement nobody took. The phase carries what is known; a distance
/// arrives when something measures the axis.
#[derive(Debug)]
pub struct PenDecoder {
    transform: PointerTransform,
    ids: ContactIds,
    x: i32,
    y: i32,
    pressure: i32,
    tilt: (i32, i32),
    tool: Pointer,
    /// The digitizer can see the pen: a tool button is set.
    in_range: bool,
    touching: bool,
    down: bool,
    dirty: bool,
    /// The id the current hover-or-press run is using.
    ///
    /// One id spans the hover that precedes a stroke and the stroke itself, so
    /// an app previewing a nib position and then drawing with it sees one
    /// contact rather than two. Lifting starts a new id for the hover that
    /// follows, because the next press is a different contact.
    contact: Option<ContactId>,
}

impl PenDecoder {
    /// A decoder using `transform` to reach panel coordinates, drawing contact
    /// ids from `ids`.
    pub fn new(transform: PointerTransform, ids: ContactIds) -> Self {
        Self {
            transform,
            ids,
            x: 0,
            y: 0,
            pressure: 0,
            tilt: (0, 0),
            tool: Pointer::Pen,
            in_range: false,
            touching: false,
            down: false,
            dirty: false,
            contact: None,
        }
    }

    /// Feeds one raw event, returning whatever frame it completed.
    pub fn feed(&mut self, event: RawEvent) -> Vec<PointerEvent> {
        match (event.kind, event.code) {
            (EV_SYN, SYN_REPORT) => return self.flush(),
            (EV_SYN, SYN_DROPPED) => return self.resync(),
            (EV_ABS, ABS_X) => {
                self.x = event.value;
                self.dirty = true;
            }
            (EV_ABS, ABS_Y) => {
                self.y = event.value;
                self.dirty = true;
            }
            (EV_ABS, ABS_PRESSURE) => self.pressure = event.value,
            (EV_ABS, ABS_TILT_X) => self.tilt.0 = event.value,
            (EV_ABS, ABS_TILT_Y) => self.tilt.1 = event.value,
            // The axis is read and discarded: see the type comment.
            (EV_ABS, ABS_DISTANCE) => {}
            (EV_KEY, BTN_TOUCH) => {
                self.touching = event.value != 0;
                self.dirty = true;
            }
            (EV_KEY, BTN_TOOL_PEN) => {
                self.in_range = event.value != 0;
                if self.in_range {
                    self.tool = Pointer::Pen;
                }
                self.dirty = true;
            }
            (EV_KEY, BTN_TOOL_RUBBER) => {
                self.in_range = event.value != 0;
                if self.in_range {
                    self.tool = Pointer::Eraser;
                }
                self.dirty = true;
            }
            _ => {}
        }
        Vec::new()
    }

    /// The event this frame completed, positioned and with its axes attached.
    fn event(&mut self, phase: PointerPhase) -> PointerEvent {
        let contact = match self.contact {
            Some(contact) => contact,
            None => {
                let contact = self.ids.allocate();
                self.contact = Some(contact);
                contact
            }
        };
        let at = self.transform.map(self.x, self.y);
        let event = PointerEvent::new(at, phase, self.tool, contact);
        // A hovering pen is not pressing, so its pressure reading is not a
        // measurement of anything. Reporting the last stroke's value would be
        // inventing one.
        if phase == PointerPhase::Hover {
            return event.with_tilt(Tilt::from_hundredths(
                self.tilt.0,
                self.tilt.1,
                PEN_TILT_FULL_SCALE,
            ));
        }
        event
            .with_pressure(Pressure::from_raw(self.pressure, PEN_PRESSURE_FULL_SCALE))
            .with_tilt(Tilt::from_hundredths(
                self.tilt.0,
                self.tilt.1,
                PEN_TILT_FULL_SCALE,
            ))
    }

    fn flush(&mut self) -> Vec<PointerEvent> {
        if !self.dirty {
            return Vec::new();
        }
        self.dirty = false;

        let phase = match (self.down, self.touching, self.in_range) {
            (false, true, _) => PointerPhase::Down,
            (true, true, _) => PointerPhase::Moved,
            (true, false, _) => PointerPhase::Up,
            (false, false, true) => PointerPhase::Hover,
            // Out of range and not touching: nothing to report, and any run
            // that was in progress is over.
            (false, false, false) => {
                self.contact = None;
                return Vec::new();
            }
        };
        self.down = self.touching;

        let event = self.event(phase);
        if phase == PointerPhase::Up {
            // The hover that follows a lift belongs to the next press.
            self.contact = None;
        }
        vec![event]
    }

    fn resync(&mut self) -> Vec<PointerEvent> {
        self.dirty = false;
        self.touching = false;
        if !self.down {
            self.contact = None;
            return Vec::new();
        }
        self.down = false;
        let event = self.event(PointerPhase::Cancelled);
        self.contact = None;
        vec![event]
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_SLOT, ABS_MT_TRACKING_ID, ABS_PRESSURE,
        ABS_TILT_X, ABS_X, ABS_Y, BTN_TOOL_PEN, BTN_TOOL_RUBBER, BTN_TOUCH, ContactIds, EV_ABS,
        EV_KEY, EV_SYN, EVENT_BYTES, PenDecoder, RawEvent, SYN_DROPPED, SYN_REPORT, TouchDecoder,
    };
    use crate::input::transform::PointerTransform;
    use paper_sdk::{Pointer, PointerPhase};
    use std::collections::HashSet;

    fn event(kind: u16, code: u16, value: i32) -> RawEvent {
        RawEvent { kind, code, value }
    }

    fn syn() -> RawEvent {
        event(EV_SYN, SYN_REPORT, 0)
    }

    fn touch() -> TouchDecoder {
        TouchDecoder::new(PointerTransform::touch(), ContactIds::new())
    }

    fn pen() -> PenDecoder {
        PenDecoder::new(PointerTransform::pen(), ContactIds::new())
    }

    fn feed_all(decoder: &mut TouchDecoder, events: &[RawEvent]) -> Vec<paper_sdk::PointerEvent> {
        events.iter().flat_map(|e| decoder.feed(*e)).collect()
    }

    #[test]
    fn raw_events_decode_from_the_kernel_byte_layout() {
        let mut bytes = [0u8; EVENT_BYTES];
        bytes[16..18].copy_from_slice(&EV_ABS.to_le_bytes());
        bytes[18..20].copy_from_slice(&ABS_MT_TRACKING_ID.to_le_bytes());
        bytes[20..24].copy_from_slice(&(-1i32).to_le_bytes());

        let decoded = RawEvent::from_bytes(&bytes);
        assert_eq!(decoded.kind, EV_ABS);
        assert_eq!(decoded.code, ABS_MT_TRACKING_ID);
        assert_eq!(decoded.value, -1, "tracking ids are signed");
    }

    #[test]
    fn a_partial_tail_is_reported_rather_than_decoded_or_dropped() {
        let bytes = vec![0u8; EVENT_BYTES * 2 + 7];
        let (events, remainder) = RawEvent::decode_all(&bytes);
        assert_eq!(events.len(), 2);
        assert_eq!(remainder, 7);
    }

    #[test]
    fn nothing_is_emitted_until_the_frame_is_complete() {
        let mut decoder = touch();
        assert!(decoder.feed(event(EV_ABS, ABS_MT_SLOT, 0)).is_empty());
        assert!(
            decoder
                .feed(event(EV_ABS, ABS_MT_TRACKING_ID, 7))
                .is_empty()
        );
        assert!(
            decoder
                .feed(event(EV_ABS, ABS_MT_POSITION_X, 1032))
                .is_empty()
        );
        assert!(
            decoder
                .feed(event(EV_ABS, ABS_MT_POSITION_Y, 1416))
                .is_empty()
        );

        let frame = decoder.feed(syn());
        assert_eq!(frame.len(), 1);
        assert_eq!(frame[0].phase, PointerPhase::Down);
        assert_eq!(frame[0].pointer, Pointer::Touch);
        assert!((frame[0].at.x - 810.0).abs() < 0.01, "{:?}", frame[0]);
        assert!((frame[0].at.y - 1080.0).abs() < 0.01, "{:?}", frame[0]);
    }

    #[test]
    fn a_tap_is_down_then_up_and_nothing_after() {
        let mut decoder = touch();
        let down = feed_all(
            &mut decoder,
            &[
                event(EV_ABS, ABS_MT_SLOT, 0),
                event(EV_ABS, ABS_MT_TRACKING_ID, 3),
                event(EV_ABS, ABS_MT_POSITION_X, 200),
                event(EV_ABS, ABS_MT_POSITION_Y, 300),
                syn(),
            ],
        );
        assert_eq!(down[0].phase, PointerPhase::Down);

        let up = feed_all(
            &mut decoder,
            &[event(EV_ABS, ABS_MT_TRACKING_ID, -1), syn()],
        );
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].phase, PointerPhase::Up);
        assert!(up[0].is_tap());

        // The slot is finished. A stray SYN must not resurrect it.
        assert!(decoder.feed(syn()).is_empty());
    }

    #[test]
    fn a_drag_reports_moves_between_the_down_and_the_up() {
        let mut decoder = touch();
        feed_all(
            &mut decoder,
            &[
                event(EV_ABS, ABS_MT_SLOT, 0),
                event(EV_ABS, ABS_MT_TRACKING_ID, 1),
                event(EV_ABS, ABS_MT_POSITION_X, 100),
                event(EV_ABS, ABS_MT_POSITION_Y, 100),
                syn(),
            ],
        );
        let moved = feed_all(
            &mut decoder,
            &[event(EV_ABS, ABS_MT_POSITION_X, 140), syn()],
        );
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].phase, PointerPhase::Moved);
        assert!(!moved[0].is_tap());
    }

    #[test]
    fn two_fingers_are_two_contacts_with_their_own_slots() {
        let mut decoder = touch();
        let frame = feed_all(
            &mut decoder,
            &[
                event(EV_ABS, ABS_MT_SLOT, 0),
                event(EV_ABS, ABS_MT_TRACKING_ID, 10),
                event(EV_ABS, ABS_MT_POSITION_X, 100),
                event(EV_ABS, ABS_MT_POSITION_Y, 100),
                event(EV_ABS, ABS_MT_SLOT, 1),
                event(EV_ABS, ABS_MT_TRACKING_ID, 11),
                event(EV_ABS, ABS_MT_POSITION_X, 1900),
                event(EV_ABS, ABS_MT_POSITION_Y, 2700),
                syn(),
            ],
        );
        assert_eq!(frame.len(), 2);
        // Two fingers down at once are two different contacts, and the ids
        // that say so are what an app keys its per-finger state on.
        assert_ne!(frame[0].contact, frame[1].contact);
        assert!(frame[1].at.x > frame[0].at.x);
        assert!(frame.iter().all(|e| e.phase == PointerPhase::Down));
    }

    #[test]
    fn lifting_one_finger_leaves_the_other_alone() {
        let mut decoder = touch();
        feed_all(
            &mut decoder,
            &[
                event(EV_ABS, ABS_MT_SLOT, 0),
                event(EV_ABS, ABS_MT_TRACKING_ID, 10),
                event(EV_ABS, ABS_MT_POSITION_X, 100),
                event(EV_ABS, ABS_MT_POSITION_Y, 100),
                event(EV_ABS, ABS_MT_SLOT, 1),
                event(EV_ABS, ABS_MT_TRACKING_ID, 11),
                event(EV_ABS, ABS_MT_POSITION_X, 500),
                event(EV_ABS, ABS_MT_POSITION_Y, 500),
                syn(),
            ],
        );
        let frame = feed_all(
            &mut decoder,
            &[
                event(EV_ABS, ABS_MT_SLOT, 0),
                event(EV_ABS, ABS_MT_TRACKING_ID, -1),
                syn(),
            ],
        );
        assert_eq!(frame.len(), 1);
        assert_eq!(frame[0].phase, PointerPhase::Up);
    }

    #[test]
    fn a_dropped_frame_cancels_every_live_contact_instead_of_sticking_one() {
        let mut decoder = touch();
        feed_all(
            &mut decoder,
            &[
                event(EV_ABS, ABS_MT_SLOT, 0),
                event(EV_ABS, ABS_MT_TRACKING_ID, 10),
                event(EV_ABS, ABS_MT_POSITION_X, 100),
                event(EV_ABS, ABS_MT_POSITION_Y, 100),
                event(EV_ABS, ABS_MT_SLOT, 3),
                event(EV_ABS, ABS_MT_TRACKING_ID, 11),
                event(EV_ABS, ABS_MT_POSITION_X, 500),
                event(EV_ABS, ABS_MT_POSITION_Y, 500),
                syn(),
            ],
        );

        let cancelled = decoder.feed(event(EV_SYN, SYN_DROPPED, 0));
        assert_eq!(cancelled.len(), 2);
        assert!(cancelled.iter().all(|e| e.phase == PointerPhase::Cancelled));
        assert!(cancelled.iter().all(|e| !e.is_tap()));
        // Nothing survives the resync.
        assert!(decoder.feed(syn()).is_empty());
    }

    #[test]
    fn an_impossible_slot_number_does_not_index_out_of_bounds() {
        let mut decoder = touch();
        let frame = feed_all(
            &mut decoder,
            &[
                event(EV_ABS, ABS_MT_SLOT, 9_999),
                event(EV_ABS, ABS_MT_TRACKING_ID, 1),
                event(EV_ABS, ABS_MT_POSITION_X, 10),
                event(EV_ABS, ABS_MT_POSITION_Y, 10),
                syn(),
            ],
        );
        assert_eq!(frame.len(), 1);

        let mut decoder = touch();
        assert!(decoder.feed(event(EV_ABS, ABS_MT_SLOT, -4)).is_empty());
    }

    #[test]
    fn the_pen_reports_pressure_tilt_and_the_eraser_as_a_tool() {
        let mut decoder = pen();
        let events = [
            event(EV_KEY, BTN_TOOL_RUBBER, 1),
            event(EV_ABS, ABS_X, 5590),
            event(EV_ABS, ABS_Y, 7670),
            event(EV_ABS, ABS_PRESSURE, 2048),
            event(EV_ABS, ABS_TILT_X, -3000),
            event(EV_KEY, BTN_TOUCH, 1),
            syn(),
        ];
        let frame: Vec<_> = events.iter().flat_map(|e| decoder.feed(*e)).collect();

        assert_eq!(frame.len(), 1);
        // The eraser is the pen, reversed — one device, a tool-type switch.
        assert_eq!(frame[0].pointer, Pointer::Eraser);
        assert!(frame[0].pointer.is_stylus());
        // Normalized, not raw: 2048 of 4096 is half pressure, -3000
        // hundredths is -30 degrees.
        assert!((frame[0].pressure.expect("pen reports pressure").get() - 0.5).abs() < 1e-6);
        let tilt = frame[0].tilt.expect("pen reports tilt");
        assert!((tilt.x_degrees + 30.0).abs() < 1e-6);
        assert_eq!(tilt.y_degrees, 0.0);
        assert_eq!(frame[0].phase, PointerPhase::Down);
        assert!((frame[0].at.x - 810.0).abs() < 0.01);
    }

    /// A finger reports no pressure, so a touch event carries none. The rule
    /// the whole input model rests on, checked where it would be easiest to
    /// break: the decoder that knows the pen reports one.
    #[test]
    fn touch_carries_no_invented_pressure_or_tilt() {
        let mut decoder = touch();
        let frame = feed_all(
            &mut decoder,
            &[
                event(EV_ABS, ABS_MT_TRACKING_ID, 4),
                event(EV_ABS, ABS_MT_POSITION_X, 100),
                event(EV_ABS, ABS_MT_POSITION_Y, 100),
                syn(),
            ],
        );
        assert_eq!(frame.len(), 1);
        assert_eq!(frame[0].pressure, None);
        assert_eq!(frame[0].tilt, None);
        assert_eq!(frame[0].pointer, Pointer::Touch);
    }

    /// A tracking id the kernel reuses must not look like the same finger. The
    /// contact ids the decoder hands out are allocated, never echoed.
    #[test]
    fn a_reused_kernel_tracking_id_is_a_new_contact() {
        let mut decoder = touch();
        let first = feed_all(
            &mut decoder,
            &[
                event(EV_ABS, ABS_MT_TRACKING_ID, 7),
                event(EV_ABS, ABS_MT_POSITION_X, 100),
                event(EV_ABS, ABS_MT_POSITION_Y, 100),
                syn(),
                event(EV_ABS, ABS_MT_TRACKING_ID, -1),
                syn(),
            ],
        );
        let second = feed_all(
            &mut decoder,
            &[
                event(EV_ABS, ABS_MT_TRACKING_ID, 7),
                event(EV_ABS, ABS_MT_POSITION_X, 200),
                event(EV_ABS, ABS_MT_POSITION_Y, 200),
                syn(),
            ],
        );
        assert_ne!(first[0].contact, second[0].contact);
    }

    /// Every contact in a session gets its own id, across both nodes, because
    /// they share one allocator.
    #[test]
    fn the_pen_and_the_touchscreen_never_share_a_contact_id() {
        let ids = ContactIds::new();
        let mut fingers = TouchDecoder::new(PointerTransform::touch(), ids.clone());
        let mut nib = PenDecoder::new(PointerTransform::pen(), ids);

        let mut seen = HashSet::new();
        for raw in [
            event(EV_ABS, ABS_MT_TRACKING_ID, 1),
            event(EV_ABS, ABS_MT_POSITION_X, 10),
            event(EV_ABS, ABS_MT_POSITION_Y, 10),
            syn(),
        ] {
            seen.extend(fingers.feed(raw).into_iter().map(|e| e.contact));
        }
        for raw in [
            event(EV_KEY, BTN_TOOL_PEN, 1),
            event(EV_ABS, ABS_X, 10),
            event(EV_ABS, ABS_Y, 10),
            event(EV_KEY, BTN_TOUCH, 1),
            syn(),
        ] {
            seen.extend(nib.feed(raw).into_iter().map(|e| e.contact));
        }
        assert_eq!(seen.len(), 2, "{seen:?}");
    }

    /// A pen the digitizer can see but that is not touching is a hover, and
    /// the hover carries no pressure — a pen in the air is not pressing, and
    /// reporting the last stroke's reading would be inventing one.
    #[test]
    fn a_pen_in_range_hovers() {
        let mut decoder = pen();
        let frame: Vec<_> = [
            event(EV_KEY, BTN_TOOL_PEN, 1),
            event(EV_ABS, ABS_X, 100),
            event(EV_ABS, ABS_Y, 100),
            event(EV_ABS, super::ABS_DISTANCE, 40),
            syn(),
        ]
        .iter()
        .flat_map(|e| decoder.feed(*e))
        .collect();
        assert_eq!(frame.len(), 1, "{frame:?}");
        assert_eq!(frame[0].phase, PointerPhase::Hover);
        assert_eq!(frame[0].pressure, None);
        assert!(!frame[0].phase.is_contact());
    }

    /// A pen nowhere near the glass says nothing. Position without a tool
    /// button is not a hover — it is the last place the pen was.
    #[test]
    fn a_pen_out_of_range_produces_nothing_at_all() {
        let mut decoder = pen();
        let frame: Vec<_> = [event(EV_ABS, ABS_X, 100), event(EV_ABS, ABS_Y, 100), syn()]
            .iter()
            .flat_map(|e| decoder.feed(*e))
            .collect();
        assert!(frame.is_empty(), "{frame:?}");
    }

    /// The hover that precedes a stroke is the same contact as the stroke, so
    /// an app previewing a nib position and then drawing sees one contact.
    /// The hover after a lift is a different one, because the next press is.
    #[test]
    fn a_hover_and_the_stroke_it_becomes_are_one_contact() {
        let mut decoder = pen();
        let mut events = Vec::new();
        for raw in [
            event(EV_KEY, BTN_TOOL_PEN, 1),
            event(EV_ABS, ABS_X, 100),
            event(EV_ABS, ABS_Y, 100),
            syn(),
            event(EV_KEY, BTN_TOUCH, 1),
            syn(),
            event(EV_KEY, BTN_TOUCH, 0),
            syn(),
            event(EV_ABS, ABS_X, 120),
            syn(),
        ] {
            events.extend(decoder.feed(raw));
        }
        let phases: Vec<_> = events.iter().map(|e| e.phase).collect();
        assert_eq!(
            phases,
            vec![
                PointerPhase::Hover,
                PointerPhase::Down,
                PointerPhase::Up,
                PointerPhase::Hover
            ]
        );
        assert_eq!(events[0].contact, events[1].contact);
        assert_eq!(events[1].contact, events[2].contact);
        assert_ne!(events[2].contact, events[3].contact);
    }

    #[test]
    fn a_pen_stroke_is_down_moved_up() {
        let mut decoder = pen();
        let mut phases = Vec::new();
        for batch in [
            vec![
                event(EV_ABS, ABS_X, 100),
                event(EV_ABS, ABS_Y, 100),
                event(EV_KEY, BTN_TOUCH, 1),
                syn(),
            ],
            vec![event(EV_ABS, ABS_X, 160), syn()],
            vec![event(EV_KEY, BTN_TOUCH, 0), syn()],
            vec![syn()],
        ] {
            for raw in batch {
                phases.extend(decoder.feed(raw).into_iter().map(|e| e.phase));
            }
        }
        assert_eq!(
            phases,
            vec![PointerPhase::Down, PointerPhase::Moved, PointerPhase::Up]
        );
    }

    #[test]
    fn a_dropped_pen_frame_cancels_a_stroke_in_progress() {
        let mut decoder = pen();
        for raw in [
            event(EV_ABS, ABS_X, 100),
            event(EV_ABS, ABS_Y, 100),
            event(EV_KEY, BTN_TOUCH, 1),
            syn(),
        ] {
            decoder.feed(raw);
        }
        let cancelled = decoder.feed(event(EV_SYN, SYN_DROPPED, 0));
        assert_eq!(cancelled.len(), 1);
        assert_eq!(cancelled[0].phase, PointerPhase::Cancelled);
        // And with nothing in progress it says nothing.
        assert!(decoder.feed(event(EV_SYN, SYN_DROPPED, 0)).is_empty());
    }
}
