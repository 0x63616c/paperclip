//! The Sudoku app's standalone entrypoint (§8, ADR-0022).
//!
//! Hands a real, unmodified [`SudokuApp`] to [`paper_sdk::run`] over this
//! process's own stdin/stdout — the same lifecycle `DevApp::Sudoku` gets
//! in-process in `paperctl`, now on the far side of an actual process
//! boundary. This is `bin/sudoku`, the file `apps/sudoku/paper.toml` names as
//! the entrypoint.

use std::io;
use std::process::ExitCode;

use paper_sdk::LocalSurfaces;
use paper_sudoku::SudokuApp;

fn main() -> ExitCode {
    let outcome = paper_sdk::run(
        SudokuApp::new(),
        io::stdin(),
        io::stdout(),
        LocalSurfaces::new(),
    );
    match outcome {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("paper-sudoku: {error}");
            ExitCode::FAILURE
        }
    }
}
