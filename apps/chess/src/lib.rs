//! The Chess screen.
//!
//! Stage 1 draws a board and a starting position, and highlights whichever
//! square was last pressed. It does not know a legal move from an illegal one
//! and must not learn: the rules library is a WWW-6 decision (§16), taken
//! after review rather than by whoever gets to the file first. Nothing here
//! should grow into a legality engine in the meantime.

mod board;
mod pieces;
mod screen;

pub use board::{BoardLayout, File, Rank, Square};
pub use pieces::{Piece, Placement, Side, draw as draw_piece};
pub use screen::{ChessScreen, render, starting_placement};
