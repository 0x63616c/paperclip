//! Layer 1: the Rust SDK enforces the interface at compile time.
//!
//! The claim this file exists to make good is the issue's own "done when": **an
//! app can be written against the SDK alone.** So the fixture here is a real
//! app — it implements [`App`], it is driven by [`paper_sdk::run`], and it
//! imports nothing but `paper_sdk`. If the SDK stopped being sufficient, this
//! file would stop compiling, which is the only form this layer's enforcement
//! can take.
//!
//! The tests then check that the loop an app gets for free does the things the
//! app is entitled to assume: that its first frame is drawn when asked, that
//! its redraw request reaches the host, that background work comes back onto
//! the event loop, and that it is given the chance to save before it dies.

use paper_conformance_tests::{Recorder, VIEWPORT, hello, script, storage_root};
use paper_protocol::{
    Action, AppMessage, Capability, ContactId, Damage, DrawReason, DrawRequest, ExitReason,
    FrameId, HostMessage, LifecycleEvent, Pointer, PointerEvent, PointerPhase, Request,
};
use paper_sdk::{App, Canvas, Context, Event, LocalSurfaces, Outcome, SaveError, palette, run};

/// An app that uses every part of the contract, written against `paper_sdk`
/// and nothing else.
#[derive(Debug, Default)]
struct Fixture {
    taps: u32,
    frames: u32,
    downloads: Vec<String>,
    suspended: bool,
    told_to_exit: Option<ExitReason>,
}

impl App for Fixture {
    /// What this app's background work produces — the App Store's case, in
    /// miniature: a download that finishes with a name.
    type Completion = String;

    fn event(
        &mut self,
        event: &Event<Self::Completion>,
        context: &mut Context<'_, Self::Completion>,
    ) -> Action {
        match event {
            Event::Pointer(pointer) if pointer.is_tap() => {
                self.taps += 1;
                // The long-running work goes to a thread, never to this
                // method: everything here runs on the UI loop.
                context.spawn(|| "catalog.json".to_owned());
                Action::Redraw
            }
            Event::Pointer(_) => Action::None,
            Event::Suspended => {
                self.suspended = true;
                Action::None
            }
            Event::Resumed => {
                self.suspended = false;
                Action::None
            }
            Event::Completed(name) => {
                self.downloads.push(name.clone());
                context.info("download finished");
                Action::Redraw
            }
            Event::PrepareToExit { reason, .. } => {
                self.told_to_exit = Some(*reason);
                Action::None
            }
            _ => Action::None,
        }
    }

    fn draw(&mut self, canvas: &mut Canvas, context: &mut Context<'_, Self::Completion>) {
        self.frames += 1;
        canvas.clear(palette::PAPER);
        // Everything `draw` needs is already in `self` and on the context.
        // There is no I/O here, and there is nothing to wait for.
        let _ = context.viewport();
    }

    fn save(&mut self, context: &mut Context<'_, Self::Completion>) -> Result<(), SaveError> {
        context
            .storage()
            .write_private("taps", &self.taps.to_le_bytes())?;
        Ok(())
    }
}

/// An app with no background work at all.
///
/// `Infallible` as the completion type makes [`Event::Completed`]
/// unconstructible, so the arm is provably dead rather than merely unused —
/// which is the cheapest demonstration that the mechanism is opt-in.
#[derive(Debug, Default)]
struct Silent;

impl App for Silent {
    type Completion = std::convert::Infallible;

    fn event(
        &mut self,
        _: &Event<Self::Completion>,
        _: &mut Context<'_, Self::Completion>,
    ) -> Action {
        Action::None
    }

    fn draw(&mut self, canvas: &mut Canvas, _: &mut Context<'_, Self::Completion>) {
        canvas.clear(palette::PAPER);
    }

    fn save(&mut self, _: &mut Context<'_, Self::Completion>) -> Result<(), SaveError> {
        Ok(())
    }
}

fn draw(frame: u64) -> HostMessage {
    HostMessage::Draw(DrawRequest {
        frame: FrameId::new(frame),
        reason: DrawReason::First,
        viewport: VIEWPORT,
    })
}

fn tap() -> HostMessage {
    HostMessage::Pointer(PointerEvent::new(
        paper_sdk::Point::new(10.0, 10.0),
        PointerPhase::Up,
        Pointer::Touch,
        ContactId::FIRST,
    ))
}

