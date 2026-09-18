//! Tests that cross crate boundaries.
//!
//! Each crate tests its own pieces. This suite is for the claims that only
//! make sense end to end: that a press on a laptop window reaches the right
//! chess square, that both screens fill the real panel geometry, and that the
//! manifests the apps actually ship are the ones the code can read.
//!
//! None of it is device qualification. Every assertion here is about software
//! running on a Mac.

use std::fs;
use std::path::Path;
use std::time::Duration;

use paper_chess::{ChessLayout, ChessScreen, Square};
use paper_chess_rules::Game;
use paper_device::{DisplayProbe, FrameDigest, HoldPlan, MemoryPanel, PixelRect};
use paper_home::{HomeScreen, ShelfEntry, ShelfGlyph, SystemFact};
use paper_packages::{
    Capability, InstallPolicy, InstalledApp, MANIFEST_FILE_NAME, Manifest, ManifestError,
};
use paper_sdk::chrome::MIN_TOUCH_TARGET;
use paper_sdk::{
    Canvas, ContactId, DisplayMapping, Point, Pointer, PointerEvent, PointerPhase, SCREEN, Size,
};
use paper_settings::SettingsScreen;
use paper_sudoku::{PadKey, SudokuScreen};
use paper_sudoku_rules::{Cell, Difficulty, Digit, Game as SudokuGame};

/// The manifests the apps really ship, read from the repository.
const APP_MANIFESTS: [(&str, &str); 4] = [
    ("home", include_str!("../../../apps/home/paper.toml")),
    ("chess", include_str!("../../../apps/chess/paper.toml")),
    (
        "settings",
        include_str!("../../../apps/settings/paper.toml"),
    ),
    ("sudoku", include_str!("../../../apps/sudoku/paper.toml")),
];

fn screen_canvas() -> Canvas {
    Canvas::new(SCREEN).expect("a screen-sized canvas")
}

fn home_screen() -> HomeScreen {
    let chess = Manifest::parse(APP_MANIFESTS[1].1).expect("the chess manifest parses");
    HomeScreen {
        entries: vec![
            ShelfEntry::from_manifest(&chess, ShelfGlyph::Board),
            ShelfEntry::action("App Store", "NOT INSTALLED", ShelfGlyph::Store),
            ShelfEntry::action("Return to stock", "REMARKABLE", ShelfGlyph::Stock),
        ],
        facts: vec![SystemFact::new("Display", "1620 x 2160")],
        pressed: None,
        status: "V0.1.0".to_owned(),
    }
}

/// What `paperctl open` would send to the panel, and what `paperctl
/// screenshot` would write to a PNG, are one rendering — so the digest a
/// device run reports can be compared with a desktop one.
///
/// This is the §9 claim ("the same UI code on both backends") made checkable.
/// It says nothing about the glass; it says the bytes are not two different
/// pictures.
#[test]
fn the_frame_a_hold_presents_is_the_same_rendering_the_screenshot_writes() {
    let mut presented = screen_canvas();
    paper_home::render(&mut presented, &home_screen());
    let mut captured = screen_canvas();
    paper_home::render(&mut captured, &home_screen());

    let digest = FrameDigest::of(&presented).expect("digests");
    assert_eq!(digest, FrameDigest::of(&captured).expect("digests"));
    assert_eq!(digest.size(), SCREEN);
    assert_eq!(digest.to_hex().len(), 64);
    // The refusal `paperctl open` performs before it stops Xochitl: a shelf
    // has ink, and an empty buffer is what a silent rendering failure looks
    // like from the device side.
    assert!(
        digest.looks_drawn(),
        "the shelf rendered as bare background"
    );
    assert!(digest.ink_per_mille() > 20, "{digest}");
}

