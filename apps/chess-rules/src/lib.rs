//! Chess legality, game state and the save format (§16).
//!
//! No UI, no SDK, no platform concept: this crate is testable with nothing
//! but `cargo test`, and stays that way on purpose (see the Stage 6 comment
//! on `apps/chess`). Legality, check, checkmate and stalemate come from the
//! `chess` crate; everything §16 calls a draw-classification question rather
//! than a legality one — insufficient material, threefold repetition, the
//! fifty-move rule — is ours. ADR-0017 records why and where the boundary
//! sits.

mod error;
mod game;
mod save;
mod types;

pub use error::{ClaimError, LoadError, MoveError, SaveError};
pub use game::{ClaimableDraw, Game, MoveRecord, Outcome};
pub use save::{load, save};
pub use types::{Color, File, Move, PieceKind, Rank, Square};
