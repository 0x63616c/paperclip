//! The Home screen's standalone entrypoint (§8, ADR-0022).
//!
//! Hands a real, unmodified [`HomeApp`] to [`paper_sdk::run`] over this
//! process's own stdin/stdout — the same lifecycle `DevApp::Home` gets
//! in-process in `paperctl`, now on the far side of an actual process
//! boundary. This is `bin/home`, the file `apps/home/paper.toml` names as the
//! entrypoint.
//!
//! # Building the shelf
//!
//! `HomeApp`'s own doc is explicit that enumerating what to show is not its
//! job: "whoever launches Home builds the entry list and hands it over." This
//! binary is that launcher, so it is where that job actually lives now,
//! rather than in a shared library crate that every other caller of `HomeApp`
//! would inherit whether it wanted this shelf or not.
//!
//! What it builds the shelf from is deliberately narrower than
//! `tools/paperctl/src/session.rs`'s own `home_screen`: Settings and the App
//! Store, because those three ship together as one platform release (§13)
//! and their manifests move in lockstep with this binary's own. Chess is a
//! catalog app, installed and versioned independently of the platform
//! release — baking its manifest in here the way the dev/run sessions do
//! would show a version that can drift from what is actually installed.
//! Surfacing catalog apps on the shelf is a runtime survey
//! (`paper_packages::inventory`), which needs a host to drive it and does not
//! exist until WWW-4's supervisor; until then this shelf offers only what it
//! can state truthfully at its own compile time.

use std::io;
use std::process::ExitCode;

use paper_home::{HomeApp, HomeScreen, ShelfEntry, ShelfGlyph, SystemFact};
use paper_packages::{Manifest, ManifestError};
use paper_sdk::LocalSurfaces;

const HOME_MANIFEST: &str = include_str!("../paper.toml");
const SETTINGS_MANIFEST: &str = include_str!("../../settings/paper.toml");
const APP_STORE_MANIFEST: &str = include_str!("../../app-store/paper.toml");

fn main() -> ExitCode {
    let screen = match build_screen() {
        Ok(screen) => screen,
        Err(error) => {
            eprintln!("paper-home: {error}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = paper_sdk::run(
        HomeApp::new(screen),
        io::stdin(),
        io::stdout(),
        LocalSurfaces::new(),
    );
    match outcome {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("paper-home: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Parses the compiled-in manifests and lays out the shelf they describe.
fn build_screen() -> Result<HomeScreen, ManifestError> {
    let home = parse(HOME_MANIFEST)?;
    let settings = parse(SETTINGS_MANIFEST)?;
    let app_store = parse(APP_STORE_MANIFEST)?;
    Ok(HomeScreen {
        entries: vec![
            ShelfEntry::from_manifest(&settings, ShelfGlyph::Gear),
            ShelfEntry::from_manifest(&app_store, ShelfGlyph::Store),
            ShelfEntry::action("Return to stock", "REMARKABLE", ShelfGlyph::Stock),
        ],
        facts: vec![SystemFact::new("Home", format!("V{}", home.version()))],
        pressed: None,
        status: format!("V{}", home.version()),
    })
}

fn parse(text: &str) -> Result<Manifest, ManifestError> {
    let manifest = Manifest::parse(text)?;
    manifest.ensure_runnable()?;
    Ok(manifest)
}