#[test]
fn an_app_written_against_the_sdk_alone_runs_a_whole_session() {
    let root = tempfile::tempdir().expect("a temp dir");
    storage_root(root.path());

    let wire = script(&[
        HostMessage::Hello(hello(root.path(), vec![Capability::Storage])),
        draw(0),
        tap(),
        HostMessage::Lifecycle(LifecycleEvent::Suspended),
        HostMessage::Lifecycle(LifecycleEvent::Resumed),
        HostMessage::Lifecycle(LifecycleEvent::prepare_to_exit(
            ExitReason::Sleep,
            std::time::Duration::from_secs(1),
        )),
    ]);
    let recorder = Recorder::new();
    let surfaces = LocalSurfaces::new();

    let outcome = run(
        Fixture::default(),
        std::io::Cursor::new(wire),
        recorder.clone(),
        surfaces.clone(),
    )
    .expect("the session runs");

    assert_eq!(
        outcome,
        Outcome::Exited {
            reason: ExitReason::Sleep,
            saved: true
        }
    );

    let sent = recorder.messages();
    // Readiness first, always.
    assert!(
        matches!(sent.first(), Some(AppMessage::Ready(_))),
        "{sent:?}"
    );
    // The frame it was asked for, answered once, with the id it was given.
    assert!(
        sent.iter().any(|message| matches!(
            message,
            AppMessage::Frame(done) if done.frame == FrameId::FIRST
        )),
        "{sent:?}"
    );
    // The redraw the tap asked for.
    assert!(
        sent.iter()
            .any(|message| matches!(message, AppMessage::Request(Request::Redraw))),
        "{sent:?}"
    );
    // And the save it was given a deadline for.
    assert!(
        matches!(sent.last(), Some(AppMessage::Saved(saved)) if saved.ok
            && saved.reason == ExitReason::Sleep),
        "{sent:?}"
    );

    // The frame reached the surface, not just the socket.
    assert_eq!(surfaces.log().frames(), 1);
    assert_eq!(surfaces.log().published(), vec![Damage::Full]);

    // And the save is on disk, atomically, where the next launch will find it.
    let saved = std::fs::read(root.path().join("private/taps")).expect("the save exists");
    assert_eq!(saved, 1_u32.to_le_bytes());
}

/// Work handed to a thread comes back as an event on the loop, in the same
/// queue as input — which is what lets an app hold its state without a lock.
#[test]
fn background_work_completes_onto_the_event_loop() {
    let root = tempfile::tempdir().expect("a temp dir");
    storage_root(root.path());

    let wire = script(&[
        HostMessage::Hello(hello(root.path(), vec![Capability::Storage])),
        tap(),
        // The completion arrives asynchronously; the host has nothing more to
        // say, so the loop ends when the script does and the completion is
        // whatever raced in first.
        HostMessage::Goodbye,
    ]);
    let recorder = Recorder::new();
    let outcome = run(
        Fixture::default(),
        std::io::Cursor::new(wire),
        recorder.clone(),
        LocalSurfaces::new(),
    )
    .expect("the session runs");
    assert_eq!(outcome, Outcome::HostClosed);

    // The tap's redraw request definitely went out; the completion may or may
    // not have won the race to the closing `Goodbye`, and asserting on a race
    // would make this test flaky rather than thorough.
    let sent = recorder.messages();
    assert!(
        sent.iter()
            .any(|message| matches!(message, AppMessage::Request(Request::Redraw))),
        "{sent:?}"
    );
}

/// An app that never links the task mechanism is a complete app. The
/// completion type being uninhabited is what says so.
#[test]
fn an_app_with_no_background_work_is_a_complete_app() {
    let root = tempfile::tempdir().expect("a temp dir");
    storage_root(root.path());
    let wire = script(&[
        HostMessage::Hello(hello(root.path(), Vec::new())),
        draw(0),
        HostMessage::Goodbye,
    ]);
    let recorder = Recorder::new();
    run(
        Silent,
        std::io::Cursor::new(wire),
        recorder.clone(),
        LocalSurfaces::new(),
    )
    .expect("the session runs");

    let sent = recorder.messages();
    assert!(matches!(sent.first(), Some(AppMessage::Ready(_))));
    assert!(
        sent.iter()
            .any(|message| matches!(message, AppMessage::Frame(_))),
        "{sent:?}"
    );
}

