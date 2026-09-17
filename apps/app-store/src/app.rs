//! The App Store as a real [`App`] (§16).
//!
//! [`AppStoreScreen`] and [`render`] hold every decision about what is shown
//! and what a press does; this is the wire adapter, plus the one thing that
//! needs a [`Context`]: handing a [`Request`] to a background thread and
//! folding what comes back — the final [`Outcome`], or a step along the way
//! — into the screen.
//!
//! Every [`StoreSource`] method blocks — `source.rs`'s own doc says so — so
//! none of them run inside `event` or `draw`. A tap starts a worker; nothing
//! here waits on it, and the input loop keeps reading while it runs (§8).

use std::sync::Arc;

use paper_packages::AppId;
use paper_packages::install::{Progress, Step};
use paper_sdk::{
    Action, App, Canvas, Completer, Context, Damage, Event, Point, PointerEvent, PointerPhase,
    Rect, SaveError,
};

use crate::render::{DetailLayout, ListLayout, StoreLayout, render};
use crate::screen::{AppStoreScreen, Outcome, Request, View};
use crate::source::{SourceError, StoreSource};

/// What a background worker reports back.
///
/// Progress is its own variant, separate from the final answer, because an
/// install reports several [`Step`]s before it is done and
/// [`Context::spawn`] only ever delivers one completion per call. For
/// [`Request::Install`] this app spawns its own thread instead and sends one
/// [`Self::Progress`] per step, then one [`Self::Done`] at the end — both
/// through the same [`Completer`], which tolerates exactly that.
#[derive(Debug, Clone)]
pub enum Completion {
    /// One step of an operation in flight.
    Progress {
        /// The word to show.
        step: String,
        /// The download fraction, when there is one.
        percent: Option<u8>,
    },
    /// The request finished, however it finished.
    Done(Outcome),
}

/// The App Store app.
#[derive(Debug)]
pub struct AppStoreApp {
    screen: AppStoreScreen,
    source: Arc<dyn StoreSource>,
    layout: Option<StoreLayout>,
    /// What the last state change actually touched, so [`App::damage`] can
    /// claim less than the whole panel when that is safe.
    ///
    /// Defaults to [`Damage::Full`], which is always correct and is what the
    /// very first frame needs — there is no previous frame to be tighter
    /// than.
    pending_damage: Damage,
}

impl AppStoreApp {
    /// Builds the App Store showing `screen`, over `source`.
    ///
    /// Both are supplied rather than built here, the same way
    /// `HomeApp::new` takes a pre-built `HomeScreen`: the initial survey is a
    /// blocking read (`source.rs`'s "everything here blocks"), and belongs to
    /// whoever is opening the session, not to the event loop.
    pub fn new(screen: AppStoreScreen, source: Arc<dyn StoreSource>) -> Self {
        Self {
            screen,
            source,
            layout: None,
            pending_damage: Damage::Full,
        }
    }

    fn press(&mut self, pointer: &PointerEvent, context: &mut Context<'_, Completion>) -> Action {
        if pointer.phase != PointerPhase::Up {
            return Action::None;
        }
        let Some(layout) = self.layout.clone() else {
            return Action::None;
        };
        match layout {
            StoreLayout::List(list) => self.press_list(&list, pointer.at, context),
            StoreLayout::Detail(detail) => self.press_detail(&detail, pointer.at, context),
        }
    }

