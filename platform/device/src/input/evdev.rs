//! Decoding `/dev/input/event*` without Qt in the way.
//!
//! Both nodes are readable concurrently and neither is `EVIOCGRAB`bed by the
//! vendor stack (WWW-1), so reading evdev directly in Rust costs nothing and
//! removes an entire toolkit from the input path. Nothing here opens a file:
//! this module turns bytes into events, which is the part that can be tested
//! on a Mac against captured traces.

use paper_sdk::{Pointer, PointerEvent, PointerPhase};

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

/// Which physical implement produced a contact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    /// A finger on the touchscreen.
    Finger,
    /// The stylus, nib down.
    Pen,
    /// The stylus, reversed. `BTN_TOOL_RUBBER`.
    Eraser,
}

impl Tool {
    /// The SDK pointer kind this tool presents as.
    pub const fn pointer(self) -> Pointer {
        match self {
            Self::Finger => Pointer::Touch,
            Self::Pen | Self::Eraser => Pointer::Pen,
        }
    }
}

/// A decoded contact event, richer than the SDK carries today.
///
/// [`paper_sdk::PointerEvent`] is `#[non_exhaustive]` and deliberately narrow
/// until WWW-5 designs the input contract. Rather than pre-empt that, the
/// device adapter reports what the hardware actually said and keeps the SDK
/// event inside as the part that is already agreed. When WWW-5 widens
/// `PointerEvent`, the extra fields here fold into it and this struct
/// shrinks — no call site above the adapter changes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContactEvent {
    /// The event in panel coordinates.
    pub pointer: PointerEvent,
    /// Which of the ten touch slots, or `0` for the pen.
    pub slot: u8,
    /// What made the contact.
    pub tool: Tool,
    /// Pen pressure, 0–4096. `None` for touch, which does not report it.
    pub pressure: Option<i32>,
    /// Pen tilt in hundredths of a degree, ±9000. `None` for touch.
    pub tilt: Option<(i32, i32)>,
}

/// Turns touchscreen reports into contact events.
///
/// Multitouch protocol B: the kernel names a slot, then describes it, and a
/// `SYN_REPORT` ends the frame. Everything is deferred to the `SYN_REPORT`
/// because a half-described frame is not a position anyone should act on.
#[derive(Debug)]
pub struct TouchDecoder {
    transform: PointerTransform,
    slots: [Slot; MAX_TOUCH_SLOTS],
    current: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    tracking: Option<i32>,
    x: i32,
    y: i32,
    down: bool,
    dirty: bool,
    lifted: bool,
}

impl TouchDecoder {
    /// A decoder using `transform` to reach panel coordinates.
    pub fn new(transform: PointerTransform) -> Self {
        Self {
            transform,
            slots: [Slot::default(); MAX_TOUCH_SLOTS],
            current: 0,
        }
    }

