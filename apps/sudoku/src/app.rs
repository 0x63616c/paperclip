//! Sudoku as a real [`App`], and the per-frame damage claim that is the point
//! of it.
//!
//! [`SudokuScreen::press`] and `paper_sudoku_rules::Game` hold every decision.
//! What is here is the wire adapter, persistence, and the one thing neither of
//! them can do: carry what a press changed forward to the moment the host asks
//! what the frame changed.

use std::convert::Infallible;

use paper_sdk::{Action, App, Canvas, Context, Damage, DamageAccumulator, Event, SaveError};
use paper_sudoku_rules::{Difficulty, Game};

use crate::screen::SudokuScreen;

/// The file name a puzzle is saved under, inside the app's own private
/// storage. Not a path: [`paper_sdk::Storage`] resolves names relative to the
/// directory the host granted.
const SAVE_FILE: &str = "puzzle.toml";

/// What a first launch generates, before anyone has chosen a level.
const FIRST_PUZZLE: Difficulty = Difficulty::Easy;

/// The Sudoku app.
#[derive(Debug)]
pub struct SudokuApp {
    game: Game,
    screen: SudokuScreen,
    /// What has changed since the last frame was published, under ADR-0021's
    /// rules — see [`DamageAccumulator`]'s own doc.
    damage_claims: DamageAccumulator,
    /// What the frame just drawn changed — [`App::damage`]'s answer.
    ///
    /// Held separately because `damage` is asked after `draw`, by which point
    /// `damage_claims` has already been reset for the next frame by
    /// [`DamageAccumulator::take_frame`].
    frame: Damage,
    /// Whether the saved puzzle has been loaded yet.
    ///
    /// Loading needs [`Context::storage`], and the only callback guaranteed to
    /// run before the first frame is [`App::draw`] — there is no
    /// `Event::Launched`. `draw`'s contract says "no filesystem"; this is the
    /// same deliberate, one-time exception `paper_chess::ChessApp` documents,
    /// guarded so it happens once and never again.
    loaded: bool,
}

impl SudokuApp {
    /// A fresh app on a puzzle nobody has seen before.
    ///
    /// The seed comes from the OS. If entropy is unavailable — which on this
    /// platform means `getrandom` failed, not that it blocked — the app falls
    /// back to a fixed seed rather than refusing to start: a Sudoku player is
    /// better served by a puzzle they may have seen than by no puzzle.
    pub fn new() -> Self {
        let mut bytes = [0u8; 8];
        let seed = match getrandom::fill(&mut bytes) {
            Ok(()) => u64::from_le_bytes(bytes),
            Err(_) => 0x5344_4F4B_5530_3031,
        };
        Self::with_seed(seed)
    }

    /// A fresh app on the puzzle `seed` generates.
    ///
    /// Deterministic, which is what the golden-frame table and every test
    /// here need.
    pub fn with_seed(seed: u64) -> Self {
        let game = Game::start(FIRST_PUZZLE, seed);
        Self {
            screen: SudokuScreen::new(game.difficulty()),
            game,
            damage_claims: DamageAccumulator::new(),
            frame: Damage::Full,
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
        match paper_sudoku_rules::load(&path) {
            Ok(game) => {
                self.screen = SudokuScreen::new(game.difficulty());
                self.game = game;
            }
            Err(error) => context.warn(format!("could not load the saved puzzle: {error}")),
        }
    }

    fn persist(&mut self, context: &mut Context<'_, Infallible>) -> Result<(), SaveError> {
        let private = context.storage().private_dir()?;
        let path = private.join(SAVE_FILE);
        paper_sudoku_rules::save(&self.game, &path).map_err(SaveError::new)
    }
}

impl Default for SudokuApp {
    fn default() -> Self {
        Self::new()
    }
}

impl App for SudokuApp {
    // Sudoku does no background work: generation is fast enough to run on the
    // UI thread. Measured on this workspace's own tests, a hard puzzle —
    // which is the expensive case, because every removed clue costs a
    // uniqueness count — generates in single-digit milliseconds in a debug
    // build. If that ever stops being true the answer is `Context::spawn` and
    // a real `Completion` type, not a slower tap.
    type Completion = Infallible;

