//! The Chess app's standalone entrypoint (§8, ADR-0022).
//!
//! Hands a real, unmodified [`ChessApp`] to [`paper_sdk::run`] over this
//! process's own stdin/stdout — the same lifecycle `DevApp::Chess` gets
//! in-process in `paperctl`, now on the far side of an actual process
//! boundary. This is `bin/chess`, the file `apps/chess/paper.toml` names as
//! the entrypoint.

use std::io;
use std::process::ExitCode;

use paper_chess::ChessApp;
use paper_sdk::LocalSurfaces;

fn main() -> ExitCode {
    let outcome = paper_sdk::run(
        ChessApp::new(),
        io::stdin(),
        io::stdout(),
        LocalSurfaces::new(),
    );
    match outcome {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("paper-chess: {error}");
            ExitCode::FAILURE
        }
    }
}