/// A hold presents the shelf once, over the whole panel, and clears before it
/// gives the display back.
///
/// Driven against a `MemoryPanel`, which proves the sequence and nothing about
/// e-ink. The clear is the assertion that matters: WWW-20 photographed what
/// skipping it leaves on a user's stock screen.
#[test]
fn a_hold_presents_the_shelf_once_and_always_clears_the_panel() {
    let mut canvas = screen_canvas();
    paper_home::render(&mut canvas, &home_screen());

    let mut panel = MemoryPanel::new(SCREEN);
    let plan = HoldPlan {
        hold: Duration::from_secs(60),
        sample_every: Duration::from_secs(30),
        ..HoldPlan::first_light()
    };
    let mut slept = Duration::ZERO;
    let work = paper_device::present_and_hold(
        &mut panel,
        &canvas,
        &plan,
        &DisplayProbe::rooted(Path::new("/nonexistent")),
        false,
        |duration| slept += duration,
    )
    .expect("presents");

    assert_eq!(panel.swaps().len(), 1, "a first paint is one swap");
    assert_eq!(panel.swaps()[0].rect, PixelRect::PANEL);
    assert_eq!(
        panel.clears(),
        1,
        "the panel was not cleared before release"
    );
    assert_eq!(slept, plan.hold, "the hold was cut short");
    assert_eq!(
        work.claim(),
        "not presented: the frame went to a memory panel, not to the glass",
        "a memory run must not claim it reached the panel"
    );
}

#[test]
fn both_screens_render_at_the_target_panel_geometry() {
    let mut chess = screen_canvas();
    paper_chess::render(&mut chess, &ChessScreen::new(), &Game::new());
    let mut home = screen_canvas();
    paper_home::render(&mut home, &home_screen());

    for (name, canvas) in [("chess", &chess), ("home", &home)] {
        assert_eq!(canvas.size(), SCREEN, "{name} is not the panel size");
        assert_eq!(canvas.size(), Size::new(1620, 2160));

        let coverage = canvas.ink_coverage();
        assert!(coverage > 0.02, "{name} drew almost nothing ({coverage})");
        assert!(coverage < 0.90, "{name} went nearly solid ({coverage})");

        let png = canvas.to_png().expect("encodes as PNG");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "{name} is not a PNG");
    }
}

#[test]
fn the_settings_screen_renders_at_the_target_panel_geometry() {
    let screen = SettingsScreen::preview();
    let mut canvas = screen_canvas();
    paper_settings::render(&mut canvas, &screen);

    assert_eq!(canvas.size(), SCREEN);
    let coverage = canvas.ink_coverage();
    assert!(coverage > 0.02, "settings drew almost nothing ({coverage})");
    assert!(coverage < 0.90, "settings went nearly solid ({coverage})");
}

#[test]
fn the_two_screens_are_not_the_same_picture() {
    let mut chess = screen_canvas();
    paper_chess::render(&mut chess, &ChessScreen::new(), &Game::new());
    let mut home = screen_canvas();
    paper_home::render(&mut home, &home_screen());
    assert_ne!(chess.to_png().unwrap(), home.to_png().unwrap());
}

/// A press at a physical window pixel, all the way to a chess square.
fn press_square(window: Size, physical: Point, layout: ChessLayout) -> Option<Square> {
    let mapping = DisplayMapping::fit(SCREEN, window);
    let at = mapping.to_canvas(physical)?;
    let event = PointerEvent::new(at, PointerPhase::Up, Pointer::Mouse, ContactId::FIRST);
    layout.board.square_at(event.at)
}

#[test]
fn a_press_in_a_letterboxed_window_reaches_the_square_under_the_cursor() {
    let mut canvas = screen_canvas();
    let layout = paper_chess::render(&mut canvas, &ChessScreen::new(), &Game::new());

    // A wide laptop window: the canvas is pillarboxed, so the mapping has to
    // subtract the bars before anything is in canvas space.
    let window = Size::new(1600, 1000);
    let mapping = DisplayMapping::fit(SCREEN, window);
    assert!(mapping.viewport().x > 0.0, "this window should pillarbox");

    for square in Square::all() {
        let physical = mapping.to_physical(layout.board.square_rect(square).center());
        assert_eq!(
            press_square(window, physical, layout),
            Some(square),
            "{} did not come back",
            square.name()
        );
    }
}

#[test]
fn a_press_on_a_letterbox_bar_reaches_nothing() {
    let mut canvas = screen_canvas();
    let layout = paper_chess::render(&mut canvas, &ChessScreen::new(), &Game::new());
    let window = Size::new(1600, 1000);
    assert_eq!(press_square(window, Point::new(4.0, 500.0), layout), None);
    assert_eq!(
        press_square(window, Point::new(1596.0, 500.0), layout),
        None
    );
}

