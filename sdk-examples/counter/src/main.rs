//! Counter's standalone entrypoint (§8, ADR-0022).
//!
//! Hands a real, unmodified [`CounterApp`] to [`paper_sdk::run`] over this
//! process's own stdin/stdout — the shape every catalog app's `[[bin]]`
//! takes, and the file `paper.toml` names as the entrypoint.

use std::io;
use std::process::ExitCode;

use paper_counter::CounterApp;
use paper_sdk::LocalSurfaces;

fn main() -> ExitCode {
    let outcome = paper_sdk::run(
        CounterApp::new(),
        io::stdin(),
        io::stdout(),
        LocalSurfaces::new(),
    );
    match outcome {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("paper-counter: {error}");
            ExitCode::FAILURE
        }
    }
}
