//! Gesture arbitration: which pointer events the system claims for itself,
//! and which pass through to the focused app (WWW-52, WWW-80).
//!
//! [`GestureDetector::arbitrate`] is the whole of it — pointer events in,
//! [`Verdict`] out, one call per event. There is no clock, no socket and no
//! executor here: [`PointerEvent`] carries no timestamp (`paper_protocol::
//! input`), so every rule below is spatial — where a contact began and how
//! far it has moved — never how long anything took. That is a deliberate
//! consequence of the wire shape, not an oversight: a detector that needed
//! real time would need wiring into whatever event loop reads the clock
//! (WWW-78), and this ticket's whole point is that the detector does not.
//!
//! ## The two things this arbitrates (ADR-0034)
//!
//! - **Long-press vs swipe.** There is no dwell timer, so "long press" is
//!   simply whatever does not cross a movement threshold. A contact that
//!   starts in an edge zone and stays there — however long, in event count —
//!   is an [`Verdict::App`] long-press candidate right up until it crosses
//!   [`EDGE_SWIPE_MIN_TRAVEL`], at which point it is irrevocably a system
//!   [`SystemGesture::EdgeSwipe`]. Movement is the only signal; there is
//!   nothing else to arbitrate on.
//! - **App vs system.** The system never claims a contact's very first event
//!   — [`SystemGesture::EdgeSwipe`] and [`SystemGesture::Escape`] both need
//!   movement to confirm, and the first event a contact produces is a
//!   `Down` with no movement yet. So the default is always [`Verdict::App`],
//!   and the system *preempts* once a gesture confirms. A caller that has
//!   already forwarded a contact's early events to the app must treat a
//!   later [`Verdict::System`] for that same contact as an implicit cancel —
//!   this module only classifies events, it does not rewrite a stream
//!   already sent, so relaying that cancellation is the event loop's job
//!   (WWW-78), not this one's.

use paper_protocol::{ContactId, Point, Pointer, PointerEvent, PointerPhase, Size};
use std::collections::BTreeMap;

/// How close to the panel's edge, in canvas pixels, a contact must begin for
/// [`GestureDetector::arbitrate`] to consider it for [`SystemGesture::EdgeSwipe`].
///
/// A contact starting outside this band is never an edge swipe, however far
/// it later travels — an app-content drag across the whole screen must not
/// turn into a system gesture just because it happens to end near an edge.
pub const EDGE_ZONE_DEPTH: f32 = 48.0;

/// How far inward, from its own origin, a contact must travel before an
/// edge-zone origin is confirmed as [`SystemGesture::EdgeSwipe`] rather than
/// a tap or a long press held near the edge.
pub const EDGE_SWIPE_MIN_TRAVEL: f32 = 96.0;

/// How much closer two concurrent touch contacts must move, in canvas
/// pixels, before they are confirmed as [`SystemGesture::Escape`] rather
/// than two independent touches that happen to be onscreen together.
pub const PINCH_MIN_CLOSING: f32 = 150.0;

/// One of the panel's four edges, in canvas space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Edge {
    /// The top edge, `y == 0`.
    Top,
    /// The bottom edge, `y == screen.height`.
    Bottom,
    /// The left edge, `x == 0`.
    Left,
    /// The right edge, `x == screen.width`.
    Right,
}

/// A gesture the system claims for itself rather than passing to the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemGesture {
    /// Two touch contacts closing together — the system's own
    /// return-to-Home gesture, live regardless of which app is focused.
    Escape,
    /// A contact that began within [`EDGE_ZONE_DEPTH`] of `Edge` and has
    /// since travelled inward past [`EDGE_SWIPE_MIN_TRAVEL`].
    EdgeSwipe(Edge),
}

/// What a pointer event belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// The system claims this event.
    System(SystemGesture),
    /// Passes through to the focused app, unchanged.
    App,
}