#[test]
fn the_same_press_lands_on_the_same_square_at_one_x_and_two_x() {
    let mut canvas = screen_canvas();
    let layout = paper_chess::render(&mut canvas, &ChessScreen::new(), &Game::new());

    let logical = (760.0, 1010.0);
    let one_x = DisplayMapping::fit_logical(SCREEN, logical, 1.0);
    let two_x = DisplayMapping::fit_logical(SCREEN, logical, 2.0);

    for square in Square::all() {
        let canvas_point = layout.board.square_rect(square).center();
        let at_one_x = one_x
            .to_canvas(one_x.to_physical(canvas_point))
            .and_then(|at| layout.board.square_at(at));
        let at_two_x = two_x
            .to_canvas(two_x.to_physical(canvas_point))
            .and_then(|at| layout.board.square_at(at));
        assert_eq!(at_one_x, Some(square));
        assert_eq!(
            at_two_x,
            at_one_x,
            "{} moved on a Retina window",
            square.name()
        );
    }
}

#[test]
fn a_press_on_the_home_shelf_reaches_the_tile_under_the_cursor() {
    let mut canvas = screen_canvas();
    let layout = paper_home::render(&mut canvas, &home_screen());

    let window = Size::new(1200, 900);
    let mapping = DisplayMapping::fit(SCREEN, window);

    for (index, tile) in layout.tiles().iter().enumerate() {
        let physical = mapping.to_physical(tile.center());
        let at = mapping.to_canvas(physical).expect("inside the viewport");
        assert_eq!(layout.hit_test(at), Some(index));
    }
}

#[test]
fn everything_tappable_on_either_screen_is_big_enough_to_tap() {
    let mut chess = screen_canvas();
    let board = paper_chess::render(&mut chess, &ChessScreen::new(), &Game::new());
    assert!(
        board.board.square_size() >= MIN_TOUCH_TARGET,
        "chess squares are {} px",
        board.board.square_size()
    );

    let mut home = screen_canvas();
    let shelf = paper_home::render(&mut home, &home_screen());
    for tile in shelf.tiles() {
        assert!(
            tile.shortest_side() >= MIN_TOUCH_TARGET,
            "a shelf tile is {tile:?}"
        );
    }
}

#[test]
fn the_manifests_the_apps_ship_are_valid_and_runnable() {
    for (app, text) in APP_MANIFESTS {
        let manifest =
            Manifest::parse(text).unwrap_or_else(|error| panic!("{app}/paper.toml: {error}"));
        manifest
            .ensure_runnable()
            .unwrap_or_else(|error| panic!("{app}/paper.toml: {error}"));
        assert_eq!(manifest.protocol(), paper_protocol::CURRENT);
        assert!(!manifest.name().as_str().is_empty());
        assert!(manifest.id().as_str().starts_with("dev.calum."));
    }
}

#[test]
fn every_shipped_app_id_is_distinct() {
    let ids: Vec<String> = APP_MANIFESTS
        .iter()
        .map(|(_, text)| {
            Manifest::parse(text)
                .expect("valid manifest")
                .id()
                .as_str()
                .to_owned()
        })
        .collect();
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "two apps share an id: {ids:?}");
}

#[test]
fn no_shipped_app_can_grant_itself_a_capability() {
    for (app, text) in APP_MANIFESTS {
        let manifest = Manifest::parse(text).expect("valid manifest");

        // The manifest as shipped holds nothing under a deny-all policy...
        let installed = InstalledApp::install(manifest.clone(), &InstallPolicy::deny_all());
        assert!(
            installed.capabilities().is_empty(),
            "{app} arrived holding capabilities"
        );

        // ...and adding the key it would need to ask for one is a parse error,
        // not a request the host gets to weigh up.
        let forged = text.replace(
            "assets = []",
            "assets = []\ncapabilities = [\"network\", \"storage\"]",
        );
        assert!(
            matches!(
                Manifest::parse(&forged),
                Err(ManifestError::SelfGrantedCapabilities { .. })
            ),
            "{app} was able to declare capabilities"
        );

        // Only the host's policy moves the answer.
        let mut policy = InstallPolicy::deny_all();
        policy.allow(manifest.id(), Capability::Storage);
        let granted = InstalledApp::install(manifest, &policy);
        assert!(granted.capabilities().holds(Capability::Storage));
        assert!(!granted.capabilities().holds(Capability::Network));
    }
}