    fn press_list(
        &mut self,
        list: &ListLayout,
        at: Point,
        context: &mut Context<'_, Completion>,
    ) -> Action {
        if list.refresh.contains(at) {
            // Nothing on screen changes at the moment of the tap: `begin`
            // does not track a "refreshing" state, only a per-app one. The
            // redraw comes later, when `Outcome::Refreshed` arrives.
            self.dispatch(Request::Refresh, context);
            return Action::None;
        }
        if let Some(index) = list.action_hit(at) {
            let Some(app) = self.screen.rows().get(index).map(|entry| entry.app.clone()) else {
                return Action::None;
            };
            let Some(request) = self.screen.primary_action(&app) else {
                return Action::None;
            };
            let row = list.rows.get(index).map(|row| row.body);
            return self.start(request, row, context);
        }
        if let Some(index) = list.row_hit(at) {
            let Some(app) = self.screen.rows().get(index).map(|entry| entry.app.clone()) else {
                return Action::None;
            };
            if let Some(request) = self.screen.open(&app) {
                self.dispatch(request, context);
            }
            // Opening a detail view redraws the whole screen, not one row.
            self.pending_damage = Damage::Full;
            return Action::Redraw;
        }
        Action::None
    }

    fn press_detail(
        &mut self,
        detail: &DetailLayout,
        at: Point,
        context: &mut Context<'_, Completion>,
    ) -> Action {
        if detail.back.contains(at) {
            self.screen.back();
            self.pending_damage = Damage::Full;
            return Action::Redraw;
        }
        let View::Detail(app) = self.screen.view().clone() else {
            return Action::None;
        };
        if detail.primary.is_some_and(|rect| rect.contains(at))
            && let Some(request) = self.screen.primary_action(&app)
        {
            return self.start(request, None, context);
        }
        if detail.rollback.is_some_and(|rect| rect.contains(at)) {
            return self.start(Request::Rollback { app }, None, context);
        }
        Action::None
    }

    /// Starts an install or a rollback: begins the operation on the screen,
    /// works out how much of the panel that actually changed, and hands the
    /// request to a worker.
    ///
    /// `row` is the list row's body, when the press was on one — the one
    /// case where the resulting damage can be tighter than the whole panel.
    /// A failure banner disappearing (`begin` always clears one) shifts
    /// every row under it, so that case still claims the whole panel; a
    /// press in the detail view has no such row to point at either.
    fn start(
        &mut self,
        request: Request,
        row: Option<Rect>,
        context: &mut Context<'_, Completion>,
    ) -> Action {
        let had_failure = self.screen.failure().is_some();
        if !self.screen.begin(&request) {
            return Action::None;
        }
        self.pending_damage = match (had_failure, row) {
            (false, Some(body)) => Damage::Regions {
                regions: vec![body],
            },
            _ => Damage::Full,
        };
        self.dispatch(request, context);
        Action::Redraw
    }

    /// Hands `request` to a worker thread. Never called from `draw`, and the
    /// worker never touches `self` — its answer comes back as
    /// [`Event::Completed`] and is folded in there, on this same thread.
    fn dispatch(&self, request: Request, context: &mut Context<'_, Completion>) {
        let source = Arc::clone(&self.source);
        match request {
            Request::Refresh => {
                context.spawn(move || {
                    Completion::Done(match source.inventory() {
                        Ok(inventory) => Outcome::Refreshed(Box::new(inventory)),
                        Err(error) => failed(None, &error),
                    })
                });
            }
            Request::Notes { app, version } => {
                context.spawn(move || {
                    Completion::Done(match source.notes(&app, &version) {
                        Ok(text) => Outcome::Notes { app, text },
                        Err(error) => failed(Some(app), &error),
                    })
                });
            }
            Request::Install { app, version } => {
                // Not `context.spawn`: that delivers exactly one completion,
                // and a download needs several — one `Progress` per `Step`,
                // then one `Done`. `Completer` tolerates being called more
                // than once, so this thread is spawned by hand instead.
                let completer = context.completer();
                std::thread::spawn(move || {
                    let reporter = Reporter::new(completer.clone());
                    let outcome = match source.install(&app, &version, &reporter) {
                        Ok(version) => Outcome::Installed { app, version },
                        Err(error) => failed(Some(app), &error),
                    };
                    let _ = completer.complete(Completion::Done(outcome));
                });
            }
            Request::Rollback { app } => {
                context.spawn(move || {
                    Completion::Done(match source.rollback(&app) {
                        Ok(version) => Outcome::RolledBack { app, version },
                        Err(error) => failed(Some(app), &error),
                    })
                });
            }
        }
    }

