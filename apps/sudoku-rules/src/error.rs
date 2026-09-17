//! Typed failures. Each variant is something a caller can act on: a screen
//! that must explain a refused tap, or a save file that must not be trusted.

use std::io;
use std::path::PathBuf;

use crate::types::Cell;

/// Why a digit could not be entered, or a cell cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MoveError {
    /// The puzzle is already finished; nothing more can be entered.
    #[error("the puzzle is already solved")]
    Solved,
    /// That cell is one of the puzzle's givens and cannot be changed.
    #[error("that cell is one of the puzzle's givens")]
    Given,
    /// Another cell in the same row, column or block already holds that digit.
    ///
    /// Carries which one, because "that digit is already in R3C4" is the only
    /// form of this answer a player can do anything with.
    #[error("that digit is already in {with}", with = with.name())]
    Conflict {
        /// The cell that already holds the digit.
        with: Cell,
    },
}

/// Why a game could not be written to disk.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SaveError {
    /// Building the temporary file, writing to it, or renaming it over the
    /// target failed.
    #[error("cannot save the puzzle to {path}")]
    Io {
        /// The save's target path.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: io::Error,
    },
    /// The game could not be turned into TOML. Not expected — the saved form
    /// is four strings and a number — but a caller taking a `Result` should
    /// not have that stand on an `unwrap()`.
    #[error("cannot encode the puzzle to save it")]
    Serialize {
        /// The underlying encoding failure.
        #[source]
        source: toml::ser::Error,
    },
}

/// Why a saved game could not be loaded.
///
/// [`Self::Corrupt`] is distinct from [`Self::Syntax`] for the reason
/// `paper_chess_rules` keeps them apart: syntax is "not TOML", which is what
/// a torn write looks like, and corrupt is "valid TOML that is not a legal
/// Sudoku", which is what a hand-edited or bit-rotted file looks like.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    /// The file could not be read at all.
    #[error("cannot read {path}")]
    Io {
        /// The path that was attempted.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: io::Error,
    },
    /// The file is not valid TOML.
    #[error("{path} is not a valid saved puzzle")]
    Syntax {
        /// The path that was attempted.
        path: PathBuf,
        /// The underlying parse failure.
        #[source]
        source: toml::de::Error,
    },
    /// The file parsed, but what it holds is not a playable puzzle — a grid
    /// that is not 81 cells, a solution that is not solved, a given that
    /// contradicts it, or an entry that breaks a constraint.
    #[error("{path} is not a playable saved puzzle: {reason}")]
    Corrupt {
        /// The path that was attempted.
        path: PathBuf,
        /// What was wrong with it.
        reason: String,
    },
}
