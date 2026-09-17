//! Layer 3: the host refuses a binary that starts and then misbehaves.
//!
//! This layer runs in the host process, against a connection, so it is the
//! first one an app cannot skip by not linking something. Everything here
//! drives [`Session`] directly — the same type the supervisor will drive
//! (WWW-4) — because that is the enforcement, not a model of it.
//!
//! The hostile cases §8 names: unsupported protocol, missing readiness,
//! oversized messages, illegal lifecycle transitions. Plus the one that is
//! structural rather than checked: an app cannot claim an identity, because
//! there is no field in any message it can send to put one in.

use paper_protocol::{
    AppMessage, CURRENT, Capability, Damage, Diagnostic, DiagnosticLevel, DrawReason, ExitReason,
    FrameDone, FrameId, HostFault, HostMessage, MAX_DAMAGE_RECTS, MAX_DIAGNOSTIC_BYTES,
    ProtocolVersion, Ready, Rect, Request, Saved, Session, SessionId, Size, State, Violation,
};

const VIEWPORT: Size = Size::new(1620, 2160);

fn session() -> Session {
    Session::greeting(SessionId::new(0xabc), CURRENT, VIEWPORT)
}

fn ready() -> AppMessage {
    AppMessage::Ready(Ready { protocol: CURRENT })
}

fn running() -> Session {
    let mut session = session();
    session
        .on_app_message(&ready())
        .expect("readiness opens the session");
    session
}

fn issued_frame(message: &HostMessage) -> FrameId {
    match message {
        HostMessage::Draw(request) => request.frame,
        other => panic!("expected a draw request, got {other:?}"),
    }
}

/// The happy path, so the refusals below are refusals of something specific.
#[test]
fn a_conforming_session_runs_start_to_finish() {
    let mut session = running();

    let draw = session.draw(DrawReason::First).expect("ready apps draw");
    let frame = issued_frame(&draw);
    session
        .on_app_message(&AppMessage::Frame(FrameDone {
            frame,
            damage: Damage::Full,
        }))
        .expect("the frame it was asked for");
    session
        .on_app_message(&AppMessage::Request(Request::Redraw))
        .expect("an app may ask to be drawn again");

    session.suspend().expect("suspending a running app");
    session.resume().expect("resuming a suspended app");

    session
        .prepare_to_exit_default(ExitReason::SwitchedAway)
        .expect("asking a running app to save");
    session
        .on_app_message(&AppMessage::Saved(Saved {
            reason: ExitReason::SwitchedAway,
            ok: true,
        }))
        .expect("the save it was asked for");
    assert_eq!(session.state(), State::Closed);
}

// --- missing readiness ----------------------------------------------------

/// Missing readiness: an app that starts talking without saying its surface is
/// mapped. Every message but a diagnostic is refused, and the session ends.
#[test]
fn an_app_that_never_reports_readiness_gets_nowhere() {
    for message in [
        AppMessage::Request(Request::Redraw),
        AppMessage::Frame(FrameDone {
            frame: FrameId::FIRST,
            damage: Damage::Full,
        }),
        AppMessage::Saved(Saved {
            reason: ExitReason::Sleep,
            ok: true,
        }),
    ] {
        let mut session = session();
        assert!(
            matches!(
                session.on_app_message(&message),
                Err(Violation::BeforeReady { .. })
            ),
            "accepted {message:?} before readiness"
        );
        assert_eq!(session.state(), State::Closed);
    }
}

/// The host cannot ask an app that has not reported readiness to draw, either.
/// The rule holds in both directions, from one place.
#[test]
fn the_host_cannot_draw_an_app_that_is_not_ready() {
    let mut session = session();
    assert!(matches!(
        session.draw(DrawReason::First),
        Err(HostFault::WrongState {
            state: State::Greeting
        })
    ));
}

/// Readiness has a budget, and both sides read it from the same constant.
#[test]
fn the_readiness_deadline_is_a_value_not_a_convention() {
    assert_eq!(session().ready_deadline(), paper_protocol::READY_DEADLINE);
    assert!(paper_protocol::READY_DEADLINE > std::time::Duration::ZERO);
}

// --- unsupported protocol -------------------------------------------------

