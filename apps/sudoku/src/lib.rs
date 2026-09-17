//! The Sudoku app.
//!
//! Generation, validation, solving and the save format are
//! [`paper_sudoku_rules`]'s. This crate lays out the grid and the digit pad,
//! turns a tap into an entry or a footer action, and wires that to the
//! platform through [`App`](paper_sdk::App) as [`SudokuApp`].
//!
//! It is also the platform's worked example of per-cell damage (WWW-6,
//! WWW-39). Entering a digit changes one cell out of eighty-one, and this app
//! claims exactly that cell: see [`screen`] for how the claim is built and
//! why it lives in the press rather than in
//! [`App::damage`](paper_sdk::App::damage).

mod app;
mod layout;
mod screen;

pub use app::SudokuApp;
pub use layout::{GridLayout, PadKey, PadLayout};
pub use screen::{Notice, Press, SudokuLayout, SudokuScreen, render};
