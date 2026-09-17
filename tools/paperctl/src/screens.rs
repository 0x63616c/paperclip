//! The screens the preview can show, and the state behind them.

use paper_chess::{BoardLayout, ChessScreen};
use paper_home::{HomeScreen, ShelfEntry, ShelfGlyph, ShelfLayout, SystemFact};
use paper_packages::Manifest;
use paper_sdk::{Canvas, PointerEvent, PointerPhase, SCREEN};
use paper_settings::{PageLayout, PlaceholderHost, SettingsLayout, SettingsScreen};

use crate::error::CommandError;

/// The manifests of the apps that exist so far, compiled in.
///
/// Read from the real files rather than retyped here: the shelf shows what the
/// app declares, and if a manifest stops parsing this binary stops building.
/// When there is a host and an install directory (Stage 4) this becomes a
/// runtime scan, and the shelf code above it does not change.
const HOME_MANIFEST: &str = include_str!("../../../apps/home/paper.toml");
const CHESS_MANIFEST: &str = include_str!("../../../apps/chess/paper.toml");
const SETTINGS_MANIFEST: &str = include_str!("../../../apps/settings/paper.toml");

/// Which screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Screen {
    /// The home screen.
    Home,
    /// The Chess screen.
    Chess,
    /// The Settings screen.
    Settings,
}

impl Screen {
    /// Every screen the preview can show, in cycling order.
    const ALL: [Screen; 3] = [Screen::Home, Screen::Chess, Screen::Settings];

    /// The name used for files and log lines.
    pub(crate) fn slug(self) -> &'static str {
        match self {
            Screen::Home => "home",
            Screen::Chess => "chess",
            Screen::Settings => "settings",
        }
    }

    /// The next screen in cycling order.
    fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|&screen| screen == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

/// Everything the preview draws, and the small amount of state behind it.
#[derive(Debug)]
pub(crate) struct Screens {
    current: Screen,
    home: HomeScreen,
    chess: ChessScreen,
    settings: SettingsScreen,
    settings_host: PlaceholderHost,
    shelf: Option<ShelfLayout>,
    board: Option<BoardLayout>,
    settings_layout: Option<SettingsLayout>,
}

impl Screens {
    /// Builds the screens from the compiled-in manifests.
    pub(crate) fn new() -> Result<Self, CommandError> {
        let home_manifest = parse_built_in("home", HOME_MANIFEST)?;
        let chess_manifest = parse_built_in("chess", CHESS_MANIFEST)?;
        parse_built_in("settings", SETTINGS_MANIFEST)?;

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

        let settings_host = PlaceholderHost::new();
        let settings = SettingsScreen::from_host(&settings_host);

        Ok(Self {
            current: Screen::Home,
            home,
            chess: ChessScreen::new(),
            settings,
            settings_host,
            shelf: None,
            board: None,
            settings_layout: None,
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
            Screen::Settings => {
                self.settings_layout = Some(paper_settings::render(canvas, &self.settings))
            }
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

    /// Renders every Settings page, plus the uninstall confirmation, each to
    /// its own full-resolution canvas — "desktop preview screens for every
    /// page" (WWW-22) captured without opening a window.
    ///
    /// The confirmation is reached through [`SettingsScreen::press_apps`]
    /// exactly as a real tap would, not by constructing a dialog by hand: what
    /// this produces is what pressing UNINSTALL actually shows.
    pub(crate) fn render_settings_pages(&mut self) -> Result<Vec<(String, Canvas)>, CommandError> {
        let blank = || {
            Canvas::new(SCREEN).ok_or(CommandError::Canvas {
                width: SCREEN.width,
                height: SCREEN.height,
            })
        };
        let mut frames = Vec::new();
        for page in paper_settings::SettingsPage::ALL {
            self.settings.page = page;
            let mut canvas = blank()?;
            let layout = paper_settings::render(&mut canvas, &self.settings);
            let slug = format!(
                "settings-{}",
                page.heading().to_lowercase().replace(' ', "-")
            );
            frames.push((slug, canvas));

            if let PageLayout::Apps(apps) = &layout.page
                && let Some(row) = apps.rows.first()
            {
                self.settings.press_apps(apps, row.uninstall.center());
                let mut confirming = blank()?;
                paper_settings::render(&mut confirming, &self.settings);
                frames.push(("settings-confirm-uninstall".to_owned(), confirming));
                self.settings.cancel();
            }
        }
        self.settings.page = paper_settings::SettingsPage::Apps;
        Ok(frames)
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
                    // A hovering pen has not pressed anything. Highlighting a
                    // tile under it would have the shelf respond to the pen
                    // being near the glass, which is not what a tap is.
                    PointerPhase::Hover => {}
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
            Screen::Settings => {
                let (Some(layout), PointerPhase::Up) = (&self.settings_layout, event.phase) else {
                    return;
                };
                if let Some(confirm) = &layout.confirm {
                    if confirm.cancel.contains(event.at) {
                        self.settings.cancel();
                    } else if confirm.confirm.contains(event.at) {
                        // Fixture data only: nothing here reaches WWW-7. A
                        // failed placeholder transaction has nothing useful to
                        // report to a preview window, so it is dropped rather
                        // than surfaced.
                        let _ = self.settings.confirm(&mut self.settings_host);
                    }
                    return;
                }
                self.settings.press_tab(&layout.nav, event.at);
                match &layout.page {
                    PageLayout::Apps(apps) => self.settings.press_apps(apps, event.at),
                    PageLayout::Grants(grants) => self.settings.press_grants(grants, event.at),
                    PageLayout::ReadOnly => {}
                }
                if layout
                    .return_to_stock
                    .is_some_and(|rect| rect.contains(event.at))
                {
                    self.current = Screen::Home;
                }
            }
        }
    }

    /// Handles a key press in the preview.
    pub(crate) fn key(&mut self, key: char) {
        match key {
            'h' => self.current = Screen::Home,
            'c' => self.current = Screen::Chess,
            's' => self.current = Screen::Settings,
            'f' => self.chess.flip(),
            'n' => self.chess.selected = None,
            '\t' | ' ' => self.current = self.current.next(),
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
