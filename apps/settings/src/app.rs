//! Settings as a real [`App`] (§6, ADR-0015, WWW-71).
//!
//! Every decision a press makes lives on [`SettingsScreen::press`]; what is
//! here is the wire adapter around it, at Home's own pattern
//! (`apps/home/src/app.rs`) scaled up to six facts and three writes instead
//! of one fact: ask on first draw, cache whatever answers land, redraw. The
//! six reads go out together on launch and again on resume, because a
//! [`SystemAnswer`](paper_sdk::SystemAnswer) may arrive several callbacks
//! later — see `platform/protocol/src/system.rs`'s module doc — and this app
//! has no reason to believe any one of them is stale less often than the
//! others.
//!
//! ## Nothing to save
//!
//! Chess saves because a half-finished game only exists in the app. Settings
//! still has no such state: a rollback, an uninstall or a revoke is a Host
//! transaction, and the only thing left in `self` once one is confirmed is
//! which tab is showing — which §6 does not ask anyone to remember, and which
//! every launch deliberately starts at [`SettingsPage::Apps`].

use std::convert::Infallible;

use paper_sdk::{
    Action, AdminQuery, AdminValue, App, Canvas, Context, Damage, Event, Rect, SaveError,
    SystemQueryKind, SystemValue,
};

use crate::nav::SettingsPage;
use crate::screen::{SettingsLayout, SettingsScreen};

/// What the previous frame showed.
///
/// Only the two things that decide how much of the panel a new frame changes:
/// e-ink pays for every pixel presented, and "which page" plus "is a dialog
/// over it" is the whole of what a press can change here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Painted {
    page: SettingsPage,
    confirming: bool,
}

/// Every read [`AdminQuery`] this app asks for at launch and on resume.
const READS: [AdminQuery; 6] = [
    AdminQuery::InstalledApps,
    AdminQuery::StorageUsage,
    AdminQuery::Grants,
    AdminQuery::CatalogStatus,
    AdminQuery::PlatformInfo,
    AdminQuery::Diagnostics,
];

/// The Settings app.
#[derive(Debug)]
pub struct SettingsApp {
    screen: SettingsScreen,
    /// Whether the launch reads have been sent yet. Sent from `draw` rather
    /// than `event` because [`Context::query_system`] is available from
    /// either, and `draw` is where Home's own launch fact-gathering already
    /// lives — one place to look for "what does this app ask for on start".
    asked: bool,
    /// What the last frame drawn showed, or `None` when the next frame has to
    /// be treated as the first one.
    painted: Option<Painted>,
    /// What [`App::damage`] will report for the frame just drawn. Computed in
    /// [`App::draw`] because `damage` takes `&self` and runs after it.
    damage: Damage,
}

impl SettingsApp {
    /// Builds Settings showing nothing until the host answers.
    pub fn new() -> Self {
        Self {
            screen: SettingsScreen::loading(),
            asked: false,
            painted: None,
            damage: Damage::Full,
        }
    }

    /// The screen this app is driving.
    pub fn screen(&self) -> &SettingsScreen {
        &self.screen
    }

    fn ask_reads(context: &mut Context<'_, Infallible>) {
        for query in READS {
            context.query_system(SystemQueryKind::Admin(query));
        }
    }

    /// What changed between `previous` and the frame just drawn as `now`.
    ///
    /// Three answers, and the middle one is the only interesting one:
    ///
    /// - No previous frame, or a confirmation open on either side of this one:
    ///   [`Damage::Full`]. The dialog draws a scrim over the whole viewport
    ///   and takes the footer action away with it, so opening or closing one
    ///   changes every pixel.
    /// - A page change: the two tabs whose highlight moved, plus everything
    ///   below the tab strip. The status bar is identical on every page and
    ///   the four tabs that did not move are identical too — about a tenth of
    ///   the panel that a `Full` would have repainted for nothing.
    /// - Anything else: no regions at all. A press or an answer that changed
    ///   nothing visible redraws nothing, and saying so is not the same as
    ///   claiming less than changed.
    fn damage_between(
        previous: Option<Painted>,
        now: Painted,
        layout: &SettingsLayout,
        bounds: Rect,
    ) -> Damage {
        let Some(previous) = previous else {
            return Damage::Full;
        };
        if previous.confirming || now.confirming {
            return Damage::Full;
        }
        if previous.page == now.page {
            return Damage::Regions {
                regions: Vec::new(),
            };
        }
        let tabs = layout.nav.tabs();
        let (Some(was), Some(is)) = (tab_rect(tabs, previous.page), tab_rect(tabs, now.page))
        else {
            // A nav strip that did not draw the tabs this app knows about is
            // not something to guess about: repaint everything.
            return Damage::Full;
        };
        let below_the_tabs = Rect::new(
            0.0,
            is.bottom(),
            bounds.width,
            (bounds.bottom() - is.bottom()).max(0.0),
        );
        Damage::Regions {
            regions: vec![was, is, below_the_tabs],
        }
    }
}

