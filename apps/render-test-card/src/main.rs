//! The render test card's standalone entrypoint (§8, ADR-0022).
//!
//! Hands a real, unmodified [`RenderTestCardApp`] to [`paper_sdk::run`] over
//! this process's own stdin/stdout — the same lifecycle `DevApp::RenderTestCard`
//! gets in-process in `paperctl`, now on the far side of an actual process
//! boundary. This is `bin/render-test-card`, the file
//! `apps/render-test-card/paper.toml` names as the entrypoint.

use std::io;
use std::process::ExitCode;

use paper_render_test_card::RenderTestCardApp;
use paper_sdk::LocalSurfaces;

fn main() -> ExitCode {
    let outcome = paper_sdk::run(
        RenderTestCardApp::new(),
        io::stdin(),
        io::stdout(),
        LocalSurfaces::new(),
    );
    match outcome {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("paper-render-test-card: {error}");
            ExitCode::FAILURE
        }
    }
}
