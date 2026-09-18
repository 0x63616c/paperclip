//! The render test card as a real [`App`].
//!
//! Every decision a press makes lives in [`crate::screen::press`]; what is
//! here is the wire adapter: feed pointer events in, draw on request, report
//! what changed. There is nothing to save — every section's state (which tab
//! is showing, the ghosting counter, the damage grid's fill) exists only to
//! be looked at once and is worth nothing to a future session. That mirrors
//! `paper_settings::SettingsApp`'s own "nothing to save" — see its module doc
//! for the fuller argument.

use std::convert::Infallible;

use paper_sdk::{Action, App, Canvas, Context, Damage, DamageAccumulator, Event, SaveError};

use crate::screen::RenderTestCardScreen;

/// The render test card app.
#[derive(Debug)]
pub struct RenderTestCardApp {
    screen: RenderTestCardScreen,
    /// What has changed since the last frame was published, under ADR-0021's
    /// rules — see [`DamageAccumulator`]'s own doc.
    damage_claims: DamageAccumulator,
    /// What the frame just drawn changed — [`App::damage`]'s answer.
    ///
    /// Held separately because `damage` is asked after `draw`, by which point
    /// `damage_claims` has already been reset for the next frame by
    /// [`DamageAccumulator::take_frame`].
    frame: Damage,
}

impl RenderTestCardApp {
    /// A fresh card, opening on `Section::Greyscale`.
    pub fn new() -> Self {
        Self {
            screen: RenderTestCardScreen::default(),
            damage_claims: DamageAccumulator::new(),
            frame: Damage::Full,
        }
    }
}

impl Default for RenderTestCardApp {
    fn default() -> Self {
        Self::new()
    }
}

impl App for RenderTestCardApp {
    type Completion = Infallible;

    fn event(
        &mut self,
        event: &Event<Self::Completion>,
        context: &mut Context<'_, Self::Completion>,
    ) -> Action {
        match event {
            Event::Pointer(pointer) => {
                // Pure and cheap: no canvas, no drawn frame to wait for. A
                // tap is hit-testable the instant the card exists, not only
                // after the first `draw` — see `crate::screen::layout`'s own
                // doc.
                let layout = crate::screen::layout(context.viewport(), &self.screen);
                let press = crate::screen::press(&mut self.screen, &layout, pointer);
                self.damage_claims.claim(press.damage);
                press.action
            }
            // Whatever was on the glass while this app was away is not
            // something it can reason about, so the next frame claims all of
            // it — same reasoning as `SudokuApp`'s.
            Event::Suspended | Event::Resumed => {
                self.damage_claims.claim(Damage::Full);
                Action::None
            }
            _ => Action::None,
        }
    }

    fn draw(&mut self, canvas: &mut Canvas, context: &mut Context<'_, Self::Completion>) {
        crate::screen::render(canvas, &self.screen, context.surface());
        self.frame = self.damage_claims.take_frame();
    }

    fn save(&mut self, _context: &mut Context<'_, Self::Completion>) -> Result<(), SaveError> {
        Ok(())
    }

