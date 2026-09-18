//! The Settings app's standalone entrypoint (§8, ADR-0022).
//!
//! Hands a real, unmodified [`SettingsApp`] to [`paper_sdk::run`] over this
//! process's own stdin/stdout — the same lifecycle `DevApp::Settings` gets
//! in-process in `paperctl`, now on the far side of an actual process
//! boundary. This is `bin/settings`, the file `apps/settings/paper.toml`
//! names as the entrypoint.
//!
//! Unlike before WWW-71, this process opens no store of its own: every read
//! and write [`SettingsApp`] needs travels as a `SystemQuery` to whichever
//! process is running this connection (`tools/paperctl/src/admin.rs`'s
//! `AdminResponder` today) — see ADR-0028.

use std::io;
use std::process::ExitCode;

use paper_sdk::LocalSurfaces;
use paper_settings::SettingsApp;

fn main() -> ExitCode {
    let outcome = paper_sdk::run(
        SettingsApp::new(),
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
