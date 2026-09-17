//! Layer 4: an app that ignores the SDK entirely still gets nowhere.
//!
//! The first three layers can all be skipped. An app does not have to link
//! `paper_sdk`; a package does not have to go through `paperctl check`; a
//! binary can be copied onto the tablet by hand. This layer is what is left
//! when all of that is assumed away: a process with a socket, writing whatever
//! it likes.
//!
//! So nothing in this file calls an SDK function. Every case builds bytes and
//! feeds them to the host's own reader, which is the code that will actually
//! be on the far side of that socket.
//!
//! The other half of this layer — the OS restrictions that stop an app doing
//! things which never touch the socket at all — is `paper_host`'s, and it has
//! its own tests: `SessionGrants::derive` turns the same `GrantedCapabilities`
//! into the paths a session may write, and the failure harness proves systemd
//! enforces it. What *is* tested here is that the numbers both sides enforce
//! come from one place, so the process the supervisor kills and the SDK that
//! told the app it was fine cannot disagree.

use paper_protocol::{
    AppMessage, CURRENT, CodecError, Diagnostic, DiagnosticLevel, DrawReason, ExitReason,
    MAX_DIAGNOSTIC_BYTES, MAX_MESSAGE_BYTES, Ready, Session, SessionId, Size, State, Violation,
    codec,
};

const VIEWPORT: Size = Size::new(1620, 2160);

/// Frames raw bytes the way the protocol does, without encoding a message.
///
/// This is the hostile app's job, done here so the tests can do it wrong on
/// purpose.
fn frame(body: &[u8]) -> Vec<u8> {
    let mut wire = (body.len() as u32).to_le_bytes().to_vec();
    wire.extend_from_slice(body);
    wire
}

/// Reads one message the way a host does.
fn read(wire: &[u8]) -> Result<AppMessage, CodecError> {
    codec::read_message::<_, AppMessage>(&mut { wire })
}

/// A length prefix that promises four gigabytes.
///
/// The check has to happen on the prefix, before the allocation: if it moved
/// after the `vec![0; len]` this test would exhaust memory rather than fail,
/// which is the failure mode it exists to prevent.
#[test]
fn a_hostile_length_prefix_costs_four_bytes_and_a_rejection() {
    let mut wire = u32::MAX.to_le_bytes().to_vec();
    wire.extend_from_slice(b"{}");
    let error = read(&wire).unwrap_err();
    assert!(
        matches!(error, CodecError::TooLarge { len, max }
            if len == u64::from(u32::MAX) && max == MAX_MESSAGE_BYTES),
        "{error:?}"
    );
}

/// A body that is genuinely as long as it claims, and still over the limit.
#[test]
fn an_oversized_message_is_refused_however_honestly_it_is_framed() {
    let body = vec![b' '; MAX_MESSAGE_BYTES + 1];
    let error = read(&frame(&body)).unwrap_err();
    assert!(matches!(error, CodecError::TooLarge { .. }), "{error:?}");

    // And a message at the limit is refused only for what it is, not its size.
    let body = vec![b' '; MAX_MESSAGE_BYTES];
    assert!(matches!(read(&frame(&body)), Err(CodecError::Malformed(_))));
}

/// Well-framed rubbish is rubbish. Being the right length does not make bytes
/// a message, and neither does having a tag the host recognises.
#[test]
fn nothing_becomes_a_message_by_being_well_framed() {
    for body in [
        &b"{}"[..],
        &b"not json"[..],
        &br#"{"type":"invented"}"#[..],
        &br#"{"type":"ready"}"#[..],
        &br#"{"type":"ready","protocol":"1"}"#[..],
        &br#"{"type":"hello"}"#[..], // a host message, from an app
        &br#"["ready"]"#[..],
        &[0xff, 0xfe, 0xfd][..], // not even UTF-8
    ] {
        assert!(
            matches!(read(&frame(body)), Err(CodecError::Malformed(_))),
            "accepted {:?}",
            String::from_utf8_lossy(body)
        );
    }
}

/// A connection that stops mid-frame yields a truncation, not a half-read
/// value the host might act on.
#[test]
fn a_connection_that_dies_mid_frame_yields_nothing() {
    let mut wire = Vec::new();
    codec::write_message(&mut wire, &AppMessage::Ready(Ready { protocol: CURRENT })).unwrap();
    for cut in 1..wire.len() {
        let partial = &wire[..cut];
        assert!(
            matches!(read(partial), Err(CodecError::Truncated { .. })),
            "a {cut}-byte prefix decoded as something"
        );
    }
}

/// A clean hang-up between frames is not an error. It is what an app exiting
/// looks like from the reading side, and a host that treated it as a fault
/// would log one on every normal exit.
#[test]
fn a_clean_hangup_is_not_a_failure() {
    assert!(matches!(read(&[]), Err(CodecError::Closed)));
}

