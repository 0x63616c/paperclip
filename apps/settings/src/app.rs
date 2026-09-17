//! Settings as a real [`App`] (§6, ADR-0015).
//!
//! Every decision a press makes lives on [`SettingsScreen::press`], and every
//! read and write of real state lives behind [`SettingsHost`]. What is here is
//! the wire adapter between them: feed pointer events in, draw on request,
//! report what changed, and say plainly that there is nothing to save.
//!
//! ## Nothing to save
//!
//! Chess saves because a half-finished game only exists in the app. Settings
//! has no such state: a rollback, an uninstall or a revoke is a Host
//! transaction that has already been committed by the time the dialog closes,
//! and the only thing left in `self` is which tab is showing — which §6 does
//! not ask anyone to remember, and which every launch deliberately starts at
//! [`SettingsPage::Apps`].

use std::convert::Infallible;
use std::fmt;

use paper_sdk::{Action, App, Canvas, Context, Damage, Event, Rect, SaveError};

use crate::host::SettingsHost;
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

/// The Settings app.
///
/// Owns its [`SettingsHost`] rather than discovering one: which store this
/// reads is a host-side fact (which layout, which root), exactly as Home does
/// not enumerate the catalog for itself. Whoever launches Settings decides
/// whether it is looking at a real store ([`LiveHost`](crate::LiveHost)) or a
/// fixture ([`PlaceholderHost`](crate::PlaceholderHost)), and Settings cannot
/// tell the difference or claim one is the other.
pub struct SettingsApp {
    screen: SettingsScreen,
    host: Box<dyn SettingsHost + Send>,
    layout: Option<SettingsLayout>,
    /// What the last frame drawn showed, or `None` when the next frame has to
    /// be treated as the first one.
    painted: Option<Painted>,
    /// What [`App::damage`] will report for the frame just drawn. Computed in
    /// [`App::draw`] because `damage` takes `&self` and runs after it.
    damage: Damage,
}

impl SettingsApp {
    /// Builds Settings over `host`, showing whatever it reports right now.
    pub fn new(host: impl SettingsHost + Send + 'static) -> Self {
        let host = Box::new(host);
        Self {
            screen: SettingsScreen::from_host(host.as_ref()),
            host,
            layout: None,
            painted: None,
            damage: Damage::Full,
        }
    }

    /// The screen this app is driving.
    pub fn screen(&self) -> &SettingsScreen {
        &self.screen
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
    /// - Anything else: no regions at all. A press that resolved to nothing
    ///   redraws nothing, and saying so is not the same as claiming less than
    ///   changed.
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

/// Where `page`'s tab was drawn.
///
/// [`NavLayout::tabs`](crate::NavLayout::tabs) is in
/// [`SettingsPage::ALL`] order, which is the only correspondence between a
/// page and a rectangle either side of this has.
fn tab_rect(tabs: &[Rect], page: SettingsPage) -> Option<Rect> {
    let index = SettingsPage::ALL.iter().position(|&known| known == page)?;
    tabs.get(index).copied()
}

impl fmt::Debug for SettingsApp {
    /// Written out rather than derived: a [`SettingsHost`] is a trait object
    /// here, and requiring every implementation to be [`Debug`] to get this
    /// one line is a bigger demand on the boundary than the line is worth.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SettingsApp")
            .field("screen", &self.screen)
            .field("painted", &self.painted)
            .field("damage", &self.damage)
            .finish_non_exhaustive()
    }
}

impl App for SettingsApp {
    // Every Host call Settings makes is a local, bounded transaction; nothing
    // here runs on a thread.
    type Completion = Infallible;