/// One contact's history, kept only for as long as the contact is live.
#[derive(Debug, Clone, Copy)]
struct Track {
    pointer: Pointer,
    origin: Point,
    last: Point,
    /// The single edge this contact began within [`EDGE_ZONE_DEPTH`] of, or
    /// `None` if it began mid-screen or in a corner close to two edges at
    /// once — a corner start is deliberately ambiguous between them and so
    /// counts as neither (see `a_corner_start_claims_no_single_edge`).
    edge: Option<Edge>,
    claimed: Option<SystemGesture>,
}

impl Track {
    fn new(event: PointerEvent, screen: Size) -> Self {
        Self {
            pointer: event.pointer,
            origin: event.at,
            last: event.at,
            edge: edge_of(event.at, screen),
            claimed: None,
        }
    }
}

/// The pure, per-session gesture arbiter: pointer events in, a [`Verdict`]
/// out, one call per event. See the module docs for the rules this applies.
#[derive(Debug, Clone)]
pub struct GestureDetector {
    screen: Size,
    contacts: BTreeMap<ContactId, Track>,
}

impl GestureDetector {
    /// A detector over a panel of this extent. `screen` is the panel's own
    /// canvas size (`paper_sdk::SCREEN` in production), not any one app's
    /// viewport — gesture zones are measured against the physical edges,
    /// which is why this crate takes a `Size` rather than depending on the
    /// SDK for the constant.
    pub fn new(screen: Size) -> Self {
        Self {
            screen,
            contacts: BTreeMap::new(),
        }
    }

    /// Classifies one pointer event and updates this contact's history.
    ///
    /// Events for a contact this detector never saw a `Down` for (e.g. fed
    /// a stream that started mid-gesture) are passed straight to the app:
    /// there is no origin to measure movement against, so nothing here can
    /// be confirmed.
    pub fn arbitrate(&mut self, event: PointerEvent) -> Verdict {
        if event.phase == PointerPhase::Hover {
            // The pen hovers; hovering is never a contact and never
            // participates in a gesture the touchscreen alone can make.
            return Verdict::App;
        }

        if event.phase == PointerPhase::Down {
            self.contacts
                .insert(event.contact, Track::new(event, self.screen));
        } else if let Some(track) = self.contacts.get_mut(&event.contact) {
            track.last = event.at;
        }

        self.confirm_pinch();

        let verdict = match self.contacts.get(&event.contact) {
            Some(track) => match track.claimed {
                Some(gesture) => Verdict::System(gesture),
                None => match track.edge {
                    Some(edge)
                        if inward_travel(track.origin, track.last, edge)
                            >= EDGE_SWIPE_MIN_TRAVEL =>
                    {
                        Verdict::System(SystemGesture::EdgeSwipe(edge))
                    }
                    _ => Verdict::App,
                },
            },
            None => Verdict::App,
        };

        if let Verdict::System(gesture) = verdict {
            self.claim(event.contact, gesture);
        }

        if event.phase.ends_contact() {
            self.contacts.remove(&event.contact);
        }

        verdict
    }

    /// If exactly two unclaimed touch contacts are live and have closed by
    /// [`PINCH_MIN_CLOSING`] since both first appeared, claims
    /// [`SystemGesture::Escape`] on both. A third live, unclaimed touch
    /// contact rules pinch recognition out entirely — v1 defines the escape
    /// gesture as two fingers, not "the two closest of however many are
    /// down," and guessing which two the user meant is worse than not
    /// recognising a pinch at all.
    fn confirm_pinch(&mut self) {
        let mut unclaimed_touches = self
            .contacts
            .iter()
            .filter(|(_, track)| track.pointer == Pointer::Touch && track.claimed.is_none())
            .map(|(id, _)| *id);
        let (Some(a), Some(b), None) = (
            unclaimed_touches.next(),
            unclaimed_touches.next(),
            unclaimed_touches.next(),
        ) else {
            return;
        };

        let (Some(track_a), Some(track_b)) = (self.contacts.get(&a), self.contacts.get(&b)) else {
            return;
        };
        let initial = distance(track_a.origin, track_b.origin);
        let current = distance(track_a.last, track_b.last);
        if initial - current >= PINCH_MIN_CLOSING {
            self.claim(a, SystemGesture::Escape);
            self.claim(b, SystemGesture::Escape);
        }
    }

