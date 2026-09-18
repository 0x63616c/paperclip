//! Counter as a real [`App`] (§8).
//!
//! [`CounterScreen::press`] holds the one decision this app makes; this is the
//! wire adapter, plus the one thing that needs a [`Context`]: persistence.

use std::convert::Infallible;

use paper_sdk::{Action, App, Canvas, Context, Event, SaveError};

use crate::screen::CounterScreen;

/// The name [`CounterScreen::count`] is saved under, inside the app's own
/// private storage. Not a path: [`paper_sdk::Storage`] resolves names
/// relative to the directory the host granted.
const SAVE_FILE: &str = "count";

/// The Counter app.
#[derive(Debug, Default)]
pub struct CounterApp {
    screen: CounterScreen,
    /// Whether the saved count has been loaded yet.
    ///
    /// Loading needs [`Context::storage`], and the only callback guaranteed
    /// to run before the first frame is [`App::draw`] itself — there is no
    /// `Event::Launched`. `draw`'s own contract says "no filesystem"; this is
    /// the one deliberate, one-time exception to it, guarded so it happens
    /// on the first call and never again — the same pattern
    /// `paper_chess::ChessApp` documents at more length.
    loaded: bool,
}

impl CounterApp {
    /// A fresh, unloaded app. The count is `0` until [`Self::load`] runs, or
    /// forever if there was nothing to load.
    pub fn new() -> Self {
        Self::default()
    }

    fn load(&mut self, context: &mut Context<'_, Infallible>) {
        self.loaded = true;
        let Ok(Some(bytes)) = context.storage().read_private(SAVE_FILE) else {
            // No `storage` capability, or nothing saved yet: nothing this
            // app can do except start from zero.
            return;
        };
        if let Ok(text) = std::str::from_utf8(&bytes)
            && let Ok(count) = text.parse()
        {
            self.screen.count = count;
        }
    }

    fn persist(&mut self, context: &mut Context<'_, Infallible>) -> Result<(), SaveError> {
        context
            .storage()
            .write_private(SAVE_FILE, self.screen.count.to_string().as_bytes())?;
        Ok(())
    }
}

impl App for CounterApp {
    // Counter does no background work of its own.
    type Completion = Infallible;

    fn event(
        &mut self,
        event: &Event<Self::Completion>,
        context: &mut Context<'_, Self::Completion>,
    ) -> Action {
        if !self.loaded {
            self.load(context);
        }
        let Event::Pointer(pointer) = event else {
            return Action::None;
        };
        // Pure and cheap: no canvas, no drawn frame to wait for. A tap is
        // hit-testable the instant this app exists — see
        // `crate::screen::layout`'s own doc.
        let layout = crate::screen::layout(context.viewport());
        let count_before = self.screen.count;
        let action = self.screen.press(&layout, pointer);
        if self.screen.count != count_before
            && let Err(error) = self.persist(context)
        {
            context.warn(format!("could not save the count: {error}"));
        }
        action
    }

    fn draw(&mut self, canvas: &mut Canvas, context: &mut Context<'_, Self::Completion>) {
        if !self.loaded {
            self.load(context);
        }
        crate::screen::render(canvas, &self.screen);
    }

    fn save(&mut self, context: &mut Context<'_, Self::Completion>) -> Result<(), SaveError> {
        self.persist(context)
    }
}

#[cfg(test)]
mod tests {
    use super::CounterApp;
    use paper_protocol::{
        AppPaths, Capability, ContactId, LaunchReason, PixelFormat, Pointer, PointerEvent,
        PointerPhase, SessionId, SurfaceDescriptor,
    };
    use paper_sdk::{LocalSurfaces, Outcome, run};
    use std::os::unix::net::UnixStream;

    /// Builds the paths a real host would grant, having already created them
    /// — as the host does before launch.
    fn app_paths(root: &std::path::Path) -> AppPaths {
        let private = root.join("private");
        std::fs::create_dir_all(&private).expect("a private directory");
        AppPaths {
            assets: root.join("assets"),
            private,
            temp: root.join("temp"),
            shared: Vec::new(),
        }
    }

    type Session = (
        UnixStream,
        UnixStream,
        std::thread::JoinHandle<Result<Outcome, paper_sdk::RuntimeError>>,
    );

