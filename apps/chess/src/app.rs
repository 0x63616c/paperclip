//! Chess as a real [`App`] (§16).
//!
//! [`ChessScreen::press`] and `paper_chess_rules::Game` hold every decision;
//! this is the wire adapter, plus the one thing that needs a
//! [`Context`]: persistence.

use std::convert::Infallible;

use paper_chess_rules::Game;
use paper_sdk::{Action, App, Canvas, Context, Event, SaveError};

use crate::screen::ChessScreen;

/// The file name a game is saved under, inside the app's own private
/// storage. Not a path: [`paper_sdk::Storage`] resolves names relative to
/// the directory the host granted, exactly as `context.storage().write_private`
/// expects.
const SAVE_FILE: &str = "game.toml";

/// The Chess app.
#[derive(Debug)]
pub struct ChessApp {
    game: Game,
    screen: ChessScreen,
    /// Whether the saved game has been loaded yet.
    ///
    /// Loading needs [`Context::storage`], and the only callback guaranteed
    /// to run before the first frame is [`App::draw`] itself — there is no
    /// `Event::Launched`. `draw`'s own contract says "no filesystem"; this is
    /// the one deliberate, one-time exception to it, guarded so it happens on
    /// the first call and never again. See the WWW-6 report for why this is
    /// flagged rather than hidden: a future protocol revision that gives
    /// launch-time apps a real hook (mirroring [`Event::Resumed`]) should
    /// replace it.
    loaded: bool,
}

impl ChessApp {
    /// A fresh, unloaded app. The actual game is [`Game::new`] until
    /// [`Self::load`] runs, or forever if there was nothing to load.
    pub fn new() -> Self {
        Self {
            game: Game::new(),
            screen: ChessScreen::new(),
            loaded: false,
        }
    }

    fn load(&mut self, context: &mut Context<'_, Infallible>) {
        self.loaded = true;
        let Ok(private) = context.storage().private_dir() else {
            // No `storage` capability: nothing to resume from, and nothing
            // this app can do about it except keep playing unsaved.
            return;
        };
        let path = private.join(SAVE_FILE);
        if !path.exists() {
            return;
        }
        match paper_chess_rules::load(&path) {
            Ok(game) => self.game = game,
            Err(error) => context.warn(format!("could not load the saved game: {error}")),
        }
    }

    fn persist(&mut self, context: &mut Context<'_, Infallible>) -> Result<(), SaveError> {
        let private = context.storage().private_dir()?;
        let path = private.join(SAVE_FILE);
        paper_chess_rules::save(&self.game, &path).map_err(SaveError::new)
    }
}

impl Default for ChessApp {
    fn default() -> Self {
        Self::new()
    }
}

impl App for ChessApp {
    // Chess does no background work of its own.
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
        // hit-testable the instant a game exists, not only after the first
        // `draw` — see `crate::screen::layout`'s own doc.
        let layout = crate::screen::layout(context.viewport(), &self.screen, &self.game);
        let ply_before = self.game.ply();
        let action = self.screen.press(&mut self.game, &layout, pointer);
        // `Action::Redraw` also covers a tap that only changed the
        // selection — nothing worth persisting. Comparing `ply()` is the
        // actual "did the game change" question; New Game resetting it back
        // to 0 still counts, because 0 != whatever it was before.
        if self.game.ply() != ply_before
            && let Err(error) = self.persist(context)
        {
            context.warn(format!("could not save the game: {error}"));
        }
        action
    }

    fn draw(&mut self, canvas: &mut Canvas, context: &mut Context<'_, Self::Completion>) {
        if !self.loaded {
            self.load(context);
        }
        crate::screen::render(canvas, &self.screen, &self.game);
    }

    fn save(&mut self, context: &mut Context<'_, Self::Completion>) -> Result<(), SaveError> {
        self.persist(context)
    }
}

#[cfg(test)]
mod tests {
    use super::ChessApp;
    use paper_chess_rules::{Move, Square};
    use paper_protocol::{
        AppPaths, Capability, ContactId, LaunchReason, PixelFormat, Pointer, PointerEvent,
        PointerPhase, SessionId, SurfaceDescriptor,
    };
    use paper_sdk::{LocalSurfaces, Outcome, run};
    use std::os::unix::net::UnixStream;

