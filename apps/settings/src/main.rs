//! The Settings app's standalone entrypoint (§8, ADR-0022).
//!
//! Hands a real, unmodified [`SettingsApp`] to [`paper_sdk::run`] over this
//! process's own stdin/stdout — the same lifecycle `DevApp::Settings` gets
//! in-process in `paperctl`, now on the far side of an actual process
//! boundary. This is `bin/settings`, the file `apps/settings/paper.toml`
//! names as the entrypoint.
//!
//! [`LiveHost`] over [`Layout::from_environment`] is the same real store
//! `paperctl run`'s own "settings" session opens
//! (`tools/paperctl/src/session.rs`), not a fixture: unlike Home and the App
//! Store, Settings needs no manifest of its own or anyone else's to start —
//! every read and write goes through the `Layout` it is handed.

use std::io;
use std::process::ExitCode;

use paper_packages::store::Layout;
use paper_sdk::LocalSurfaces;
use paper_settings::{LiveHost, SettingsApp};

fn main() -> ExitCode {
    let outcome = paper_sdk::run(
        SettingsApp::new(LiveHost::new(Layout::from_environment())),
        io::stdin(),
        io::stdout(),
        LocalSurfaces::new(),
    );
    match outcome {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("paper-settings: {error}");
            ExitCode::FAILURE
        }
    }
}