    /// Opens a real session over a real socket pair — the same
    /// `paper_sdk::run` loop a launched process runs.
    fn start_session(root: &std::path::Path) -> Session {
        let (host_side, app_side) = UnixStream::pair().expect("a socket pair");
        let reader = app_side.try_clone().expect("clone for reading");
        let writer = app_side;
        let handle = std::thread::spawn(move || {
            run(CounterApp::new(), reader, writer, LocalSurfaces::new())
        });

        let mut host_writer = host_side.try_clone().expect("clone for writing");
        let hello = paper_protocol::Hello {
            protocol: paper_protocol::CURRENT,
            session: SessionId::new(1),
            app: "dev.calum.counter".parse().unwrap(),
            version: "0.1.0".parse().unwrap(),
            launch: LaunchReason::Fresh,
            surface: SurfaceDescriptor::packed(paper_sdk::SCREEN, PixelFormat::Argb8888),
            capabilities: vec![Capability::Storage],
            paths: app_paths(root),
        };
        paper_protocol::codec::write_message(
            &mut host_writer,
            &paper_protocol::HostMessage::Hello(hello),
        )
        .unwrap();

        let mut host_reader = host_side;
        let _: paper_protocol::AppMessage =
            paper_protocol::codec::read_message(&mut host_reader).unwrap();
        (host_reader, host_writer, handle)
    }

    fn send(
        host_reader: &mut UnixStream,
        host_writer: &mut UnixStream,
        event: &paper_protocol::HostMessage,
    ) -> paper_protocol::AppMessage {
        paper_protocol::codec::write_message(host_writer, event).unwrap();
        paper_protocol::codec::read_message(host_reader).unwrap()
    }

    fn finish_session(
        mut host_reader: UnixStream,
        mut host_writer: UnixStream,
        handle: std::thread::JoinHandle<Result<Outcome, paper_sdk::RuntimeError>>,
    ) {
        let deadline = paper_protocol::LifecycleEvent::prepare_to_exit(
            paper_protocol::ExitReason::ReturnToStock,
            std::time::Duration::from_secs(5),
        );
        let saved = send(
            &mut host_reader,
            &mut host_writer,
            &paper_protocol::HostMessage::Lifecycle(deadline),
        );
        assert!(
            matches!(
                saved,
                paper_protocol::AppMessage::Saved(paper_protocol::Saved { ok: true, .. })
            ),
            "expected a successful save, got {saved:?}"
        );
        match handle.join().expect("the session thread did not panic") {
            Ok(Outcome::Exited { saved: true, .. }) => {}
            other => panic!("expected a saved exit, got {other:?}"),
        }
    }

    fn tap_at(at: paper_protocol::Point) -> paper_protocol::HostMessage {
        paper_protocol::HostMessage::Pointer(PointerEvent::new(
            at,
            PointerPhase::Up,
            Pointer::Touch,
            ContactId::FIRST,
        ))
    }

    #[test]
    fn a_count_from_one_session_is_there_in_the_next() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let layout = crate::screen::layout(paper_sdk::SCREEN);
        let count_button = layout.count.center();

        let (mut host_reader, mut host_writer, handle) = start_session(dir.path());
        send(&mut host_reader, &mut host_writer, &tap_at(count_button));
        send(&mut host_reader, &mut host_writer, &tap_at(count_button));
        finish_session(host_reader, host_writer, handle);

        let saved = std::fs::read_to_string(dir.path().join("private").join("count"))
            .expect("a save file exists");
        assert_eq!(saved, "2");

        let (mut host_reader, mut host_writer, handle) = start_session(dir.path());
        send(
            &mut host_reader,
            &mut host_writer,
            &paper_protocol::HostMessage::Draw(paper_protocol::DrawRequest {
                frame: paper_protocol::FrameId::new(1),
                reason: paper_protocol::DrawReason::First,
                viewport: paper_sdk::SCREEN,
            }),
        );
        send(&mut host_reader, &mut host_writer, &tap_at(count_button));
        finish_session(host_reader, host_writer, handle);

        let saved =
            std::fs::read_to_string(dir.path().join("private").join("count")).expect("still there");
        assert_eq!(saved, "3", "the second session resumed the first's count");
    }

    /// The bug WWW-51 removed from five other apps: `layout: Option<Layout>`
    /// left a tap dead until the first `draw` completed. A tap here lands
    /// before this session has ever been asked to draw a frame, and still
    /// counts.
    #[test]
    fn a_tap_before_any_draw_still_counts() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let layout = crate::screen::layout(paper_sdk::SCREEN);

        let (mut host_reader, mut host_writer, handle) = start_session(dir.path());
        let reply = send(
            &mut host_reader,
            &mut host_writer,
            &tap_at(layout.count.center()),
        );
        assert!(
            matches!(
                reply,
                paper_protocol::AppMessage::Request(paper_protocol::Request::Redraw)
            ),
            "a tap before any draw must still ask for a redraw, got {reply:?}"
        );
        finish_session(host_reader, host_writer, handle);

        let saved = std::fs::read_to_string(dir.path().join("private").join("count"))
            .expect("a save file exists");
        assert_eq!(saved, "1");
    }
}