    fn event(
        &mut self,
        event: &Event<Self::Completion>,
        context: &mut Context<'_, Self::Completion>,
    ) -> Action {
        if !self.loaded {
            self.load(context);
        }
        match event {
            Event::Pointer(pointer) => {
                // Pure and cheap: no canvas, no drawn frame to wait for. A
                // tap is hit-testable the instant a puzzle exists, not only
                // after the first `draw` — see `crate::screen::layout`'s own
                // doc.
                let layout = crate::screen::layout(context.viewport());
                let revision = self.game.revision();
                let press = self.screen.press(&mut self.game, &layout, pointer);
                self.damage_claims.claim(press.damage);
                // Comparing the revision is the "did the puzzle change"
                // question: a tap that only moved the selection, or that was
                // refused, has nothing worth writing to storage.
                if self.game.revision() != revision
                    && let Err(error) = self.persist(context)
                {
                    context.warn(format!("could not save the puzzle: {error}"));
                }
                press.action
            }
            // Whatever was on the glass while this app was away is not
            // something it can reason about, so the next frame claims all of
            // it. The host sends its own draw request once the panel is
            // presentable (see `Event::Resumed`), which is why this asks for
            // nothing.
            Event::Suspended | Event::Resumed => {
                self.damage_claims.claim(Damage::Full);
                Action::None
            }
            _ => Action::None,
        }
    }

    fn draw(&mut self, canvas: &mut Canvas, context: &mut Context<'_, Self::Completion>) {
        if !self.loaded {
            self.load(context);
        }
        crate::screen::render(canvas, &self.screen, &self.game);
        self.frame = self.damage_claims.take_frame();
    }

    fn save(&mut self, context: &mut Context<'_, Self::Completion>) -> Result<(), SaveError> {
        self.persist(context)
    }

    fn damage(&self) -> Damage {
        self.frame.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::SudokuApp;
    use paper_protocol::{
        AppPaths, Capability, ContactId, Damage, DrawReason, DrawRequest, FrameId, LaunchReason,
        PixelFormat, Point, Pointer, PointerEvent, PointerPhase, Rect, SessionId,
        SurfaceDescriptor,
    };
    use paper_sdk::{LocalSurfaces, Outcome, run};
    use paper_sudoku_rules::{Cell, Difficulty, Digit, Game};
    use std::os::unix::net::UnixStream;

    /// The seed every session test starts from, so the puzzle under test is
    /// the same puzzle the probe render measured.
    const SEED: u64 = 9_001;

    /// Builds the paths a real host would grant, having already created them.
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
            run(
                SudokuApp::with_seed(SEED),
                reader,
                writer,
                LocalSurfaces::new(),
            )
        });

