//! The Chess app.
//!
//! Legality, check, checkmate, stalemate and the draw rules are
//! `paper_chess_rules::Game`'s (§16, ADR-0017). This crate draws the board,
//! turns a tap into a move or a footer action, and wires that to the
//! platform through [`App`](paper_sdk::App) as [`ChessApp`].

mod app;
mod board;
mod pieces;
mod screen;

pub use app::ChessApp;
pub use board::{BoardLayout, File, Rank, Square};
pub use pieces::{Piece, Placement, Side, draw as draw_piece};
pub use screen::{ChessLayout, ChessScreen, PromotionLayout, render};
