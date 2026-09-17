//! The screens the preview can show, and the state behind them.

use paper_app_store::{AppStoreScreen, StoreLayout};
use paper_chess::{ChessLayout, ChessScreen};
use paper_chess_rules::Game;
use paper_home::{HomeScreen, ShelfEntry, ShelfGlyph, ShelfLayout, SystemFact};
use paper_packages::Manifest;
use paper_packages::inventory::{AppEntry, CatalogStatus, Inventory};
#[cfg(feature = "desktop")]
use paper_sdk::{Action, PointerEvent, PointerPhase};
use paper_sdk::{Canvas, SCREEN};
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
const APP_STORE_MANIFEST: &str = include_str!("../../../apps/app-store/paper.toml");

/// Which screen is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Screen {
    /// The home screen.
    Home,
    /// The Chess screen.
    Chess,
    /// The Settings screen.
    Settings,
    /// The App Store screen.
    AppStore,
}

impl Screen {
    /// Every screen the preview can show, in cycling order.
    #[cfg(feature = "desktop")]
    const ALL: [Screen; 4] = [
        Screen::Home,
        Screen::Chess,
        Screen::Settings,
        Screen::AppStore,
    ];

    /// The name used for files and log lines.
    pub(crate) fn slug(self) -> &'static str {
        match self {
            Screen::Home => "home",
            Screen::Chess => "chess",
            Screen::Settings => "settings",
            Screen::AppStore => "app-store",
        }
    }

    /// The next screen in cycling order.
    #[cfg(feature = "desktop")]
    fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|&screen| screen == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

/// What the App Store shows in the preview.
///
/// A fixture, and labelled one: there is no store and no catalog on a
/// developer's Mac by default, and surveying nothing would draw an empty
/// screen that demonstrates nothing. The real screen is fed by
/// `PackagesSource` against a real store — see `apps/app-store/tests/store.rs`,
/// which drives exactly that path.
fn preview_inventory(chess: &Manifest, app_store: &Manifest) -> Inventory {
    Inventory::fixture(
        vec![
            // Installed, and the catalog has moved on.
            AppEntry::fixture(
                chess.id().clone(),
                chess.name().clone(),
                Some(chess.version().clone()),
                Some(semver::Version::new(0, 2, 0)),
            ),
            // A default app, current.
            AppEntry::fixture(
                app_store.id().clone(),
                app_store.name().clone(),
                Some(app_store.version().clone()),
                Some(app_store.version().clone()),
            ),
        ],
        Some(CatalogStatus {
            name: "calum-home".to_owned(),
            serial: 7,
            stale: false,
        }),
    )
}

/// Everything the preview draws, and the small amount of state behind it.
#[derive(Debug)]
pub(crate) struct Screens {
    current: Screen,
    home: HomeScreen,
    chess: ChessScreen,
    chess_game: Game,
    settings: SettingsScreen,
    // Only the preview window drives Settings interactively; the device build
    // renders and presents, so it carries neither the host nor the handlers.
    #[cfg(feature = "desktop")]
    settings_host: PlaceholderHost,
    app_store: AppStoreScreen,
    shelf: Option<ShelfLayout>,
    board: Option<ChessLayout>,
    settings_layout: Option<SettingsLayout>,
    app_store_layout: Option<StoreLayout>,
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
        let app_store_manifest = parse_built_in("app-store", APP_STORE_MANIFEST)?;
        let app_store =
            AppStoreScreen::new(preview_inventory(&chess_manifest, &app_store_manifest));