    fn damage(&self) -> Damage {
        self.frame.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::RenderTestCardApp;
    use crate::nav::Section;
    use paper_protocol::{
        AppPaths, Capability, ContactId, Damage, DrawReason, DrawRequest, FrameId, LaunchReason,
        PixelFormat, Point, Pointer, PointerEvent, PointerPhase, SessionId, SurfaceDescriptor,
    };
    use paper_sdk::{LocalSurfaces, Outcome, run};
    use std::os::unix::net::UnixStream;

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

    fn start_session(root: &std::path::Path) -> Session {
        let (host_side, app_side) = UnixStream::pair().expect("a socket pair");
        let reader = app_side.try_clone().expect("clone for reading");
        let writer = app_side;
        let handle = std::thread::spawn(move || {
            run(
                RenderTestCardApp::new(),
                reader,
                writer,
                LocalSurfaces::new(),
            )
        });

        let mut host_writer = host_side.try_clone().expect("clone for writing");
        let hello = paper_protocol::Hello {
            protocol: paper_protocol::CURRENT,
            session: SessionId::new(1),
            app: "dev.calum.render-test-card".parse().unwrap(),
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

    fn draw_request(frame: u64) -> paper_protocol::HostMessage {
        paper_protocol::HostMessage::Draw(DrawRequest {
            frame: FrameId::new(frame),
            reason: if frame == 1 {
                DrawReason::First
            } else {
                DrawReason::AppRequested
            },
            viewport: paper_sdk::SCREEN,
        })
    }

    fn pointer_message(at: Point) -> paper_protocol::HostMessage {
        paper_protocol::HostMessage::Pointer(PointerEvent::new(
            at,
            PointerPhase::Up,
            Pointer::Touch,
            ContactId::FIRST,
        ))
    }

    fn frame_damage(message: &paper_protocol::AppMessage) -> &Damage {
        match message {
            paper_protocol::AppMessage::Frame(frame) => &frame.damage,
            other => panic!("expected a frame, got {other:?}"),
        }
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

    #[test]
    fn the_first_frame_claims_the_whole_viewport() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let (mut host_reader, mut host_writer, handle) = start_session(dir.path());
        let first = send(&mut host_reader, &mut host_writer, &draw_request(1));
        assert_eq!(
            frame_damage(&first),
            &Damage::Full,
            "nothing is on the glass yet, so nothing can be reused"
        );
        finish_session(host_reader, host_writer, handle);
    }

    #[test]
    fn switching_sections_over_the_wire_claims_the_whole_viewport() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let (mut host_reader, mut host_writer, handle) = start_session(dir.path());
        send(&mut host_reader, &mut host_writer, &draw_request(1));

        // The Colour tab: seven equal tabs across the full width, tab index 1.
        let width = paper_sdk::SCREEN.width as f32 / Section::ALL.len() as f32;
        let colour_tab_center = Point::new(width * 1.5, 132.0 + 60.0);
        send(
            &mut host_reader,
            &mut host_writer,
            &pointer_message(colour_tab_center),
        );
        let switched = send(&mut host_reader, &mut host_writer, &draw_request(2));
        assert_eq!(frame_damage(&switched), &Damage::Full);

        finish_session(host_reader, host_writer, handle);
    }

    #[test]
    fn a_resumed_app_claims_the_whole_viewport_again() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let (mut host_reader, mut host_writer, handle) = start_session(dir.path());
        send(&mut host_reader, &mut host_writer, &draw_request(1));
        paper_protocol::codec::write_message(
            &mut host_writer,
            &paper_protocol::HostMessage::Lifecycle(paper_protocol::LifecycleEvent::Suspended),
        )
        .unwrap();
        paper_protocol::codec::write_message(
            &mut host_writer,
            &paper_protocol::HostMessage::Lifecycle(paper_protocol::LifecycleEvent::Resumed),
        )
        .unwrap();
        let resumed = send(&mut host_reader, &mut host_writer, &draw_request(2));
        assert_eq!(frame_damage(&resumed), &Damage::Full);

        finish_session(host_reader, host_writer, handle);
    }

    #[test]
    fn home_from_the_footer_asks_the_platform_for_home() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let (mut host_reader, mut host_writer, handle) = start_session(dir.path());
        send(&mut host_reader, &mut host_writer, &draw_request(1));

        // The footer's one action spans the width, inset by the chrome
        // margin; its vertical band is the bottom `FOOTER_HEIGHT` of the
        // screen (§7 chrome).
        let home_center = Point::new(
            paper_sdk::SCREEN.width as f32 / 2.0,
            paper_sdk::SCREEN.height as f32 - 90.0,
        );
        let request = send(
            &mut host_reader,
            &mut host_writer,
            &pointer_message(home_center),
        );
        assert!(matches!(
            request,
            paper_protocol::AppMessage::Request(paper_protocol::Request::Home)
        ));
        finish_session(host_reader, host_writer, handle);
    }
}