    fn event(
        &mut self,
        event: &Event<Self::Completion>,
        context: &mut Context<'_, Self::Completion>,
    ) -> Action {
        match event {
            Event::Pointer(pointer) => {
                let Some(layout) = self.layout.as_ref() else {
                    return Action::None;
                };
                let press = self.screen.press(layout, pointer, self.host.as_mut());
                if let Some(error) = press.error {
                    // The dialog has already closed and the snapshot has
                    // already been refreshed, so the next frame shows the
                    // truth — that the app is still installed, or the grant
                    // still held. What it cannot show is why.
                    context.warn(format!("the host refused the action: {error:?}"));
                }
                press.action
            }
            Event::Resumed => {
                // Whatever was on the glass while this app was away, and
                // whatever changed in the store while it was not looking.
                self.screen.refresh(self.host.as_ref());
                self.painted = None;
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    fn draw(&mut self, canvas: &mut Canvas, _context: &mut Context<'_, Self::Completion>) {
        let layout = crate::screen::render(canvas, &self.screen);
        let now = Painted {
            page: self.screen.page,
            confirming: self.screen.is_confirming(),
        };
        self.damage = Self::damage_between(self.painted, now, &layout, canvas.bounds());
        self.painted = Some(now);
        self.layout = Some(layout);
    }

    fn save(&mut self, _context: &mut Context<'_, Self::Completion>) -> Result<(), SaveError> {
        // See the module doc: there is nothing here that outlives the process.
        Ok(())
    }

    fn damage(&self) -> Damage {
        self.damage.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{Painted, SettingsApp};
    use crate::host::PlaceholderHost;
    use crate::nav::SettingsPage;
    use crate::screen::{PageLayout, SettingsLayout};
    use paper_sdk::{Canvas, Damage, PointerEvent, Rect, SCREEN};
    use paper_sdk::{ContactId, Point, Pointer, PointerPhase};

    fn app() -> SettingsApp {
        SettingsApp::new(PlaceholderHost::new())
    }

    fn canvas() -> Canvas {
        Canvas::new(SCREEN).expect("screen-sized canvas")
    }

    /// Draws one frame the way the runtime does — `draw`, then `damage` —
    /// and hands back both the layout that was drawn and what was claimed.
    ///
    /// `draw` needs a `Context`, which only `paper_sdk::run` can build, so
    /// the tests that only care about drawing call `render` and the damage
    /// arithmetic directly; the ones that care about the whole loop
    /// (`app_runs_a_whole_session`) go through a real session instead.
    fn frame(app: &mut SettingsApp, canvas: &mut Canvas) -> (SettingsLayout, Damage) {
        let layout = crate::screen::render(canvas, &app.screen);
        let now = Painted {
            page: app.screen.page,
            confirming: app.screen.is_confirming(),
        };
        let damage = SettingsApp::damage_between(app.painted, now, &layout, canvas.bounds());
        app.painted = Some(now);
        app.layout = Some(layout.clone());
        (layout, damage)
    }

    fn tap(at: Point) -> PointerEvent {
        PointerEvent::new(at, PointerPhase::Up, Pointer::Touch, ContactId::FIRST)
    }

    fn area(regions: &[Rect]) -> f32 {
        regions.iter().map(|rect| rect.width * rect.height).sum()
    }

    #[test]
    fn the_first_frame_claims_the_whole_viewport() {
        let mut app = app();
        let mut canvas = canvas();
        let (_, damage) = frame(&mut app, &mut canvas);
        assert_eq!(damage, Damage::Full);
    }

    #[test]
    fn a_frame_that_changed_nothing_claims_nothing() {
        let mut app = app();
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
        let mut app = app();
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
        let mut app = app();
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
    fn a_press_before_the_first_frame_does_nothing() {
        let mut session = session::Session::start(app());
        assert_eq!(
            session.pointer(tap(Point::new(1.0, 1.0))),
            None,
            "there was no layout to hit-test against"
        );
        session.finish();
    }

    /// The whole reason this file exists: a real session, over a real socket,
    /// through `paper_sdk::run` — the same loop a launched process runs.
    /// Everything above tests the arithmetic; this tests that Settings is
    /// something the platform can actually start, draw and stop.
    #[test]
    fn a_whole_session_opens_draws_a_page_and_exits_saved() {
        let mut probe_app = app();
        let mut probe = canvas();
        let (layout, _) = frame(&mut probe_app, &mut probe);
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

    /// A real session over a real socket pair, the same shape
    /// `apps/chess/src/app.rs` uses: the SDK's own loop on one side, a
    /// hand-driven host on the other.
    mod session {
        use super::SettingsApp;
        use paper_protocol::{
            AppMessage, AppPaths, DrawReason, DrawRequest, FrameId, Hello, HostMessage,
            LaunchReason, PixelFormat, PointerEvent, Request, SessionId, SurfaceDescriptor, codec,
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
                    // Settings reads the store through its `SettingsHost`, not
                    // through `Storage`: it is granted nothing.
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

            /// Asks for a frame and waits for it.
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
                        _ => continue,
                    }
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