/// The manifest said `1.0`; the binary says otherwise. The manifest is a text
/// file next to the binary and nothing makes them agree, so the version that
/// gets enforced is the one the running process reported.
#[test]
fn a_binary_that_contradicts_its_manifest_is_caught_on_the_connection() {
    let mut session = Session::greeting(SessionId::new(1), ProtocolVersion::new(1, 2), VIEWPORT);
    let error = session
        .on_app_message(&AppMessage::Ready(Ready {
            protocol: ProtocolVersion::new(1, 5),
        }))
        .unwrap_err();
    assert!(
        matches!(error, Violation::UnsupportedProtocol { app, host }
            if app == ProtocolVersion::new(1, 5) && host == ProtocolVersion::new(1, 2)),
        "{error:?}"
    );
    assert_eq!(session.state(), State::Closed);
}

/// A host may be newer than the app it runs, never older, and never a
/// different major. The rule is refused explicitly rather than negotiated
/// down to something both sides half-speak.
#[test]
fn protocol_compatibility_is_one_directional() {
    let host = ProtocolVersion::new(1, 3);
    for (app, runs) in [
        (ProtocolVersion::new(1, 0), true),
        (ProtocolVersion::new(1, 3), true),
        (ProtocolVersion::new(1, 4), false),
        (ProtocolVersion::new(0, 9), false),
        (ProtocolVersion::new(2, 0), false),
    ] {
        let mut session = Session::greeting(SessionId::new(1), host, VIEWPORT);
        let result = session.on_app_message(&AppMessage::Ready(Ready { protocol: app }));
        assert_eq!(result.is_ok(), runs, "host {host} running app {app}");
    }
}

// --- oversized messages ---------------------------------------------------

/// An app that bypassed the SDK's truncation. The SDK cuts a diagnostic at the
/// limit; reaching the host with a longer one means the SDK was not involved,
/// which is exactly the case this layer exists for.
#[test]
fn an_oversized_diagnostic_ends_the_session() {
    let mut session = running();
    let error = session
        .on_app_message(&AppMessage::Diagnostic(Diagnostic {
            level: DiagnosticLevel::Info,
            message: "x".repeat(MAX_DIAGNOSTIC_BYTES + 1),
        }))
        .unwrap_err();
    assert!(
        matches!(error, Violation::DiagnosticTooLong { len, max }
            if len == MAX_DIAGNOSTIC_BYTES + 1 && max == MAX_DIAGNOSTIC_BYTES),
        "{error:?}"
    );

    // One byte under is fine. The limit is a limit, not a suggestion in either
    // direction.
    let mut session = running();
    session
        .on_app_message(&AppMessage::Diagnostic(Diagnostic {
            level: DiagnosticLevel::Info,
            message: "x".repeat(MAX_DIAGNOSTIC_BYTES),
        }))
        .expect("a diagnostic at the limit is legal");
}

/// A damage list is bounded too. Past a handful of rectangles the host would
/// rather present the bounding box, so an unbounded list buys nothing and
/// costs an allocation per frame.
#[test]
fn an_unbounded_damage_list_ends_the_session() {
    let mut session = running();
    let frame = issued_frame(&session.draw(DrawReason::First).unwrap());
    let error = session
        .on_app_message(&AppMessage::Frame(FrameDone {
            frame,
            damage: Damage::Regions {
                regions: vec![Rect::new(0.0, 0.0, 1.0, 1.0); MAX_DAMAGE_RECTS + 1],
            },
        }))
        .unwrap_err();
    assert!(
        matches!(error, Violation::TooMuchDamage { count, max }
            if count == MAX_DAMAGE_RECTS + 1 && max == MAX_DAMAGE_RECTS),
        "{error:?}"
    );
}

// --- illegal lifecycle transitions ----------------------------------------

#[test]
fn readiness_is_reported_exactly_once() {
    let mut session = running();
    assert_eq!(
        session.on_app_message(&ready()),
        Err(Violation::AlreadyReady)
    );
}

