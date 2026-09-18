//! The render test card's standalone entrypoint (§8, ADR-0022).
//!
//! Hands a real, unmodified [`RenderTestCardApp`] to [`paper_sdk::run`] over
//! this process's own stdin/stdout — the same lifecycle `DevApp::RenderTestCard`
//! gets in-process in `paperctl`, now on the far side of an actual process
//! boundary. This is `bin/render-test-card`, the file
//! `apps/render-test-card/paper.toml` names as the entrypoint.
//!
//! Its pixels go to the compositor (WWW-81): [`paper_compositor::
//! CompositorSurfaces`] replaces [`paper_sdk::LocalSurfaces`], so the test
//! card is an ordinary compositor client with no special access to the
//! panel — the thing it exists to look at.

use std::io;
use std::process::ExitCode;

use paper_compositor::{ClientRole, CompositorSurfaces, socket_path};
use paper_render_test_card::RenderTestCardApp;

fn main() -> ExitCode {
    let outcome = paper_sdk::run(
        RenderTestCardApp::new(),
        io::stdin(),
        io::stdout(),
        CompositorSurfaces::new(socket_path(), ClientRole::App, "Render Test Card"),
    );
    match outcome {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("paper-render-test-card: {error}");
            ExitCode::FAILURE
        }
    }
}
