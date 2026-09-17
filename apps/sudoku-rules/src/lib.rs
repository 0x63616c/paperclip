//! Sudoku: the grid, generation, validation, solving and the save format.
//!
//! No UI, no SDK, no platform concept — the same boundary
//! `paper_chess_rules` keeps (ADR-0017), for the same reason: everything
//! that can be decided without a screen is decided here, where `cargo test`
//! can see it, and `apps/sudoku` is left with presentation and one tap-to-
//! intent translation.
//!
//! Unlike chess there is no established crate to adopt. Sudoku's whole rule
//! set is "no digit twice in a row, a column or a 3x3 block", which is one
//! function; what actually needs care is generation — a puzzle whose solution
//! is not unique is a broken puzzle, and the only way to know is to count the
//! solutions. [`solutions`] is therefore load-bearing rather than a
//! convenience, and [`Puzzle::generate`] never returns a puzzle it has not
//! counted.
//!
//! Generation is deterministic in its seed. That is not a test convenience:
//! it is what lets the golden-frame table in `paperctl` freeze a Sudoku
//! render at all, and what lets a save file restore a puzzle by storing 81
//! characters rather than a solver's internal state.

mod error;
mod game;
mod generate;
mod rng;
mod save;
mod solve;
mod types;

pub use error::{LoadError, MoveError, SaveError};
pub use game::Game;
pub use generate::{Difficulty, Puzzle};
pub use save::{load, save};
pub use solve::{is_unique, solutions, solve};
pub use types::{Cell, Digit, Grid};