    fn claim(&mut self, contact: ContactId, gesture: SystemGesture) {
        if let Some(track) = self.contacts.get_mut(&contact) {
            track.claimed = Some(gesture);
        }
    }
}

/// The single edge `point` began within [`EDGE_ZONE_DEPTH`] of, or `None` if
/// it is mid-screen or in a corner close to two edges at once.
fn edge_of(point: Point, screen: Size) -> Option<Edge> {
    let near_top = point.y <= EDGE_ZONE_DEPTH;
    let near_bottom = point.y >= screen.height as f32 - EDGE_ZONE_DEPTH;
    let near_left = point.x <= EDGE_ZONE_DEPTH;
    let near_right = point.x >= screen.width as f32 - EDGE_ZONE_DEPTH;

    match (near_top, near_bottom, near_left, near_right) {
        (true, false, false, false) => Some(Edge::Top),
        (false, true, false, false) => Some(Edge::Bottom),
        (false, false, true, false) => Some(Edge::Left),
        (false, false, false, true) => Some(Edge::Right),
        _ => None,
    }
}

/// How far `last` has moved from `origin`, measured along `edge`'s inward
/// normal alone — lateral travel along the edge does not count, so a finger
/// dragged sideways just inside the zone never confirms a swipe.
fn inward_travel(origin: Point, last: Point, edge: Edge) -> f32 {
    match edge {
        Edge::Top => last.y - origin.y,
        Edge::Bottom => origin.y - last.y,
        Edge::Left => last.x - origin.x,
        Edge::Right => origin.x - last.x,
    }
}