/// The whole layer, end to end: a process that never linked the SDK, writing
/// plausible bytes by hand, is stopped by the host — first by the codec for
/// what is not a message, then by the session for what is a message but not
/// allowed.
#[test]
fn a_hand_written_app_is_held_to_the_same_contract() {
    let mut session = Session::greeting(SessionId::new(7), CURRENT, VIEWPORT);

    // It opens by asking for a redraw, which is not how a session opens.
    let mut wire = Vec::new();
    codec::write_message(
        &mut wire,
        &AppMessage::Request(paper_protocol::Request::Redraw),
    )
    .unwrap();
    let message = read(&wire).expect("it is a well-formed message");
    assert!(matches!(
        session.on_app_message(&message),
        Err(Violation::BeforeReady { message: "request" })
    ));
    assert_eq!(session.state(), State::Closed);
}

/// A hand-written app that gets the handshake right is *allowed* to continue —
/// the layer refuses misbehaviour, not the absence of the SDK. Anyone may
/// write an app in any language against this protocol.
#[test]
fn a_hand_written_app_that_conforms_is_a_conforming_app() {
    let mut session = Session::greeting(SessionId::new(8), CURRENT, VIEWPORT);

    let body = format!(r#"{{"type":"ready","protocol":"{CURRENT}"}}"#);
    let message = read(&frame(body.as_bytes())).expect("hand-written JSON is a message");
    session
        .on_app_message(&message)
        .expect("a correct handshake is a correct handshake");
    assert_eq!(session.state(), State::Running);

    let draw = session.draw(DrawReason::First).expect("it can be drawn");
    let paper_protocol::HostMessage::Draw(request) = draw else {
        panic!("expected a draw request");
    };
    let body = format!(
        r#"{{"type":"frame","frame":{},"damage":{{"damage":"full"}}}}"#,
        request.frame.get()
    );
    let message = read(&frame(body.as_bytes())).expect("hand-written frame is a message");
    session
        .on_app_message(&message)
        .expect("a correct frame is a correct frame");
}

/// The limits the supervisor enforces are the limits the SDK enforces, from
/// the same constants. If these could drift, a conforming app would be killed
/// for a message its own SDK told it was fine to send.
#[test]
fn every_limit_has_exactly_one_definition() {
    // The diagnostic limit: the SDK truncates at it, the host refuses past it.
    let truncated = Diagnostic::new(DiagnosticLevel::Info, "x".repeat(MAX_DIAGNOSTIC_BYTES * 2));
    assert!(truncated.message.len() <= MAX_DIAGNOSTIC_BYTES);

    let mut session = Session::greeting(SessionId::new(9), CURRENT, VIEWPORT);
    session
        .on_app_message(&AppMessage::Ready(Ready { protocol: CURRENT }))
        .unwrap();
    session
        .on_app_message(&AppMessage::Diagnostic(truncated))
        .expect("what the SDK produces is what the host accepts");

    // The message limit: the encoder refuses to produce what the reader would
    // refuse to accept, so there is no message that is legal to send and fatal
    // to receive.
    let over = vec![0u8; MAX_MESSAGE_BYTES + 1];
    assert!(matches!(
        codec::encode(&over),
        Err(CodecError::TooLarge { .. })
    ));

    // The deadlines: values both sides read, with a stated floor.
    assert!(paper_protocol::MIN_EXIT_DEADLINE <= paper_protocol::EXIT_DEADLINE);
    assert!(paper_protocol::FRAME_DEADLINE < paper_protocol::READY_DEADLINE);
}

/// A prepare-to-exit deadline is a number in the message, so an app can size
/// its save path against the budget it was actually given rather than against
/// a convention.
#[test]
fn the_exit_deadline_travels_with_the_message() {
    let mut session = Session::greeting(SessionId::new(10), CURRENT, VIEWPORT);
    session
        .on_app_message(&AppMessage::Ready(Ready { protocol: CURRENT }))
        .unwrap();

    let message = session
        .prepare_to_exit(ExitReason::Sleep, std::time::Duration::from_millis(750))
        .expect("a running app can be asked to save");
    let paper_protocol::HostMessage::Lifecycle(event) = message else {
        panic!("expected a lifecycle event");
    };
    assert_eq!(
        event.deadline(),
        Some(std::time::Duration::from_millis(750))
    );

    // And it survives the wire, because that is where the app reads it.
    let mut wire = Vec::new();
    codec::write_message(&mut wire, &paper_protocol::HostMessage::Lifecycle(event)).unwrap();
    let back: paper_protocol::HostMessage =
        codec::read_message(&mut wire.as_slice()).expect("it decodes");
    let paper_protocol::HostMessage::Lifecycle(event) = back else {
        panic!("expected a lifecycle event");
    };
    assert_eq!(
        event.deadline(),
        Some(std::time::Duration::from_millis(750))
    );
}