#[test]
fn a_staged_package_validates_against_its_payload() {
    let package = tempfile::tempdir().expect("tempdir");
    let root = package.path();

    let manifest_text = r#"
[app]
id = "dev.calum.chess"
name = "Chess"
version = "0.1.0"
protocol = "1.0"
entrypoint = "bin/chess"
assets = ["assets/opening-book.toml"]
"#;
    fs::write(root.join(MANIFEST_FILE_NAME), manifest_text).expect("manifest");
    fs::create_dir_all(root.join("bin")).expect("bin");
    fs::create_dir_all(root.join("assets")).expect("assets");
    fs::write(root.join("bin/chess"), b"#!/bin/sh\n").expect("entrypoint");

    let manifest = Manifest::read_package(root).expect("manifest reads");
    assert!(
        manifest.validate_payload(root).is_err(),
        "a package missing a declared asset must not validate"
    );

    fs::write(root.join("assets/opening-book.toml"), b"").expect("asset");
    manifest
        .validate_payload(root)
        .expect("the package is complete now");
}

/// The cost of a digit entry, in panel pixels — the whole reason §4 asks apps
/// to report damage, and the claim ADR-0021 makes on Sudoku's behalf.
///
/// The app crate already checks that entering a digit claims one cell and
/// changes no pixel outside it. This is the layer past that: the rectangle
/// `paperctl run` hands the waveform engine, computed by the same
/// [`PixelRect::covering`] the presenting loop calls. An app that claims
/// correctly and a presenter that widens the claim to the panel would look
/// identical in the app's own tests and cost a full-screen waveform per
/// keystroke on the glass.
///
/// Software only. It says what will be swapped, not what the panel does with
/// it — no digit has reached the glass yet (WWW-39).
#[test]
fn a_sudoku_digit_entry_swaps_one_cell_and_not_the_panel() {
    let tap = |at: Point| PointerEvent::new(at, PointerPhase::Up, Pointer::Touch, ContactId::FIRST);

    let mut game = SudokuGame::start(Difficulty::Easy, 4_242);
    let mut screen = SudokuScreen::new(game.difficulty());
    let mut canvas = screen_canvas();
    let layout = paper_sudoku::render(&mut canvas, &screen, &game);

    let (cell, digit) = Cell::all()
        .filter(|cell| game.digit_at(*cell).is_none())
        .find_map(|cell| {
            Digit::ALL
                .into_iter()
                .find(|digit| game.board().accepts(cell, *digit))
                .map(|digit| (cell, digit))
        })
        .expect("some empty cell takes some digit");

    // Select the cell, then re-render: the layout a real session presses
    // against is the one the previous frame produced.
    screen.press(
        &mut game,
        &layout,
        &tap(layout.grid.cell_rect(cell).center()),
    );
    let layout = paper_sudoku::render(&mut canvas, &screen, &game);
    let key = layout
        .pad
        .key_rect(PadKey::Digit(digit))
        .expect("the pad has that digit");
    let press = screen.press(&mut game, &layout, &tap(key.center()));
    assert_eq!(
        game.digit_at(cell),
        Some(digit),
        "the tap claimed a cell it never wrote"
    );

    let swap = PixelRect::covering(&press.damage, SCREEN);
    assert_eq!(
        swap,
        PixelRect::enclosing(layout.grid.cell_rect(cell), SCREEN),
        "the swap is not the cell that changed"
    );
    assert!(!swap.is_empty(), "a digit entry asked for no swap at all");
    assert!(swap.fits_in(SCREEN));

    // The number that matters: a keystroke must not cost a panel.
    let panel = u64::from(SCREEN.width) * u64::from(SCREEN.height);
    let swapped = u64::from(swap.width) * u64::from(swap.height);
    assert!(
        swapped * 100 < panel,
        "a digit entry swaps {swapped} of {panel} panel pixels; per-cell damage buys nothing"
    );

    // And the contrast, so the assertion above cannot pass by accident on a
    // presenter that ignores damage: a first draw really is the whole panel.
    assert_eq!(
        PixelRect::covering(&paper_sdk::Damage::Full, SCREEN),
        PixelRect::PANEL
    );
}