        Ok(Self {
            current: Screen::Home,
            home,
            chess: ChessScreen::new(),
            chess_game: Game::new(),
            settings,
            #[cfg(feature = "desktop")]
            settings_host,
            app_store,
            shelf: None,
            board: None,
            settings_layout: None,
            app_store_layout: None,
        })
    }

    /// Shows a specific screen.
    #[cfg(feature = "desktop")]
    pub(crate) fn show(&mut self, screen: Screen) {
        self.current = screen;
    }

    /// Draws the current screen, remembering the layouts for hit-testing.
    pub(crate) fn render(&mut self, canvas: &mut Canvas) {
        match self.current {
            Screen::Home => self.shelf = Some(paper_home::render(canvas, &self.home)),
            Screen::Chess => {
                self.board = Some(paper_chess::render(canvas, &self.chess, &self.chess_game))
            }
            Screen::Settings => {
                self.settings_layout = Some(paper_settings::render(canvas, &self.settings))
            }
            Screen::AppStore => {
                self.app_store_layout = Some(paper_app_store::render(canvas, &self.app_store))
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
    #[cfg(feature = "desktop")]
    pub(crate) fn pointer(&mut self, event: PointerEvent) {
        match self.current {
            Screen::Home => {
                let Some(shelf) = self.shelf.clone() else {
                    return;
                };
                // `HomeScreen::press` is the real interaction logic (ADR-0018);
                // the preview just has to act on what it asks for, the same
                // way a real host would.
                if let Action::Launch(id) = self.home.press(&shelf, &event)
                    && id.as_str() == "dev.calum.chess"
                {
                    self.current = Screen::Chess;
                }
            }
            Screen::Chess => {
                if let Some(board) = self.board
                    && matches!(
                        self.chess.press(&mut self.chess_game, &board, &event),
                        Action::Home
                    )
                {
                    self.current = Screen::Home;
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
            Screen::AppStore => {
                let (Some(layout), PointerPhase::Up) = (&self.app_store_layout, event.phase) else {
                    return;
                };
                // The preview navigates; it does not install. Installing needs
                // a `StoreSource`, and a preview has no store and no catalog to
                // point one at — a button that pretended otherwise would be
                // exactly the thing this app exists not to do.
                match layout {
                    StoreLayout::List(list) => {
                        if let Some(index) = list.row_hit(event.at)
                            && let Some(entry) = self.app_store.rows().get(index)
                        {
                            let app = entry.app.clone();
                            self.app_store.open(&app);
                        }
                    }
                    StoreLayout::Detail(detail) => {
                        if detail.back.contains(event.at) {
                            self.app_store.back();
                        }
                    }
                }
            }
        }
    }

    /// Handles a key press in the preview.
    #[cfg(feature = "desktop")]
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

/// Golden frames: the digest each screen is frozen at.
///
/// `render_offscreen` is the one drawing path — `paperctl open` sends its
/// canvas to the panel, `paperctl screenshot` writes the same canvas to a PNG.
/// Up to now nothing asserted on either: the PNGs were looked at once by a
/// human and the digests were only ever compared with themselves, which is how
/// "was that digest a real shelf render or a fallback buffer?" stayed an open
/// question through WWW-23 and WWW-27.
///
/// Freezing the digests answers it before a takeover starts, and answers it
/// without a device, a mock or a window: if `paperctl open` prints the digest
/// in this table then the frame it is about to present is the shelf that was
/// reviewed, pixel for pixel, and not a blank buffer, a placeholder or a
/// half-built screen. What it still says nothing about is the glass (§17).
///
/// **When one of these fails after a deliberate UI change**, look at the PNG
/// — `paperctl screenshot`, which renders the same screens (Settings as one
/// file per page) — satisfy yourself that the new picture is the intended one,
/// and update the table in the same commit as the change. A digest re-blessed
/// on its own, in a commit of its own, is the assertion switched off.
#[cfg(test)]
mod golden {
    use paper_device::FrameDigest;
    use paper_sdk::SCREEN;

    use super::{Screen, Screens};

    /// Screen, its frozen digest, and its ink coverage in per mille.
    ///
    /// Home's value is also device evidence rather than only a desktop
    /// reading: `docs/device/www-23-first-light.md` records the tablet
    /// computing `0fb73b27…b0b08dd2` at ink 318/1000 for this same render, on
    /// aarch64, which is the one direct check that the digest a device run
    /// reports and the digest a Mac run reports are the same number.
    ///
    /// The other three are desktop readings only. Rendering uses `sin`, `cos`
    /// and `powf`, whose results are the platform's libm rather than something
    /// IEEE 754 pins down, so a device run printing a different digest for
    /// Chess, Settings or the App Store is a question to investigate — which
    /// libm, and by how many pixels — and not by itself proof the render
    /// drifted. Home matching across both platforms is the reason to expect
    /// they agree, not a guarantee that they do.
    const GOLDEN: [(Screen, &str, u32); 4] = [
        (
            Screen::Home,
            "0fb73b27198efb386d8d5dc906b190430e9ef465f7243b69e8b72cb6b0b08dd2",
            318,
        ),
        (
            Screen::Chess,
            "a56ceac7501a6013361a1c6ab268c8f85d2d1866c7402139ce9f609bbad572b9",
            567,
        ),
        (
            Screen::Settings,
            "d9387dd736fd2424b8732a76c7bdcd6ff47507ad0c70dcfa4bbf7c5660b4eaaa",
            132,
        ),
        (
            Screen::AppStore,
            "5aaf7f60493c7f8bda53fe2e694fa27d9993fd31db11f5616a652f819a191023",
            165,
        ),
    ];

    /// Every mismatch in one run, not just the first: a change that moves all
    /// four screens should print all four new digests, so the table can be
    /// checked against four PNGs and updated once.
    #[test]
    fn every_screen_renders_the_frame_it_is_frozen_at() {
        let mut screens = Screens::new().expect("the built-in screens build");
        let mut drift = Vec::new();
        for (screen, digest, ink) in GOLDEN {
            let canvas = screens
                .render_offscreen(screen)
                .expect("a screen renders offscreen");
            let actual = FrameDigest::of(&canvas).expect("digests");
            if actual.to_hex() != digest || actual.ink_per_mille() != ink {
                drift.push(format!(
                    "{}: expected sha256:{digest} at ink {ink}/1000, rendered {actual}",
                    screen.slug()
                ));
            }
        }
        assert!(
            drift.is_empty(),
            "the golden frames moved:\n{}",
            drift.join("\n")
        );
    }

    /// The two failures a digest table cannot catch by matching, because a
    /// wrong value frozen once matches forever: a screen that rendered nothing,
    /// and two screens that are secretly the same picture. Both are what a
    /// fallback or placeholder buffer looks like.
    #[test]
    fn each_frozen_frame_is_drawn_at_panel_size_and_distinct_from_the_others() {
        let mut screens = Screens::new().expect("the built-in screens build");
        let mut seen: Vec<(Screen, FrameDigest)> = Vec::new();
        for (screen, _, _) in GOLDEN {
            let canvas = screens
                .render_offscreen(screen)
                .expect("a screen renders offscreen");
            let digest = FrameDigest::of(&canvas).expect("digests");
            assert_eq!(
                digest.size(),
                SCREEN,
                "{} is not panel-sized",
                screen.slug()
            );
            assert!(
                digest.looks_drawn(),
                "{} rendered no ink at all",
                screen.slug()
            );
            if let Some((other, _)) = seen.iter().find(|(_, other)| other == &digest) {
                panic!(
                    "{} and {} render the identical frame",
                    screen.slug(),
                    other.slug()
                );
            }
            seen.push((screen, digest));
        }
    }
}