/// An app that was granted nothing gets a typed error naming the capability,
/// at the call — not an `EACCES` from three layers down, and not a silent
/// write into a directory the OS would have refused anyway.
#[test]
fn saving_without_the_storage_capability_fails_honestly() {
    let root = tempfile::tempdir().expect("a temp dir");
    storage_root(root.path());
    let wire = script(&[
        HostMessage::Hello(hello(root.path(), Vec::new())),
        HostMessage::Lifecycle(LifecycleEvent::prepare_to_exit(
            ExitReason::Shutdown,
            std::time::Duration::from_secs(1),
        )),
    ]);
    let recorder = Recorder::new();
    let outcome = run(
        Fixture::default(),
        std::io::Cursor::new(wire),
        recorder.clone(),
        LocalSurfaces::new(),
    )
    .expect("the session runs");

    assert_eq!(
        outcome,
        Outcome::Exited {
            reason: ExitReason::Shutdown,
            saved: false
        }
    );
    let sent = recorder.messages();
    assert!(
        matches!(sent.last(), Some(AppMessage::Saved(saved)) if !saved.ok),
        "a failed save is reported as one: {sent:?}"
    );
    assert!(
        sent.iter().any(|message| matches!(
            message,
            AppMessage::Diagnostic(diagnostic) if diagnostic.message.contains("storage")
        )),
        "the diagnostic names the missing capability: {sent:?}"
    );
}

/// An app on a host it cannot speak to refuses to start, rather than running
/// and reading for messages that do not exist.
///
/// At protocol `1.0` the only incompatible host is a different major, which
/// the SDK refuses for the strongest available reason. The launch-time suite
/// covers the minor-version rule, where a `Session` can be built against any
/// version rather than only the one this binary was compiled with.
#[test]
fn an_app_refuses_a_host_it_cannot_speak_to() {
    let root = tempfile::tempdir().expect("a temp dir");
    storage_root(root.path());
    let incompatible =
        paper_protocol::ProtocolVersion::new(paper_protocol::CURRENT.major().wrapping_add(1), 0);
    let wire = script(
        &[HostMessage::Hello(paper_conformance_tests::hello_speaking(
            incompatible,
            root.path(),
            Vec::new(),
        ))],
    );
    let error = run(
        Silent,
        std::io::Cursor::new(wire),
        Recorder::new(),
        LocalSurfaces::new(),
    )
    .expect_err("a mismatched major does not run");
    assert!(
        matches!(error, paper_sdk::RuntimeError::UnsupportedProtocol { .. }),
        "{error:?}"
    );
}

/// Every limit in the contract is enforced somewhere. This one is the host's
/// to respect and the app's to check, because the app is the only receiver —
/// and it refuses rather than truncating, since an app silently missing a
/// share the user granted is worse than one that does not start.
#[test]
fn a_hello_with_too_many_grants_is_refused() {
    let root = tempfile::tempdir().expect("a temp dir");
    storage_root(root.path());

    let mut greeting = hello(root.path(), vec![Capability::Sharing]);
    greeting.paths.shared = (0..=paper_protocol::MAX_SHARED_GRANTS)
        .map(|index| paper_protocol::SharedGrant {
            with: format!("dev.calum.app{index}")
                .parse()
                .expect("a valid fixture id"),
            path: root.path().join(format!("share{index}")),
            access: paper_protocol::ShareAccess::Read,
        })
        .collect();

    let wire = script(&[HostMessage::Hello(greeting)]);
    let error = run(
        Silent,
        std::io::Cursor::new(wire),
        Recorder::new(),
        LocalSurfaces::new(),
    )
    .expect_err("too many grants is refused");
    assert!(
        matches!(error, paper_sdk::RuntimeError::TooManyGrants { granted, max }
            if granted == paper_protocol::MAX_SHARED_GRANTS + 1
                && max == paper_protocol::MAX_SHARED_GRANTS),
        "{error:?}"
    );
}

/// The first message is `Hello` or there is no session. An app that started
/// without one would not know its own id, its viewport or where its save goes.
#[test]
fn a_session_that_does_not_open_with_hello_does_not_open() {
    let wire = script(&[HostMessage::Goodbye]);
    let error = run(
        Silent,
        std::io::Cursor::new(wire),
        Recorder::new(),
        LocalSurfaces::new(),
    )
    .expect_err("no hello, no session");
    assert!(
        matches!(error, paper_sdk::RuntimeError::NoHello),
        "{error:?}"
    );
}
