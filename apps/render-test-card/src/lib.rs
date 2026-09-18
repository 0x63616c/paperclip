//! The render test card (WWW-47): a Paperclip app whose whole job is to show
//! what this panel actually does, so rendering questions are settled by
//! looking rather than by arguing.
//!
//! Every display question in this project so far has been answered by
//! reconstruction after the fact. This app is the instrument instead: seven
//! sections, each individually reachable through the tab strip, each drawing
//! samples labelled with the values they show so a photograph of any one of
//! them stands on its own. See `nav::Section` for the list and the ticket for
//! why each one is here.
//!
//! It is an ordinary app, wired the same way `paper_sudoku::SudokuApp` is:
//! `paper.toml` names `bin/render-test-card` as its entrypoint (ADR-0022), and
//! [`RenderTestCardApp`] implements [`App`](paper_sdk::App) like any other. It
//! has no capability grants and nothing to save — every section's state is
//! scratch, reset by a fresh launch, which is the right lifetime for numbers
//! that exist to be looked at once and then re-created for the next look.

mod app;
mod nav;
mod screen;
mod sections;

pub use app::RenderTestCardApp;