    /// Builds the paths a real host would grant, having already created them
    /// — as the host does before launch, and as `AppPaths`'s own doc comment
    /// assumes: an app is handed directories, never told to make its own.
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
        std::os::unix::net::UnixStream,
        std::os::unix::net::UnixStream,
        std::thread::JoinHandle<Result<Outcome, paper_sdk::RuntimeError>>,
    );

    /// Opens a real session over a real socket pair — the same
    /// `paper_sdk::run` loop a launched process runs — so persistence is
    /// proven against the actual `Context`/`Storage` machinery rather than a
    /// hand-built stand-in. Blocks until `Ready` arrives, so anything sent
    /// after this returns is sent to an app that has already mapped its
    /// surface.
    fn start_session(root: &std::path::Path) -> Session {
        let (host_side, app_side) = UnixStream::pair().expect("a socket pair");
        let reader = app_side.try_clone().expect("clone for reading");
        let writer = app_side;
        let handle =
            std::thread::spawn(move || run(ChessApp::new(), reader, writer, LocalSurfaces::new()));

        let mut host_writer = host_side.try_clone().expect("clone for writing");
        let hello = paper_protocol::Hello {
            protocol: paper_protocol::CURRENT,
            session: SessionId::new(1),
            app: "dev.calum.chess".parse().unwrap(),
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
        // `Ready` always comes first.
        let _: paper_protocol::AppMessage =
            paper_protocol::codec::read_message(&mut host_reader).unwrap();
        (host_reader, host_writer, handle)
    }

    /// Sends `event` and reads back the one message it produces.
    ///
    /// A `Draw` always answers with exactly one `Frame`; a `Pointer` that
    /// changed nothing answers with nothing at all (`Action::None` has no
    /// wire form). Every pointer event these tests send is one that moves a
    /// piece, opens a selection, or otherwise asks for a redraw, so it always
    /// answers with exactly one message too.
    fn send(
        host_reader: &mut std::os::unix::net::UnixStream,
        host_writer: &mut std::os::unix::net::UnixStream,
        event: &paper_protocol::HostMessage,
    ) -> paper_protocol::AppMessage {
        paper_protocol::codec::write_message(host_writer, event).unwrap();
        paper_protocol::codec::read_message(host_reader).unwrap()
    }

    /// Ends the session the way a real host always does: `PrepareToExit`,
    /// then the `Saved` it guarantees a reply to.
    fn finish_session(
        mut host_reader: std::os::unix::net::UnixStream,
        mut host_writer: std::os::unix::net::UnixStream,
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

    /// Runs a whole session — open, `events` in order, exit — for tests that
    /// only care about the state once it is over.
    fn run_session(root: &std::path::Path, events: Vec<paper_protocol::HostMessage>) {
        let (mut host_reader, mut host_writer, handle) = start_session(root);
        for event in events {
            send(&mut host_reader, &mut host_writer, &event);
        }
        finish_session(host_reader, host_writer, handle);
    }

    fn pointer_message(at: paper_protocol::Point) -> paper_protocol::HostMessage {
        paper_protocol::HostMessage::Pointer(PointerEvent::new(
            at,
            PointerPhase::Up,
            Pointer::Touch,
            ContactId::FIRST,
        ))
    }

    /// Where a square lands, computed the same pure way
    /// [`ChessApp::event`](super::ChessApp) does — no canvas needed, since
    /// [`crate::screen::layout`] does not draw anything.
    fn square_center(file: u8, rank: u8) -> paper_protocol::Point {
        let layout = crate::screen::layout(
            paper_sdk::SCREEN,
            &crate::screen::ChessScreen::new(),
            &paper_chess_rules::Game::new(),
        );
        layout
            .board
            .square_rect(crate::board::Square::new(file, rank).unwrap())
            .center()
    }

    #[test]
    fn selecting_a_piece_alone_does_not_write_a_save_file() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let e2 = square_center(4, 1);
        let save_path = dir.path().join("private").join("game.toml");

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
        send(&mut host_reader, &mut host_writer, &pointer_message(e2));

        // Checked *before* the exit save runs — that one always writes the
        // game, so its existence afterward would prove nothing about this
        // tap specifically.
        assert!(
            !save_path.exists(),
            "selecting a piece wrote a save file before anything moved"
        );

        finish_session(host_reader, host_writer, handle);
    }

    #[test]
    fn a_move_played_in_one_session_is_there_in_the_next() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let e2 = square_center(4, 1);
        let e4 = square_center(4, 3);

        run_session(
            dir.path(),
            vec![
                paper_protocol::HostMessage::Draw(paper_protocol::DrawRequest {
                    frame: paper_protocol::FrameId::new(1),
                    reason: paper_protocol::DrawReason::First,
                    viewport: paper_sdk::SCREEN,
                }),
                pointer_message(e2),
                pointer_message(e4),
            ],
        );

        let saved = std::fs::read_to_string(dir.path().join("private").join("game.toml"))
            .expect("a save file exists");
        assert!(saved.contains("\"e2\""), "saved: {saved}");
        assert!(saved.contains("\"e4\""), "saved: {saved}");

        let loaded = paper_chess_rules::load(&dir.path().join("private").join("game.toml"))
            .expect("the save loads");
        assert_eq!(
            loaded.piece_at(Square::new(4, 3).unwrap()).map(|(_, k)| k),
            Some(paper_chess_rules::PieceKind::Pawn)
        );
    }

    #[test]
    fn a_second_launch_resumes_the_first_sessions_game() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let mut game = paper_chess_rules::Game::new();
        game.apply(Move::new(
            Square::new(4, 1).unwrap(),
            Square::new(4, 3).unwrap(),
        ))
        .unwrap();
        std::fs::create_dir_all(dir.path().join("private")).unwrap();
        paper_chess_rules::save(&game, &dir.path().join("private").join("game.toml")).unwrap();

        run_session(
            dir.path(),
            vec![paper_protocol::HostMessage::Draw(
                paper_protocol::DrawRequest {
                    frame: paper_protocol::FrameId::new(1),
                    reason: paper_protocol::DrawReason::First,
                    viewport: paper_sdk::SCREEN,
                },
            )],
        );

        // The session above only drew and exited; it never moved. If it had
        // resumed the wrong game (a fresh one) it would have saved *that*
        // over the fixture on its way out, since `save` always writes
        // `self.game`. Instead the file on disk must still be the one move in.
        let on_disk = std::fs::read_to_string(dir.path().join("private").join("game.toml"))
            .expect("still there");
        assert!(on_disk.contains("\"e2\""));
    }
}
