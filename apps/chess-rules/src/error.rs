//! Typed failures. Every variant names what a caller can do about it, since
//! the only two callers this crate has are a UI that must react (ask for a
//! promotion piece, refuse a tap) and a save file that must not be trusted.

use std::io;
use std::path::PathBuf;

/// Why a proposed [`crate::Move`](crate::Move) was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MoveError {
    /// The game already has an [`crate::Outcome`](crate::Outcome); no move is legal in a finished game.
    #[error("the game is over")]
    GameOver,
    /// There is no piece on the `from` square.
    #[error("there is no piece on that square")]
    EmptySquare,
    /// The piece on `from` belongs to the side who is not moving.
    #[error("that piece belongs to the side not to move")]
    WrongColor,
    /// No legal move goes from `from` to `to`, with any promotion choice.
    #[error("that is not a legal move")]
    Illegal,
    /// A pawn reaching the back rank was moved without saying what it becomes.
    #[error("a promotion piece is required")]
    PromotionRequired,
    /// A promotion piece was given for a move that is not a promotion.
    #[error("that move does not promote a pawn")]
    PromotionNotAllowed,
}

/// Why a draw could not be claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ClaimError {
    /// The game already has an outcome.
    #[error("the game is over")]
    GameOver,
    /// Neither threefold repetition nor the fifty-move rule is available.
    #[error("no claimable draw is available")]
    NothingToClaim,
}

/// Why a game could not be written to disk.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SaveError {
    /// Building the temporary file, writing to it, or renaming it over the
    /// target failed.
    #[error("cannot save the game to {path}")]
    Io {
        /// The save's target path.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: io::Error,
    },
    /// The game could not be turned into TOML. Not expected to happen —
    /// nothing in [`crate::Game`](crate::Game) is unserialisable — but a
    /// caller taking a `Result` should not have that stand on an `unwrap()`.
    #[error("cannot encode the game to save it")]
    Serialize {
        /// The underlying encoding failure.
        #[source]
        source: toml::ser::Error,
    },
}

/// Why a saved game could not be loaded.
///
/// [`Self::Corrupt`] is distinct from [`Self::Syntax`] on purpose: syntax is
/// "not TOML", corrupt is "valid TOML that does not replay into a legal
/// game" — a torn write is far more likely to produce the first than the
/// second, and a hand-edited or bit-rotted file is the one that produces the
/// second.
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
    #[error("{path} is not a valid saved game")]
    Syntax {
        /// The path that was attempted.
        path: PathBuf,
        /// The underlying parse failure.
        #[source]
        source: toml::de::Error,
    },
    /// The file parsed, but replaying its events does not produce a legal
    /// game — for example a move recorded out of turn, or a claim recorded
    /// when no draw was claimable at that point.
    #[error("{path} does not replay into a legal game: {reason}")]
    Corrupt {
        /// The path that was attempted.
        path: PathBuf,
        /// What went wrong during replay.
        reason: String,
    },
}
