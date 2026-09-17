//! The screens the preview can show, and the state behind them.

use paper_chess::{BoardLayout, ChessScreen};
use paper_home::{HomeScreen, ShelfEntry, ShelfGlyph, ShelfLayout, SystemFact};
use paper_packages::Manifest;
use paper_sdk::{Canvas, PointerEvent, PointerPhase, SCREEN};

use crate::error::CommandError;

/// The manifests of the apps that exist so far, compiled in.
///
/// Read from the real files rather than retyped here: the shelf shows what the
/// app declares, and if a manifest stops parsing this binary stops building.
/// When there is a host and an install directory (Stage 4) this becomes a
/// runtime scan, and the shelf code above it does not change.
const HOME_MANIFEST: &str = include_str!("../../../apps/home/paper.toml");
const CHESS_MANIFEST: &str = include_str!("../../../apps/chess/paper.toml");

/// Which screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Screen {
    /// The home screen.
    Home,
    /// The Chess screen.
    Chess,
}

impl Screen {
    /// The name used for files and log lines.
    pub(crate) fn slug(self) -> &'static str {
        match self {
            Screen::Home => "home",
            Screen::Chess => "chess",
        }
    }

    /// The other screen. With two screens this is the whole navigation model,
    /// and saying so is more honest than a router that routes between two.
    fn toggled(self) -> Self {
        match self {
            Screen::Home => Screen::Chess,
            Screen::Chess => Screen::Home,
        }
    }
}

/// Everything the preview draws, and the small amount of state behind it.
#[derive(Debug)]
pub(crate) struct Screens {
    current: Screen,
    home: HomeScreen,
    chess: ChessScreen,
    shelf: Option<ShelfLayout>,
    board: Option<BoardLayout>,
}

impl Screens {
    /// Builds the screens from the compiled-in manifests.
    pub(crate) fn new() -> Result<Self, CommandError> {
        let home_manifest = parse_built_in("home", HOME_MANIFEST)?;
        let chess_manifest = parse_built_in("chess", CHESS_MANIFEST)?;

        let home = HomeScreen {
            entries: vec![
                ShelfEntry::from_manifest(&chess_manifest, ShelfGlyph::Board),
                ShelfEntry::action("App Store", "NOT INSTALLED", ShelfGlyph::Store),
                ShelfEntry::action("Return to stock", "REMARKABLE", ShelfGlyph::Stock),
            ],
            facts: vec![
                SystemFact::new("Display", format!("{} x {}", SCREEN.width, SCREEN.height)),
                SystemFact::new("Renderer", "Software / CPU"),
                SystemFact::new("App protocol", paper_protocol::CURRENT.to_string()),
                SystemFact::new("Home", format!("V{}", home_manifest.version())),
                SystemFact::new("Device", "Not verified"),
            ],
            pressed: None,
            status: format!("V{}", home_manifest.version()),
        };

        Ok(Self {
            current: Screen::Home,
            home,
            chess: ChessScreen::new(),
            shelf: None,
            board: None,
        })
    }

    /// Shows a specific screen.
    pub(crate) fn show(&mut self, screen: Screen) {
        self.current = screen;
    }

    /// Draws the current screen, remembering the layouts for hit-testing.
    pub(crate) fn render(&mut self, canvas: &mut Canvas) {
        match self.current {
            Screen::Home => self.shelf = Some(paper_home::render(canvas, &self.home)),
            Screen::Chess => self.board = Some(paper_chess::render(canvas, &self.chess)),
        }
    }

    /// Draws one screen onto a fresh canvas at full target resolution.
    pub(crate) fn render_offscreen(&mut self, screen: Screen) -> Result<Canvas, CommandError> {
        let mut canvas = Canvas::new(SCREEN).ok_or(CommandError::Canvas {
            width: SCREEN.width,
            height: SCREEN.height,
        })?;
        let previous = self.current;
        self.current = screen;
        self.render(&mut canvas);
        self.current = previous;
        Ok(canvas)
    }

    /// Feeds a pointer event to whichever screen is showing.
    pub(crate) fn pointer(&mut self, event: PointerEvent) {
        match self.current {
            Screen::Home => {
                let Some(shelf) = self.shelf.as_ref() else {
                    return;
                };
                let hit = shelf.hit_test(event.at);
                match event.phase {
                    PointerPhase::Down | PointerPhase::Moved => self.home.pressed = hit,
                    PointerPhase::Cancelled => self.home.pressed = None,
                    PointerPhase::Up => {
                        self.home.pressed = None;
                        // Only the Chess tile leads anywhere in Stage 1. The
                        // other two are drawn because the shelf is the
                        // deliverable, not because they work.
                        if hit == Some(0) {
                            self.current = Screen::Chess;
                        }
                    }
                }
            }
            Screen::Chess => {
                if let (Some(board), PointerPhase::Up) = (self.board, event.phase) {
                    self.chess.press(board, event.at);
                }
            }
        }
    }

    /// Handles a key press in the preview.
    pub(crate) fn key(&mut self, key: char) {
        match key {
            'h' => self.current = Screen::Home,
            'c' => self.current = Screen::Chess,
            'f' => self.chess.flip(),
            'n' => self.chess.selected = None,
            '\t' | ' ' => self.current = self.current.toggled(),
            _ => {}
        }
    }
}

fn parse_built_in(app: &'static str, text: &str) -> Result<Manifest, CommandError> {
    let manifest =
        Manifest::parse(text).map_err(|source| CommandError::BuiltInManifest { app, source })?;
    manifest
        .ensure_runnable()
        .map_err(|source| CommandError::BuiltInManifest { app, source })?;
    Ok(manifest)
}