fn distance(a: Point, b: Point) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::{
        EDGE_SWIPE_MIN_TRAVEL, EDGE_ZONE_DEPTH, Edge, GestureDetector, PINCH_MIN_CLOSING,
        SystemGesture, Verdict,
    };
    use paper_protocol::{ContactId, Point, Pointer, PointerEvent, PointerPhase, Size};

    const SCREEN: Size = Size::new(1620, 2160);

    fn detector() -> GestureDetector {
        GestureDetector::new(SCREEN)
    }

    fn touch(at: Point, phase: PointerPhase, contact: u64) -> PointerEvent {
        PointerEvent::new(at, phase, Pointer::Touch, ContactId::new(contact))
    }

    #[test]
    fn a_tap_in_the_middle_of_the_screen_passes_to_the_app() {
        let mut arbiter = detector();
        let centre = Point::new(810.0, 1080.0);
        assert_eq!(
            arbiter.arbitrate(touch(centre, PointerPhase::Down, 0)),
            Verdict::App
        );
        assert_eq!(
            arbiter.arbitrate(touch(centre, PointerPhase::Up, 0)),
            Verdict::App
        );
    }

    /// The long-press-vs-swipe rule: with no dwell timer, a contact held
    /// near an edge without crossing the travel threshold stays the app's,
    /// no matter how many events it produces — there is nothing here that
    /// promotes a long hold on its own.
    #[test]
    fn a_stationary_hold_near_an_edge_stays_with_the_app() {
        let mut arbiter = detector();
        let near_top = Point::new(800.0, 4.0);
        assert_eq!(
            arbiter.arbitrate(touch(near_top, PointerPhase::Down, 0)),
            Verdict::App
        );
        for _ in 0..20 {
            assert_eq!(
                arbiter.arbitrate(touch(near_top, PointerPhase::Moved, 0)),
                Verdict::App
            );
        }
        assert_eq!(
            arbiter.arbitrate(touch(near_top, PointerPhase::Up, 0)),
            Verdict::App
        );
    }

    #[test]
    fn a_confirmed_inward_swipe_from_the_top_edge_is_the_systems() {
        let mut arbiter = detector();
        let origin = Point::new(800.0, 4.0);
        assert_eq!(
            arbiter.arbitrate(touch(origin, PointerPhase::Down, 0)),
            Verdict::App
        );

        let short_of_threshold = origin.offset(0.0, EDGE_SWIPE_MIN_TRAVEL - 1.0);
        assert_eq!(
            arbiter.arbitrate(touch(short_of_threshold, PointerPhase::Moved, 0)),
            Verdict::App
        );

        let past_threshold = origin.offset(0.0, EDGE_SWIPE_MIN_TRAVEL);
        assert_eq!(
            arbiter.arbitrate(touch(past_threshold, PointerPhase::Moved, 0)),
            Verdict::System(SystemGesture::EdgeSwipe(Edge::Top))
        );

        // Once claimed, the contact stays the system's for the rest of its
        // life, even on the release that ends it.
        assert_eq!(
            arbiter.arbitrate(touch(past_threshold, PointerPhase::Up, 0)),
            Verdict::System(SystemGesture::EdgeSwipe(Edge::Top))
        );
    }

    #[test]
    fn a_contact_starting_outside_the_edge_zone_never_becomes_an_edge_swipe() {
        let mut arbiter = detector();
        let origin = Point::new(800.0, EDGE_ZONE_DEPTH + 1.0);
        arbiter.arbitrate(touch(origin, PointerPhase::Down, 0));
        let travelled = origin.offset(0.0, EDGE_SWIPE_MIN_TRAVEL * 4.0);
        assert_eq!(
            arbiter.arbitrate(touch(travelled, PointerPhase::Moved, 0)),
            Verdict::App
        );
    }

    /// A corner is close to two edges at once, which is exactly the
    /// ambiguity this detector refuses to guess at — see `edge_of`.
    #[test]
    fn a_corner_start_claims_no_single_edge() {
        let mut arbiter = detector();
        let corner = Point::new(2.0, 2.0);
        arbiter.arbitrate(touch(corner, PointerPhase::Down, 0));
        let travelled = corner.offset(EDGE_SWIPE_MIN_TRAVEL * 4.0, EDGE_SWIPE_MIN_TRAVEL * 4.0);
        assert_eq!(
            arbiter.arbitrate(touch(travelled, PointerPhase::Moved, 0)),
            Verdict::App
        );
    }

    #[test]
    fn two_fingers_closing_together_is_the_escape_pinch() {
        let mut arbiter = detector();
        let left_origin = Point::new(400.0, 1080.0);
        let right_origin = Point::new(1200.0, 1080.0);
        assert_eq!(
            arbiter.arbitrate(touch(left_origin, PointerPhase::Down, 0)),
            Verdict::App
        );
        assert_eq!(
            arbiter.arbitrate(touch(right_origin, PointerPhase::Down, 1)),
            Verdict::App
        );

        // Distance starts at 800; close it past PINCH_MIN_CLOSING (150).
        let left_moved = left_origin.offset(PINCH_MIN_CLOSING, 0.0);
        let right_moved = right_origin.offset(-PINCH_MIN_CLOSING, 0.0);
        assert_eq!(
            arbiter.arbitrate(touch(left_moved, PointerPhase::Moved, 0)),
            Verdict::System(SystemGesture::Escape)
        );
        // The partner contact flips to `System` on its own next event —
        // this call is what actually updates its `last` and re-checks the
        // shared pinch state (see the module docs' note on this lag).
        assert_eq!(
            arbiter.arbitrate(touch(right_moved, PointerPhase::Moved, 1)),
            Verdict::System(SystemGesture::Escape)
        );
    }

    #[test]
    fn two_fingers_that_stay_apart_are_not_a_pinch() {
        let mut arbiter = detector();
        let left = Point::new(400.0, 1080.0);
        let right = Point::new(1200.0, 1080.0);
        arbiter.arbitrate(touch(left, PointerPhase::Down, 0));
        arbiter.arbitrate(touch(right, PointerPhase::Down, 1));

        let left_moved = left.offset(10.0, 0.0);
        assert_eq!(
            arbiter.arbitrate(touch(left_moved, PointerPhase::Moved, 0)),
            Verdict::App
        );
    }

    /// v1's escape gesture is defined as exactly two fingers. A third live
    /// touch contact makes "which two converged" a guess, so pinch
    /// recognition is withheld entirely rather than picking a pair.
    #[test]
    fn a_third_touch_contact_rules_out_pinch_recognition() {
        let mut arbiter = detector();
        let left = Point::new(400.0, 1080.0);
        let right = Point::new(1200.0, 1080.0);
        let third = Point::new(810.0, 400.0);
        arbiter.arbitrate(touch(left, PointerPhase::Down, 0));
        arbiter.arbitrate(touch(right, PointerPhase::Down, 1));
        arbiter.arbitrate(touch(third, PointerPhase::Down, 2));

        let left_moved = left.offset(PINCH_MIN_CLOSING, 0.0);
        let right_moved = right.offset(-PINCH_MIN_CLOSING, 0.0);
        arbiter.arbitrate(touch(left_moved, PointerPhase::Moved, 0));
        assert_eq!(
            arbiter.arbitrate(touch(right_moved, PointerPhase::Moved, 1)),
            Verdict::App
        );
    }

    #[test]
    fn a_pen_never_participates_in_the_touch_only_escape_pinch() {
        let mut arbiter = detector();
        let left = Point::new(400.0, 1080.0);
        let right = Point::new(1200.0, 1080.0);
        arbiter.arbitrate(PointerEvent::new(
            left,
            PointerPhase::Down,
            Pointer::Pen,
            ContactId::new(0),
        ));
        arbiter.arbitrate(touch(right, PointerPhase::Down, 1));

        let left_moved = left.offset(PINCH_MIN_CLOSING, 0.0);
        let right_moved = right.offset(-PINCH_MIN_CLOSING, 0.0);
        arbiter.arbitrate(PointerEvent::new(
            left_moved,
            PointerPhase::Moved,
            Pointer::Pen,
            ContactId::new(0),
        ));
        assert_eq!(
            arbiter.arbitrate(touch(right_moved, PointerPhase::Moved, 1)),
            Verdict::App
        );
    }

    #[test]
    fn hover_never_arbitrates_and_never_starts_a_contact() {
        let mut arbiter = detector();
        let hover_at = Point::new(800.0, 1080.0);
        assert_eq!(
            arbiter.arbitrate(PointerEvent::new(
                hover_at,
                PointerPhase::Hover,
                Pointer::Pen,
                ContactId::new(0)
            )),
            Verdict::App
        );
        // No `Down` was ever recorded for this contact, so a later event
        // referencing it is unarbitrated and passes straight through too.
        assert_eq!(
            arbiter.arbitrate(PointerEvent::new(
                hover_at.offset(0.0, EDGE_SWIPE_MIN_TRAVEL * 4.0),
                PointerPhase::Moved,
                Pointer::Pen,
                ContactId::new(0)
            )),
            Verdict::App
        );
    }

    #[test]
    fn cancelling_a_contact_forgets_its_history() {
        let mut arbiter = detector();
        let origin = Point::new(800.0, 4.0);
        arbiter.arbitrate(touch(origin, PointerPhase::Down, 0));
        arbiter.arbitrate(touch(origin, PointerPhase::Cancelled, 0));

        // A fresh `Down` reusing the same id starts a clean history rather
        // than inheriting the cancelled one's origin.
        let elsewhere = Point::new(810.0, 1080.0);
        assert_eq!(
            arbiter.arbitrate(touch(elsewhere, PointerPhase::Down, 0)),
            Verdict::App
        );
    }
}