impl Default for SettingsApp {
    fn default() -> Self {
        Self::new()
    }
}

/// Where `page`'s tab was drawn.
///
/// [`NavLayout::tabs`](crate::NavLayout::tabs) is in
/// [`SettingsPage::ALL`] order, which is the only correspondence between a
/// page and a rectangle either side of this has.
fn tab_rect(tabs: &[Rect], page: SettingsPage) -> Option<Rect> {
    let index = SettingsPage::ALL.iter().position(|&known| known == page)?;
    tabs.get(index).copied()
}

impl App for SettingsApp {
    // Every write this app asks for is a `SystemQuery`, answered
    // asynchronously on the same queue as everything else — nothing here
    // spawns a thread of its own.
    type Completion = Infallible;

    fn event(
        &mut self,
        event: &Event<Self::Completion>,
        context: &mut Context<'_, Self::Completion>,
    ) -> Action {
        match event {
            Event::Pointer(pointer) => {
                // Pure and cheap: no canvas, no drawn frame to wait for. A
                // tap is hit-testable the instant Settings exists, not only
                // after the first `draw` — see `crate::screen::layout`'s own
                // doc.
                let layout = crate::screen::layout(context.viewport(), &self.screen);
                let press = self.screen.press(&layout, pointer);
                if let Some(kind) = press.admin {
                    context.query_system(SystemQueryKind::Admin(kind.into_query()));
                }
                press.action
            }
            Event::Resumed => {
                // Whatever was on the glass while this app was away, and
                // whatever changed in the store while it was not looking —
                // the cache stays as it was until the reads this asks for
                // now come back, rather than going blank in the meantime.
                Self::ask_reads(context);
                self.painted = None;
                Action::Redraw
            }
            Event::System(answer) => match &answer.result {
                Ok(SystemValue::Admin(value)) => {
                    let outcome = write_outcome(value);
                    self.screen.apply(value.clone());
                    if let Some(Err(error)) = outcome {
                        context.warn(format!("the host refused the action: {error:?}"));
                    }
                    if outcome.is_some() {
                        // A write landed; re-read everything it could have
                        // changed rather than guess which fields moved.
                        Self::ask_reads(context);
                    }
                    Action::Redraw
                }
                Err(denial) => {
                    context.warn(format!("an admin query was denied: {denial:?}"));
                    Action::None
                }
                _ => Action::None,
            },
            _ => Action::None,
        }
    }

    fn draw(&mut self, canvas: &mut Canvas, context: &mut Context<'_, Self::Completion>) {
        if !self.asked {
            self.asked = true;
            Self::ask_reads(context);
        }
        let layout = crate::screen::render(canvas, &self.screen);
        let now = Painted {
            page: self.screen.page,
            confirming: self.screen.is_confirming(),
        };
        self.damage = Self::damage_between(self.painted, now, &layout, canvas.bounds());
        self.painted = Some(now);
    }

    fn save(&mut self, _context: &mut Context<'_, Self::Completion>) -> Result<(), SaveError> {
        // See the module doc: there is nothing here that outlives the process.
        Ok(())
    }

    fn damage(&self) -> Damage {
        self.damage.clone()
    }
}