#[test]
fn a_frame_nobody_asked_for_ends_the_session() {
    let mut session = running();
    let frame = issued_frame(&session.draw(DrawReason::First).unwrap());

    let mut stale = running();
    let _ = stale.draw(DrawReason::First).unwrap();
    assert!(matches!(
        stale.on_app_message(&AppMessage::Frame(FrameDone {
            frame: frame.next(),
            damage: Damage::Full,
        })),
        Err(Violation::UnknownFrame { .. })
    ));

    // Answering the same frame twice is the same problem seen a moment later.
    session
        .on_app_message(&AppMessage::Frame(FrameDone {
            frame,
            damage: Damage::Full,
        }))
        .expect("the first answer is fine");
    assert_eq!(
        session.on_app_message(&AppMessage::Frame(FrameDone {
            frame,
            damage: Damage::Full,
        })),
        Err(Violation::NoFrameOutstanding)
    );
}

#[test]
fn an_app_that_keeps_working_after_being_told_to_exit_is_cut_off() {
    let mut session = running();
    session
        .prepare_to_exit_default(ExitReason::Shutdown)
        .unwrap();
    for message in [
        AppMessage::Request(Request::Redraw),
        AppMessage::Frame(FrameDone {
            frame: FrameId::FIRST,
            damage: Damage::Full,
        }),
        ready(),
    ] {
        let mut exiting = running();
        exiting
            .prepare_to_exit_default(ExitReason::Shutdown)
            .unwrap();
        assert!(
            matches!(
                exiting.on_app_message(&message),
                Err(Violation::AfterExit { .. })
            ),
            "accepted {message:?} after prepare-to-exit"
        );
    }

    // A diagnostic on the way out is still legal: an app that cannot save
    // should be able to say so.
    session
        .on_app_message(&AppMessage::Diagnostic(Diagnostic::new(
            DiagnosticLevel::Error,
            "no room left to save",
        )))
        .expect("an exiting app may still report");
}

#[test]
fn a_save_must_answer_the_exit_it_was_asked_about() {
    let mut session = running();
    session
        .prepare_to_exit_default(ExitReason::ReturnToStock)
        .unwrap();
    assert!(matches!(
        session.on_app_message(&AppMessage::Saved(Saved {
            reason: ExitReason::AppUpdate,
            ok: true,
        })),
        Err(Violation::SavedWrongReason { .. })
    ));
}

#[test]
fn a_suspended_app_is_never_asked_to_draw() {
    let mut session = running();
    session.suspend().unwrap();
    assert!(matches!(
        session.draw(DrawReason::HostRequested),
        Err(HostFault::WrongState {
            state: State::Suspended
        })
    ));
}

/// Once an app has broken the protocol, nothing it says afterwards means
/// anything — so the session stays closed rather than recovering into a state
/// where the next message is evaluated on its own merits.
#[test]
fn a_violation_is_not_recoverable() {
    let mut session = session();
    assert!(
        session
            .on_app_message(&AppMessage::Request(Request::Redraw))
            .is_err()
    );
    assert!(matches!(
        session.on_app_message(&ready()),
        Err(Violation::AfterClose { .. })
    ));
}

// --- identity -------------------------------------------------------------

/// The structural check: an app cannot claim to be another app, because there
/// is no field in any message it can send that carries an app id.
///
/// This is asserted over the *encoded* form rather than the type, because the
/// type could grow a field and this test should fail when it does.
#[test]
fn nothing_an_app_can_send_carries_an_identity() {
    let messages = [
        ready(),
        AppMessage::Frame(FrameDone {
            frame: FrameId::FIRST,
            damage: Damage::Full,
        }),
        AppMessage::Request(Request::Home),
        AppMessage::Saved(Saved {
            reason: ExitReason::Sleep,
            ok: true,
        }),
        AppMessage::Diagnostic(Diagnostic::new(DiagnosticLevel::Info, "hello")),
    ];
    for message in messages {
        let json = serde_json::to_string(&message).expect("messages encode");
        for forbidden in ["\"app\"", "\"session\"", "\"id\"", "\"capabilit"] {
            assert!(
                !json.contains(forbidden),
                "{message:?} encodes {forbidden}: {json}"
            );
        }
    }
}

/// And the host is the one that says what an app holds. The capability list
/// travels outward only.
#[test]
fn capabilities_travel_from_the_host_to_the_app_and_not_back() {
    let json = serde_json::to_string(&HostMessage::Goodbye).unwrap();
    assert!(!json.contains("capabilit"), "{json}");

    // The one place a capability appears is in `Hello`, which only a host
    // sends.
    let sample = serde_json::to_string(&Capability::Network).unwrap();
    assert_eq!(sample, "\"network\"");
}