    /// Feeds one raw event, returning whatever frame it completed.
    pub fn feed(&mut self, event: RawEvent) -> Vec<ContactEvent> {
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

    fn flush(&mut self) -> Vec<ContactEvent> {
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

            out.push(ContactEvent {
                pointer: PointerEvent::new(at, phase, Pointer::Touch),
                slot: index as u8,
                tool: Tool::Finger,
                pressure: None,
                tilt: None,
            });

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

    fn resync(&mut self) -> Vec<ContactEvent> {
        let mut out = Vec::new();
        for index in 0..MAX_TOUCH_SLOTS {
            let slot = self.slots[index];
            if slot.down {
                out.push(ContactEvent {
                    pointer: PointerEvent::new(
                        self.transform.map(slot.x, slot.y),
                        PointerPhase::Cancelled,
                        Pointer::Touch,
                    ),
                    slot: index as u8,
                    tool: Tool::Finger,
                    pressure: None,
                    tilt: None,
                });
            }
            self.slots[index] = Slot::default();
        }
        self.current = 0;
        out
    }
}

/// Turns pen reports into contact events.
///
/// Hovering is silently dropped. The pen reports `ABS_DISTANCE` and a tool
/// button well before it touches the glass, and [`PointerPhase`] has no phase
/// that means "near". Inventing one here would put a hover contract in the
/// device adapter that WWW-5 has not agreed; dropping it loses nothing v1
/// needs.
#[derive(Debug)]
pub struct PenDecoder {
    transform: PointerTransform,
    x: i32,
    y: i32,
    pressure: i32,
    tilt: (i32, i32),
    tool: Tool,
    touching: bool,
    down: bool,
    dirty: bool,
}

impl PenDecoder {
    /// A decoder using `transform` to reach panel coordinates.
    pub fn new(transform: PointerTransform) -> Self {
        Self {
            transform,
            x: 0,
            y: 0,
            pressure: 0,
            tilt: (0, 0),
            tool: Tool::Pen,
            touching: false,
            down: false,
            dirty: false,
        }
    }

    /// Feeds one raw event, returning whatever frame it completed.
    pub fn feed(&mut self, event: RawEvent) -> Vec<ContactEvent> {
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
            // Hover distance is read and ignored: see the type comment.
            (EV_ABS, ABS_DISTANCE) => {}
            (EV_KEY, BTN_TOUCH) => {
                self.touching = event.value != 0;
                self.dirty = true;
            }
            (EV_KEY, BTN_TOOL_PEN) if event.value != 0 => self.tool = Tool::Pen,
            (EV_KEY, BTN_TOOL_RUBBER) if event.value != 0 => self.tool = Tool::Eraser,
            _ => {}
        }
        Vec::new()
    }

    fn flush(&mut self) -> Vec<ContactEvent> {
        if !self.dirty {
            return Vec::new();
        }
        self.dirty = false;

        let phase = match (self.down, self.touching) {
            (false, true) => PointerPhase::Down,
            (true, true) => PointerPhase::Moved,
            (true, false) => PointerPhase::Up,
            (false, false) => return Vec::new(),
        };
        self.down = self.touching;

        vec![ContactEvent {
            pointer: PointerEvent::new(self.transform.map(self.x, self.y), phase, Pointer::Pen),
            slot: 0,
            tool: self.tool,
            pressure: Some(self.pressure),
            tilt: Some(self.tilt),
        }]
    }

    fn resync(&mut self) -> Vec<ContactEvent> {
        self.dirty = false;
        self.touching = false;
        if !self.down {
            return Vec::new();
        }
        self.down = false;
        vec![ContactEvent {
            pointer: PointerEvent::new(
                self.transform.map(self.x, self.y),
                PointerPhase::Cancelled,
                Pointer::Pen,
            ),
            slot: 0,
            tool: self.tool,
            pressure: Some(self.pressure),
            tilt: Some(self.tilt),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_SLOT, ABS_MT_TRACKING_ID, ABS_PRESSURE,
        ABS_TILT_X, ABS_X, ABS_Y, BTN_TOOL_RUBBER, BTN_TOUCH, EV_ABS, EV_KEY, EV_SYN, EVENT_BYTES,
        PenDecoder, RawEvent, SYN_DROPPED, SYN_REPORT, Tool, TouchDecoder,
    };
    use crate::input::transform::PointerTransform;
    use paper_sdk::{Pointer, PointerPhase};

    fn event(kind: u16, code: u16, value: i32) -> RawEvent {
        RawEvent { kind, code, value }
    }

    fn syn() -> RawEvent {
        event(EV_SYN, SYN_REPORT, 0)
    }

    fn touch() -> TouchDecoder {
        TouchDecoder::new(PointerTransform::touch())
    }

    fn pen() -> PenDecoder {
        PenDecoder::new(PointerTransform::pen())
    }

    fn feed_all(decoder: &mut TouchDecoder, events: &[RawEvent]) -> Vec<super::ContactEvent> {
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
        assert_eq!(frame[0].pointer.phase, PointerPhase::Down);
        assert_eq!(frame[0].pointer.pointer, Pointer::Touch);
        assert!(
            (frame[0].pointer.at.x - 810.0).abs() < 0.01,
            "{:?}",
            frame[0]
        );
        assert!(
            (frame[0].pointer.at.y - 1080.0).abs() < 0.01,
            "{:?}",
            frame[0]
        );
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
        assert_eq!(down[0].pointer.phase, PointerPhase::Down);

        let up = feed_all(
            &mut decoder,
            &[event(EV_ABS, ABS_MT_TRACKING_ID, -1), syn()],
        );
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].pointer.phase, PointerPhase::Up);
        assert!(up[0].pointer.is_tap());

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
        assert_eq!(moved[0].pointer.phase, PointerPhase::Moved);
        assert!(!moved[0].pointer.is_tap());
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
        assert_eq!(frame[0].slot, 0);
        assert_eq!(frame[1].slot, 1);
        assert!(frame[1].pointer.at.x > frame[0].pointer.at.x);
        assert!(frame.iter().all(|e| e.pointer.phase == PointerPhase::Down));
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
        assert_eq!(frame[0].slot, 0);
        assert_eq!(frame[0].pointer.phase, PointerPhase::Up);
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
        assert!(
            cancelled
                .iter()
                .all(|e| e.pointer.phase == PointerPhase::Cancelled)
        );
        assert!(cancelled.iter().all(|e| !e.pointer.is_tap()));
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
        assert!((frame[0].slot as usize) < super::MAX_TOUCH_SLOTS);

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
        assert_eq!(frame[0].tool, Tool::Eraser);
        assert_eq!(frame[0].tool.pointer(), Pointer::Pen);
        assert_eq!(frame[0].pressure, Some(2048));
        assert_eq!(frame[0].tilt, Some((-3000, 0)));
        assert_eq!(frame[0].pointer.phase, PointerPhase::Down);
        assert!((frame[0].pointer.at.x - 810.0).abs() < 0.01);
    }

    #[test]
    fn hovering_produces_nothing_at_all() {
        let mut decoder = pen();
        let frame: Vec<_> = [
            event(EV_ABS, ABS_X, 100),
            event(EV_ABS, ABS_Y, 100),
            event(EV_ABS, super::ABS_DISTANCE, 40),
            syn(),
        ]
        .iter()
        .flat_map(|e| decoder.feed(*e))
        .collect();
        assert!(frame.is_empty(), "{frame:?}");
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
                phases.extend(decoder.feed(raw).into_iter().map(|e| e.pointer.phase));
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
        assert_eq!(cancelled[0].pointer.phase, PointerPhase::Cancelled);
        // And with nothing in progress it says nothing.
        assert!(decoder.feed(event(EV_SYN, SYN_DROPPED, 0)).is_empty());
    }
}