/// The `Result` a write's own [`AdminValue`] answer carries, or `None` when
/// `value` is a read.
fn write_outcome(value: &AdminValue) -> Option<&Result<(), paper_sdk::AdminError>> {
    match value {
        AdminValue::Rollback(result)
        | AdminValue::Uninstall(result)
        | AdminValue::RevokeGrant(result) => Some(result),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Painted, SettingsApp};
    use crate::nav::SettingsPage;
    use crate::screen::{PageLayout, SettingsLayout};
    use paper_sdk::{Canvas, Damage, PointerEvent, Rect, SCREEN};
    use paper_sdk::{ContactId, Point, Pointer, PointerPhase};

    fn app() -> SettingsApp {
        SettingsApp::new()
    }

    fn canvas() -> Canvas {
        Canvas::new(SCREEN).expect("screen-sized canvas")
    }

    /// Draws one frame the way the runtime does — `draw`, then `damage` —
    /// and hands back both the layout that was drawn and what was claimed.
    ///
    /// `draw` needs a `Context`, which only `paper_sdk::run` can build, so
    /// the tests that only care about drawing call `render` and the damage
    /// arithmetic directly against a screen seeded with
    /// [`crate::screen::SettingsScreen::preview`]; the ones that care about
    /// the whole loop (`a_whole_session_opens_draws_a_page_and_exits_saved`)
    /// go through a real session instead, where the app starts from
    /// `SettingsScreen::loading` exactly as a launched process does.
    fn frame(app: &mut SettingsApp, canvas: &mut Canvas) -> (SettingsLayout, Damage) {
        let layout = crate::screen::render(canvas, &app.screen);
        let now = Painted {
            page: app.screen.page,
            confirming: app.screen.is_confirming(),
        };
        let damage = SettingsApp::damage_between(app.painted, now, &layout, canvas.bounds());
        app.painted = Some(now);
        (layout, damage)
    }

    fn tap(at: Point) -> PointerEvent {
        PointerEvent::new(at, PointerPhase::Up, Pointer::Touch, ContactId::FIRST)
    }

    fn area(regions: &[Rect]) -> f32 {
        regions.iter().map(|rect| rect.width * rect.height).sum()
    }

    /// A screen seeded with fixture data, the same shape a real screen has
    /// once its launch reads have all come back — everything below this
    /// point tests the damage arithmetic and press routing, not the async
    /// loading state `screen::tests` already covers.
    fn loaded_app() -> SettingsApp {
        let mut app = app();
        app.screen = crate::screen::SettingsScreen::preview();
        app
    }

    #[test]
    fn the_first_frame_claims_the_whole_viewport() {
        let mut app = loaded_app();
        let mut canvas = canvas();
        let (_, damage) = frame(&mut app, &mut canvas);
        assert_eq!(damage, Damage::Full);
    }

    #[test]
    fn a_frame_that_changed_nothing_claims_nothing() {
        let mut app = loaded_app();
        let mut canvas = canvas();
        frame(&mut app, &mut canvas);
        let (_, damage) = frame(&mut app, &mut canvas);
        assert_eq!(
            damage,
            Damage::Regions {
                regions: Vec::new()
            }
        );
    }

    /// The point of overriding `damage` at all: a tab press must not ask the
    /// panel to repaint the parts of it that are identical.
    #[test]
    fn switching_pages_claims_the_tabs_that_moved_and_the_page_body_only() {
        let mut app = loaded_app();
        let mut canvas = canvas();
        let (layout, _) = frame(&mut app, &mut canvas);

        let storage_tab = layout.nav.tabs()[1];
        app.screen.press_tab(&layout.nav, storage_tab.center());
        assert_eq!(app.screen.page, SettingsPage::Storage);

        let (_, damage) = frame(&mut app, &mut canvas);
        let Some(regions) = damage.regions().map(<[Rect]>::to_vec) else {
            panic!("a tab press repainted the whole panel");
        };
        let whole = canvas.bounds();
        assert!(
            area(&regions) < whole.width * whole.height,
            "claimed at least the whole panel: {regions:?}"
        );
        // The status bar says SETTINGS and the version on every page; nothing
        // in it can change on a tab press.
        assert!(
            regions.iter().all(|rect| rect.y >= storage_tab.y),
            "claimed part of the status bar: {regions:?}"
        );
        // Everything the new page draws has to be inside what was claimed, or
        // it stays on the glass as the old page's ink.
        for rect in [storage_tab, layout.nav.tabs()[0]] {
            assert!(
                regions
                    .iter()
                    .any(|claimed| claimed.contains(rect.center())),
                "{rect:?} moved but was not claimed"
            );
        }
        let body_bottom = Point::new(whole.center().x, whole.bottom() - 1.0);
        assert!(
            regions.iter().any(|rect| rect.contains(body_bottom)),
            "the bottom of the page was not claimed: {regions:?}"
        );
    }

    /// The scrim covers the whole viewport and the footer action disappears
    /// under it, so a claim smaller than everything would leave the dialog
    /// drawn over a page that is still showing its old ink.
    #[test]
    fn opening_and_closing_a_confirmation_claims_the_whole_viewport() {
        let mut app = loaded_app();
        let mut canvas = canvas();
        let (layout, _) = frame(&mut app, &mut canvas);

        let PageLayout::Apps(apps) = &layout.page else {
            panic!("Settings opens on the apps page");
        };
        app.screen.press_apps(apps, apps.rows[0].uninstall.center());
        let (_, opened) = frame(&mut app, &mut canvas);
        assert_eq!(opened, Damage::Full);

        app.screen.cancel();
        let (_, closed) = frame(&mut app, &mut canvas);
        assert_eq!(closed, Damage::Full);
    }

    #[test]
    fn a_press_before_the_first_frame_hits_nothing_up_there() {
        // The layout is computed fresh from `context.viewport()` on every
        // event now (`crate::screen::layout`), not cached from a previous
        // `draw` — so this asks whether (1, 1) lands on anything, not
        // whether a layout exists yet. It does not: that point is inside the
        // status bar, above every tab and action.
        let mut session = session::Session::start(app());
        assert_eq!(session.pointer(tap(Point::new(1.0, 1.0))), None);
        session.finish();
    }

    /// The bug the wire-adapter restructure this app was built under removes:
    /// five apps each stored `layout: Option<Layout>` and read taps as dead
    /// until the first `draw` completed. A tab tap here lands before this
    /// session has ever been asked to draw a frame, and still switches the
    /// page — against a screen still in its loading state, which is exactly
    /// what a real launch looks like before any admin answer has come back.
    #[test]
    fn a_tab_press_before_any_draw_still_switches_the_page() {
        let screen = crate::screen::SettingsScreen::loading();
        let layout = crate::screen::layout(SCREEN, &screen);
        let storage_tab = layout.nav.tabs()[1];

        let mut session = session::Session::start(app());
        assert_eq!(
            session.pointer(tap(storage_tab.center())),
            Some(paper_protocol::Request::Redraw)
        );
        session.finish();
    }

    /// The whole reason this file exists: a real session, over a real socket,
    /// through `paper_sdk::run` — the same loop a launched process runs, and
    /// the same one that sends this app's launch reads and would answer them
    /// if anything on the other end of the socket were `paperctl`'s
    /// `AdminResponder` rather than this bare test harness.
    #[test]
    fn a_whole_session_opens_draws_a_page_and_exits_saved() {
        let screen = crate::screen::SettingsScreen::loading();
        let layout = crate::screen::layout(SCREEN, &screen);
        let storage_tab = layout.nav.tabs()[1];
        let footer = layout
            .return_to_stock
            .expect("the footer action is drawn when nothing is confirming");

        let mut session = session::Session::start(app());
        session.draw();
        assert_eq!(
            session.pointer(tap(storage_tab.center())),
            Some(paper_protocol::Request::Redraw),
            "a tab press asks for a redraw"
        );
        session.draw();
        assert_eq!(
            session.pointer(tap(footer.center())),
            Some(paper_protocol::Request::ReturnToStock),
            "the footer action hands the panel back"
        );
        session.finish();
    }

    #[test]
    fn a_tap_on_nothing_asks_for_nothing() {
        let mut session = session::Session::start(app());
        session.draw();
        // The status bar: no tab, no row, no footer action.
        assert_eq!(session.pointer(tap(Point::new(20.0, 20.0))), None);
        session.finish();
    }

    /// An admin answer arriving asks for a redraw, the same round trip
    /// `platform/sdk/src/runtime.rs`'s own test proves for the no-grant tier
    /// — proof the answer actually reached `Event::System` and was applied,
    /// rather than silently dropped on a queue nothing drains.
    #[test]
    fn an_installed_apps_answer_asks_for_a_redraw() {
        let mut session = session::Session::start(app());
        let request = session.answer_installed_apps(vec![paper_sdk::InstalledAppSummary {
            id: "dev.calum.chess".parse().unwrap(),
            name: "Chess".to_owned(),
            active_version: "0.2.0".to_owned(),
            previous_version: None,
            data_bytes: 0,
        }]);
        assert_eq!(
            request,
            Some(paper_protocol::Request::Redraw),
            "an applied admin answer should ask for a redraw"
        );
        session.finish();
    }

    /// A real session over a real socket pair, the same shape
    /// `apps/chess/src/app.rs` uses: the SDK's own loop on one side, a
    /// hand-driven host on the other.
    mod session {
        use super::SettingsApp;
        use paper_protocol::{
            AdminQuery, AppMessage, AppPaths, DrawReason, DrawRequest, FrameId, Hello, HostMessage,
            InstalledAppSummary, LaunchReason, PixelFormat, PointerEvent, Request, SessionId,
            SurfaceDescriptor, SystemAnswer, SystemQueryKind, SystemValue, codec,
        };
        use paper_sdk::{LocalSurfaces, Outcome, SCREEN, run};
        use std::os::unix::net::UnixStream;

        pub(super) struct Session {
            reader: UnixStream,
            writer: UnixStream,
            next_frame: FrameId,
            app: std::thread::JoinHandle<Result<Outcome, paper_sdk::RuntimeError>>,
        }

        impl Session {
            /// Opens a session on `app` and waits for its `Ready`.
            pub(super) fn start(app: SettingsApp) -> Self {
                let (host_side, app_side) = UnixStream::pair().expect("a socket pair");
                let app_reader = app_side.try_clone().expect("clone for reading");
                let handle = std::thread::spawn(move || {
                    run(app, app_reader, app_side, LocalSurfaces::new())
                });

                let mut reader = host_side;
                let mut writer = reader.try_clone().expect("clone for writing");
                let root = std::env::temp_dir().join(format!(
                    "paper-settings-session-{}-{:?}",
                    std::process::id(),
                    std::thread::current().id()
                ));
                std::fs::create_dir_all(&root).expect("a temp directory");
                let hello = Hello {
                    protocol: paper_protocol::CURRENT,
                    session: SessionId::new(1),
                    app: "dev.calum.settings".parse().expect("a valid id"),
                    version: "0.1.0".parse().expect("a valid version"),
                    launch: LaunchReason::Fresh,
                    surface: SurfaceDescriptor::packed(SCREEN, PixelFormat::Argb8888),
                    // Settings is granted nothing: every fact and write it
                    // needs travels as a `SystemQuery` instead.
                    capabilities: Vec::new(),
                    paths: AppPaths {
                        assets: root.join("assets"),
                        private: root.join("private"),
                        temp: root.join("temp"),
                        shared: Vec::new(),
                    },
                };
                codec::write_message(&mut writer, &HostMessage::Hello(hello)).expect("says hello");
                let ready: AppMessage = codec::read_message(&mut reader).expect("reads Ready");
                assert!(matches!(ready, AppMessage::Ready(_)), "got {ready:?}");

                Self {
                    reader,
                    writer,
                    next_frame: FrameId::FIRST,
                    app: handle,
                }
            }

            /// Asks for a frame and waits for it. Any `SystemQuery` sent
            /// along the way is left unanswered — this harness is not
            /// `AdminResponder`, and a test that needs a real answer calls
            /// [`Self::answer_installed_apps`] instead.
            pub(super) fn draw(&mut self) {
                let frame = self.next_frame;
                self.next_frame = self.next_frame.next();
                codec::write_message(
                    &mut self.writer,
                    &HostMessage::Draw(DrawRequest {
                        frame,
                        reason: if frame == FrameId::FIRST {
                            DrawReason::First
                        } else {
                            DrawReason::AppRequested
                        },
                        viewport: SCREEN,
                    }),
                )
                .expect("asks for a frame");
                loop {
                    match codec::read_message::<_, AppMessage>(&mut self.reader)
                        .expect("reads the answer")
                    {
                        AppMessage::Frame(_) => return,
                        AppMessage::SystemQuery(_) => {}
                        _ => continue,
                    }
                }
            }

            /// Asks for a frame — the same trigger [`Self::draw`] uses — then
            /// answers the first `AdminQuery::InstalledApps` query that
            /// produces with `apps`, and returns whatever [`Request`] the
            /// app asked for once it applied the answer, if any.
            ///
            /// One call rather than "draw, then answer": the launch reads
            /// this app sends go out from its very first `draw` and are
            /// gone once ignored, the same way a real host's queries are —
            /// there is no second chance to answer a query nobody kept.
            pub(super) fn answer_installed_apps(
                &mut self,
                apps: Vec<InstalledAppSummary>,
            ) -> Option<Request> {
                let frame = self.next_frame;
                self.next_frame = self.next_frame.next();
                codec::write_message(
                    &mut self.writer,
                    &HostMessage::Draw(DrawRequest {
                        frame,
                        reason: if frame == FrameId::FIRST {
                            DrawReason::First
                        } else {
                            DrawReason::AppRequested
                        },
                        viewport: SCREEN,
                    }),
                )
                .expect("asks for a frame");
                loop {
                    match codec::read_message::<_, AppMessage>(&mut self.reader)
                        .expect("reads a message")
                    {
                        AppMessage::SystemQuery(query)
                            if query.kind == SystemQueryKind::Admin(AdminQuery::InstalledApps) =>
                        {
                            codec::write_message(
                                &mut self.writer,
                                &HostMessage::SystemAnswer(SystemAnswer {
                                    id: query.id,
                                    result: Ok(SystemValue::Admin(
                                        paper_protocol::AdminValue::InstalledApps(apps),
                                    )),
                                }),
                            )
                            .expect("answers the query");
                            break;
                        }
                        _ => continue,
                    }
                }
                // The rest of the launch batch and this Draw's own Frame are
                // already on the wire, ahead of whatever the answer produces
                // (§8: one process, one loop, strict send order).
                loop {
                    match codec::read_message::<_, AppMessage>(&mut self.reader)
                        .expect("reads a message")
                    {
                        AppMessage::Frame(_) => break,
                        _ => continue,
                    }
                }
                match codec::read_message::<_, AppMessage>(&mut self.reader)
                    .expect("reads what the answer produced")
                {
                    AppMessage::Request(request) => Some(request),
                    _ => None,
                }
            }

            /// Sends a tap and returns the request it produced, if any.
            ///
            /// A press that changes nothing sends no message at all, so this
            /// asks for a frame afterwards and treats whatever arrives before
            /// it as the answer.
            pub(super) fn pointer(&mut self, event: PointerEvent) -> Option<Request> {
                codec::write_message(&mut self.writer, &HostMessage::Pointer(event))
                    .expect("sends a tap");
                let frame = self.next_frame;
                self.next_frame = self.next_frame.next();
                codec::write_message(
                    &mut self.writer,
                    &HostMessage::Draw(DrawRequest {
                        frame,
                        reason: DrawReason::AppRequested,
                        viewport: SCREEN,
                    }),
                )
                .expect("asks for a frame");
                let mut request = None;
                loop {
                    match codec::read_message::<_, AppMessage>(&mut self.reader)
                        .expect("reads the answer")
                    {
                        AppMessage::Frame(_) => return request,
                        AppMessage::Request(seen) => request = Some(seen),
                        AppMessage::SystemQuery(_) => {}
                        _ => continue,
                    }
                }
            }

            /// Ends the session the way a host does, and checks it saved.
            pub(super) fn finish(mut self) {
                let event = paper_protocol::LifecycleEvent::prepare_to_exit(
                    paper_protocol::ExitReason::ReturnToStock,
                    std::time::Duration::from_secs(5),
                );
                codec::write_message(&mut self.writer, &HostMessage::Lifecycle(event))
                    .expect("asks it to exit");
                let saved = loop {
                    match codec::read_message::<_, AppMessage>(&mut self.reader)
                        .expect("reads the answer")
                    {
                        AppMessage::Saved(saved) => break saved,
                        _ => continue,
                    }
                };
                assert!(
                    saved.ok,
                    "Settings has nothing to save and cannot fail at it"
                );
                match self.app.join().expect("the session thread did not panic") {
                    Ok(Outcome::Exited { saved: true, .. }) => {}
                    other => panic!("expected a saved exit, got {other:?}"),
                }
            }
        }
    }
}