    /// The row the operation in flight belongs to, in the current layout —
    /// `None` when that is not answerable (the detail view is showing, the
    /// layout has not been drawn yet, or the row scrolled out since). Every
    /// `None` case falls back to [`Damage::Full`] in the caller, which is
    /// always correct.
    fn working_row(&self) -> Option<Rect> {
        let StoreLayout::List(list) = self.layout.as_ref()? else {
            return None;
        };
        let working = self.screen.working()?;
        let index = self
            .screen
            .rows()
            .iter()
            .position(|entry| entry.app == working.app)?;
        list.rows.get(index).map(|row| row.body)
    }
}

impl App for AppStoreApp {
    // What a worker reports: a step along the way, or the final outcome.
    type Completion = Completion;

    fn event(
        &mut self,
        event: &Event<Self::Completion>,
        context: &mut Context<'_, Self::Completion>,
    ) -> Action {
        match event {
            Event::Pointer(pointer) => self.press(pointer, context),
            Event::Completed(completion) => {
                match completion.clone() {
                    Completion::Progress { step, percent } => {
                        self.pending_damage =
                            self.working_row()
                                .map_or(Damage::Full, |body| Damage::Regions {
                                    regions: vec![body],
                                });
                        self.screen.report(&step, percent);
                    }
                    Completion::Done(outcome) => {
                        self.screen.apply(outcome);
                        // Refreshed inventories reorder and resize rows,
                        // notes and failures change what a detail view or a
                        // banner shows, and a finished install leaves the
                        // row it was in stale until the next refresh (§6) —
                        // none of that is safe to describe as one rectangle.
                        self.pending_damage = Damage::Full;
                    }
                }
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    fn draw(&mut self, canvas: &mut Canvas, _context: &mut Context<'_, Self::Completion>) {
        self.layout = Some(render(canvas, &self.screen));
    }

    /// Nothing here outlives the process. The screen's inventory comes from a
    /// fresh survey every launch, exactly the way Home's shelf does — there
    /// is no local state a save could lose.
    fn save(&mut self, _context: &mut Context<'_, Self::Completion>) -> Result<(), SaveError> {
        Ok(())
    }

    fn damage(&self) -> Damage {
        self.pending_damage.clone()
    }
}

fn failed(app: Option<AppId>, error: &SourceError) -> Outcome {
    Outcome::Failed {
        app,
        message: error.to_string(),
        advice: error.advice(),
    }
}

/// Turns install progress into what [`Completion::Progress`] shows.
///
/// A thread-safe [`Progress`] over a [`Completer`] rather than a channel of
/// its own: the event loop already has exactly one queue for "something a
/// worker wants to tell the app", and this is that, once per [`Step`].
#[derive(Debug)]
struct Reporter {
    completer: Completer<Completion>,
}

impl Reporter {
    fn new(completer: Completer<Completion>) -> Self {
        Self { completer }
    }
}

impl Progress for Reporter {
    fn step(&self, step: Step) {
        let (step, percent) = describe(step);
        let _ = self
            .completer
            .complete(Completion::Progress { step, percent });
    }
}

/// The word — and, for a download, the fraction — one [`Step`] shows as.
///
/// Mirrors `paperctl install`'s own `Printer` (`tools/paperctl/src/install.rs`):
/// a terminal and the panel report the same step in the same words, on
/// purpose, so the reporting has a second consumer and cannot quietly drift.
fn describe(step: Step) -> (String, Option<u8>) {
    match step {
        Step::Downloading { done, total } if total > 0 => (
            "DOWNLOADING".to_owned(),
            Some((done.saturating_mul(100) / total).min(100) as u8),
        ),
        Step::Downloading { .. } => ("DOWNLOADING".to_owned(), None),
        Step::Verifying => ("VERIFYING".to_owned(), None),
        Step::Extracting => ("EXTRACTING".to_owned(), None),
        Step::Committing => ("COMMITTING".to_owned(), None),
        Step::Activating => ("ACTIVATING".to_owned(), None),
    }
}

#[cfg(test)]
mod tests {
    //! Drives [`AppStoreApp`] the way a real host would: over a socket pair,
    //! through `paper_sdk::run`, the same harness `ChessApp`'s own tests use
    //! (`apps/chess/src/app.rs`). A [`StoreSource`] test double stands in for
    //! `PackagesSource` so the timing below is deterministic — a real
    //! download's length depends on the network, and these tests exist
    //! specifically to pin down what happens *while* one is in flight.

    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use paper_packages::AppId;
    use paper_packages::install::{Progress, Step};
    use paper_packages::inventory::{AppEntry, Inventory};
    use paper_protocol::{
        AppMessage, AppPaths, Capability, ContactId, DrawReason, DrawRequest, ExitReason, FrameId,
        Hello, HostMessage, LaunchReason, LifecycleEvent, PixelFormat, Pointer, PointerEvent,
        PointerPhase, Request, SessionId, SurfaceDescriptor, codec,
    };
    use paper_sdk::{Canvas, Damage, LocalSurfaces, Outcome, RuntimeError, SCREEN, run};
    use semver::Version;

    use super::AppStoreApp;
    use crate::render::{StoreLayout, render};
    use crate::screen::AppStoreScreen;
    use crate::source::{SourceError, StoreSource};

    fn chess() -> AppId {
        "dev.calum.chess".parse().expect("a valid id")
    }

    fn inventory_offering(version: &Version) -> Inventory {
        Inventory::fixture(
            vec![AppEntry::fixture(
                chess(),
                "Chess".parse().expect("a valid name"),
                None,
                Some(version.clone()),
            )],
            None,
        )
    }

    /// A [`StoreSource`] whose install takes exactly `delay` — standing in
    /// for a slow download so the tests below are about ordering and timing
    /// rather than about a real catalog, which `apps/app-store/tests/store.rs`
    /// already covers.
    #[derive(Debug)]
    struct SlowSource {
        version: Version,
        delay: Duration,
    }

    impl StoreSource for SlowSource {
        fn inventory(&self) -> Result<Inventory, SourceError> {
            Ok(inventory_offering(&self.version))
        }

        fn notes(&self, _app: &AppId, _version: &Version) -> Result<String, SourceError> {
            Ok(String::new())
        }

        fn install(
            &self,
            _app: &AppId,
            version: &Version,
            progress: &dyn Progress,
        ) -> Result<Version, SourceError> {
            progress.step(Step::Downloading { done: 0, total: 1 });
            std::thread::sleep(self.delay);
            progress.step(Step::Committing);
            Ok(version.clone())
        }

        fn rollback(&self, _app: &AppId) -> Result<Version, SourceError> {
            unreachable!("these tests never roll back")
        }
    }

    /// A [`StoreSource`] whose install always fails — for the WWW-38
    /// requirement that a failure "surface as `Failure`, not a panic".
    #[derive(Debug)]
    struct FailingSource {
        version: Version,
    }

    impl StoreSource for FailingSource {
        fn inventory(&self) -> Result<Inventory, SourceError> {
            Ok(inventory_offering(&self.version))
        }

        fn notes(&self, _app: &AppId, _version: &Version) -> Result<String, SourceError> {
            Ok(String::new())
        }

        fn install(
            &self,
            app: &AppId,
            version: &Version,
            _progress: &dyn Progress,
        ) -> Result<Version, SourceError> {
            Err(SourceError::NotOffered {
                app: app.clone(),
                version: version.clone(),
            })
        }

        fn rollback(&self, _app: &AppId) -> Result<Version, SourceError> {
            unreachable!("this test never rolls back")
        }
    }

    /// Paths a real host would grant. Nothing in `AppStoreApp` reads them —
    /// it never calls `context.storage()` — so unlike `ChessApp`'s tests
    /// these do not need to exist on disk.
    fn app_paths() -> AppPaths {
        AppPaths {
            assets: "assets".into(),
            private: "private".into(),
            temp: "temp".into(),
            shared: Vec::new(),
        }
    }

    type Session = (
        UnixStream,
        UnixStream,
        std::thread::JoinHandle<Result<Outcome, RuntimeError>>,
    );

    /// Opens a real session over a real socket pair, the same loop a
    /// launched process runs, blocking until `Ready` arrives.
    fn start_session(source: Arc<dyn StoreSource>) -> Session {
        let (host_side, app_side) = UnixStream::pair().expect("a socket pair");
        let reader = app_side.try_clone().expect("clone for reading");
        let writer = app_side;
        let screen = AppStoreScreen::new(source.inventory().expect("an inventory"));
        let app = AppStoreApp::new(screen, source);
        let handle = std::thread::spawn(move || run(app, reader, writer, LocalSurfaces::new()));

        let mut host_writer = host_side.try_clone().expect("clone for writing");
        let hello = Hello {
            protocol: paper_protocol::CURRENT,
            session: SessionId::new(1),
            app: "dev.calum.app-store".parse().expect("a valid id"),
            version: "0.1.0".parse().expect("a valid version"),
            launch: LaunchReason::Fresh,
            surface: SurfaceDescriptor::packed(SCREEN, PixelFormat::Argb8888),
            capabilities: vec![Capability::Packages],
            paths: app_paths(),
        };
        codec::write_message(&mut host_writer, &HostMessage::Hello(hello))
            .expect("writes the hello");

        let mut host_reader = host_side;
        let _ready: AppMessage = codec::read_message(&mut host_reader).expect("reads ready");
        (host_reader, host_writer, handle)
    }

    fn send(
        host_reader: &mut UnixStream,
        host_writer: &mut UnixStream,
        message: &HostMessage,
    ) -> AppMessage {
        codec::write_message(host_writer, message).expect("writes the message");
        codec::read_message(host_reader).expect("reads the reply")
    }

    /// Asks for `frame` and returns its damage, discarding any `Request`s
    /// that arrive first — an install in flight sends one unprompted per
    /// step, exactly like `paperctl`'s own `Session::request_frame`
    /// (`tools/paperctl/src/session.rs`), and for the same reason: a `Draw`
    /// is answered by exactly one `Frame`, whatever else is interleaved with
    /// it on the wire.
    fn draw(host_reader: &mut UnixStream, host_writer: &mut UnixStream, frame: u64) -> Damage {
        codec::write_message(
            host_writer,
            &HostMessage::Draw(DrawRequest {
                frame: FrameId::new(frame),
                reason: DrawReason::AppRequested,
                viewport: SCREEN,
            }),
        )
        .expect("writes the draw request");
        loop {
            match codec::read_message::<_, AppMessage>(host_reader).expect("reads a reply") {
                AppMessage::Frame(done) => return done.damage,
                _ => continue,
            }
        }
    }

    fn pointer_up(at: paper_protocol::Point) -> HostMessage {
        HostMessage::Pointer(PointerEvent::new(
            at,
            PointerPhase::Up,
            Pointer::Touch,
            ContactId::FIRST,
        ))
    }

    /// Ends the session the way a real host always does.
    fn finish_session(
        mut host_reader: UnixStream,
        mut host_writer: UnixStream,
        handle: std::thread::JoinHandle<Result<Outcome, RuntimeError>>,
    ) {
        let deadline =
            LifecycleEvent::prepare_to_exit(ExitReason::ReturnToStock, Duration::from_secs(5));
        let reply = send(
            &mut host_reader,
            &mut host_writer,
            &HostMessage::Lifecycle(deadline),
        );
        assert!(
            matches!(
                reply,
                AppMessage::Saved(paper_protocol::Saved { ok: true, .. })
            ),
            "expected a successful save, got {reply:?}"
        );
        match handle.join().expect("the session thread did not panic") {
            Ok(Outcome::Exited { saved: true, .. }) => {}
            other => panic!("expected a saved exit, got {other:?}"),
        }
    }

    /// The install button's position and its row's body, computed the same
    /// way the app itself renders — the same convention `ChessApp`'s own
    /// tests use (`square_center`, `apps/chess/src/app.rs`) — so a tap lands
    /// where the button actually is.
    fn install_button(inventory: Inventory) -> (paper_protocol::Point, paper_protocol::Rect) {
        let screen = AppStoreScreen::new(inventory);
        let mut canvas = Canvas::new(SCREEN).expect("a canvas");
        let StoreLayout::List(list) = render(&mut canvas, &screen) else {
            panic!("an app that is only offered, not installed, shows the list");
        };
        let row = list.rows.first().expect("one row");
        (row.action.expect("an install button").center(), row.body)
    }

    #[test]
    fn the_first_frame_claims_the_whole_panel() {
        let source: Arc<dyn StoreSource> = Arc::new(SlowSource {
            version: "0.1.0".parse().expect("a valid version"),
            delay: Duration::ZERO,
        });
        let (mut host_reader, mut host_writer, handle) = start_session(source);

        assert_eq!(draw(&mut host_reader, &mut host_writer, 1), Damage::Full);

        finish_session(host_reader, host_writer, handle);
    }

    #[test]
    fn pressing_install_answers_the_next_draw_without_waiting_for_it_to_finish() {
        let version: Version = "0.1.0".parse().expect("a valid version");
        let delay = Duration::from_millis(300);
        let (install_at, row_body) = install_button(inventory_offering(&version));
        let source: Arc<dyn StoreSource> = Arc::new(SlowSource {
            version: version.clone(),
            delay,
        });

        let (mut host_reader, mut host_writer, handle) = start_session(source);
        assert_eq!(draw(&mut host_reader, &mut host_writer, 1), Damage::Full);

        let reply = send(&mut host_reader, &mut host_writer, &pointer_up(install_at));
        assert!(
            matches!(reply, AppMessage::Request(Request::Redraw)),
            "pressing install must ask for a redraw, got {reply:?}"
        );

        // The install is still sleeping. If starting it had blocked the
        // event loop — the failure mode WWW-38 exists to avoid — this draw
        // would not answer until the sleep was over.
        let started = Instant::now();
        let damage = draw(&mut host_reader, &mut host_writer, 2);
        let elapsed = started.elapsed();
        assert!(
            elapsed < delay / 2,
            "a draw requested right after starting an install must not wait on it \
             (took {elapsed:?} against a {delay:?} install)"
        );
        assert_eq!(
            damage,
            Damage::Regions {
                regions: vec![row_body]
            },
            "starting an install changes only the row it is in, not the panel"
        );

        // Long enough that the install (and its `Committing` step) has
        // definitely finished and been folded in.
        std::thread::sleep(delay + Duration::from_millis(200));
        assert_eq!(
            draw(&mut host_reader, &mut host_writer, 3),
            Damage::Full,
            "a finished install can leave more than one row stale (§6: the \
             row itself is not refreshed for free) and must not under-claim"
        );

        finish_session(host_reader, host_writer, handle);
    }

    #[test]
    fn an_install_that_fails_redraws_instead_of_panicking() {
        let version: Version = "0.1.0".parse().expect("a valid version");
        let (install_at, _row_body) = install_button(inventory_offering(&version));
        let source: Arc<dyn StoreSource> = Arc::new(FailingSource { version });

        let (mut host_reader, mut host_writer, handle) = start_session(source);
        draw(&mut host_reader, &mut host_writer, 1);
        send(&mut host_reader, &mut host_writer, &pointer_up(install_at));

        // No panic reaching here is most of the assertion; the rest is that
        // the session is still alive and answering normally afterwards.
        assert_eq!(draw(&mut host_reader, &mut host_writer, 2), Damage::Full);

        finish_session(host_reader, host_writer, handle);
    }
}