        let mut host_writer = host_side.try_clone().expect("clone for writing");
        let hello = paper_protocol::Hello {
            protocol: paper_protocol::CURRENT,
            session: SessionId::new(1),
            app: "dev.calum.sudoku".parse().unwrap(),
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

    /// What the app published, read out of a `Frame` message.
    fn frame_damage(message: &paper_protocol::AppMessage) -> &Damage {
        match message {
            paper_protocol::AppMessage::Frame(frame) => &frame.damage,
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    /// Ends the session the way a real host does.
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

    /// The game a session on [`SEED`] starts from, and the layout a real
    /// session sees — computed the same pure way [`SudokuApp::event`] does,
    /// no canvas needed, since `crate::screen::layout` does not draw
    /// anything.
    fn probe() -> (Game, crate::SudokuLayout) {
        let game = Game::start(Difficulty::Easy, SEED);
        let layout = crate::screen::layout(paper_sdk::SCREEN);
        (game, layout)
    }

    /// An empty cell, a digit legal in it, and where both are on screen.
    fn playable(game: &Game, layout: &crate::SudokuLayout) -> (Cell, Rect, Rect) {
        let (cell, digit) = Cell::all()
            .filter(|cell| game.digit_at(*cell).is_none())
            .find_map(|cell| {
                Digit::ALL
                    .into_iter()
                    .find(|digit| game.board().accepts(cell, *digit))
                    .map(|digit| (cell, digit))
            })
            .expect("some empty cell takes some digit");
        (
            cell,
            layout.grid.cell_rect(cell),
            layout
                .pad
                .key_rect(crate::PadKey::Digit(digit))
                .expect("the pad has that digit"),
        )
    }

    #[test]
    fn selecting_a_cell_alone_does_not_write_a_save_file() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let (game, layout) = probe();
        let (_, cell_rect, _) = playable(&game, &layout);
        let save_path = dir.path().join("private").join("puzzle.toml");

        let (mut host_reader, mut host_writer, handle) = start_session(dir.path());
        send(&mut host_reader, &mut host_writer, &draw_request(1));
        send(
            &mut host_reader,
            &mut host_writer,
            &pointer_message(cell_rect.center()),
        );

        // Checked before the exit save, which always writes.
        assert!(
            !save_path.exists(),
            "selecting a cell wrote a save file before anything was entered"
        );
        finish_session(host_reader, host_writer, handle);
    }

    #[test]
    fn a_digit_entered_in_one_session_is_there_in_the_next() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let (game, layout) = probe();
        let (cell, cell_rect, key_rect) = playable(&game, &layout);
        let expected = {
            let mut played = game.clone();
            let digit = Digit::ALL
                .into_iter()
                .find(|digit| played.board().accepts(cell, *digit))
                .expect("something is placeable");
            played.set(cell, digit).expect("a legal entry");
            digit
        };

        let (mut host_reader, mut host_writer, handle) = start_session(dir.path());
        send(&mut host_reader, &mut host_writer, &draw_request(1));
        send(
            &mut host_reader,
            &mut host_writer,
            &pointer_message(cell_rect.center()),
        );
        send(&mut host_reader, &mut host_writer, &draw_request(2));
        send(
            &mut host_reader,
            &mut host_writer,
            &pointer_message(key_rect.center()),
        );
        finish_session(host_reader, host_writer, handle);

        let saved = paper_sudoku_rules::load(&dir.path().join("private").join("puzzle.toml"))
            .expect("the save loads");
        assert_eq!(saved.digit_at(cell), Some(expected));
        assert_eq!(saved.entered(), 1);
        assert_eq!(
            saved.puzzle(),
            game.puzzle(),
            "a different puzzle was saved"
        );
    }

    #[test]
    fn a_second_launch_resumes_the_first_sessions_puzzle() {
        let dir = tempfile::tempdir().expect("a temp dir");
        // A puzzle from a different seed entirely, so resuming it cannot be
        // confused with generating the session's own.
        let mut fixture = Game::start(Difficulty::Hard, 77);
        let cell = Cell::all()
            .find(|cell| fixture.digit_at(*cell).is_none())
            .expect("a puzzle has empty cells");
        let digit = Digit::ALL
            .into_iter()
            .find(|digit| fixture.board().accepts(cell, *digit))
            .expect("something is placeable");
        fixture.set(cell, digit).expect("a legal entry");
        std::fs::create_dir_all(dir.path().join("private")).unwrap();
        paper_sudoku_rules::save(&fixture, &dir.path().join("private").join("puzzle.toml"))
            .expect("the fixture saves");

        let (mut host_reader, mut host_writer, handle) = start_session(dir.path());
        send(&mut host_reader, &mut host_writer, &draw_request(1));
        finish_session(host_reader, host_writer, handle);

        // The session only drew and exited. If it had started its own puzzle
        // instead of resuming, its exit save would have written that one over
        // the fixture.
        let after = paper_sudoku_rules::load(&dir.path().join("private").join("puzzle.toml"))
            .expect("still loads");
        assert_eq!(after.puzzle(), fixture.puzzle());
        assert_eq!(after.digit_at(cell), Some(digit));
        assert_eq!(after.difficulty(), Difficulty::Hard);
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
    fn the_frame_after_a_digit_claims_one_cell_on_the_wire() {
        // The whole point of this app, asserted where it actually matters: on
        // the `FrameDone` the host reads. One region, and it is the cell.
        let dir = tempfile::tempdir().expect("a temp dir");
        let (game, layout) = probe();
        let (cell, cell_rect, key_rect) = playable(&game, &layout);

        let (mut host_reader, mut host_writer, handle) = start_session(dir.path());
        send(&mut host_reader, &mut host_writer, &draw_request(1));

        send(
            &mut host_reader,
            &mut host_writer,
            &pointer_message(cell_rect.center()),
        );
        let selection = send(&mut host_reader, &mut host_writer, &draw_request(2));
        assert_eq!(
            frame_damage(&selection),
            &Damage::Regions {
                regions: vec![cell_rect]
            },
            "selecting {} should claim only that cell",
            cell.name()
        );

        send(
            &mut host_reader,
            &mut host_writer,
            &pointer_message(key_rect.center()),
        );
        let entry = send(&mut host_reader, &mut host_writer, &draw_request(3));
        assert_eq!(
            frame_damage(&entry),
            &Damage::Regions {
                regions: vec![cell_rect]
            },
            "entering a digit in {} should claim only that cell",
            cell.name()
        );

        // A draw nobody asked for has no claim behind it, and must not
        // present nothing.
        let unprompted = send(&mut host_reader, &mut host_writer, &draw_request(4));
        assert_eq!(frame_damage(&unprompted), &Damage::Full);

        finish_session(host_reader, host_writer, handle);
    }

    #[test]
    fn a_resumed_app_claims_the_whole_viewport_again() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let (game, layout) = probe();
        let (_, cell_rect, _) = playable(&game, &layout);

        let (mut host_reader, mut host_writer, handle) = start_session(dir.path());
        send(&mut host_reader, &mut host_writer, &draw_request(1));
        send(
            &mut host_reader,
            &mut host_writer,
            &pointer_message(cell_rect.center()),
        );
        // Away and back between the press and the frame: whatever was on the
        // panel meanwhile is not this app's to reason about.
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
}
