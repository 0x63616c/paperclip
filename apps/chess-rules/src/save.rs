//! The save format: an ordered event log, replayed to reconstruct state.
//!
//! §16 asks for the position *and* the history needed for repetition and the
//! fifty-move rule, and for that to survive corrupted and interrupted
//! writes. Storing our own event log rather than a snapshot of `chess::Board`
//! gives the first for free — replay derives everything, including the
//! repetition counts — and [`save`] gives the second: it builds the whole
//! file in a temporary sibling and renames it into place, so a write that
//! dies partway through leaves either the previous save or the new one,
//! never something in between.

use std::fs;
use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{LoadError, SaveError};
use crate::game::{Game, GameEvent};

#[derive(Debug, Serialize, Deserialize)]
struct SavedGame {
    events: Vec<GameEvent>,
}

/// Writes `game` to `path`, replacing whatever was there atomically.
///
/// # Errors
///
/// If the game cannot be encoded, or the temporary file cannot be created,
/// written, or renamed into place. In every one of those cases `path` still
/// holds whatever it held before this call.
pub fn save(game: &Game, path: &Path) -> Result<(), SaveError> {
    let saved = SavedGame {
        events: game.events().to_vec(),
    };
    let text = toml::to_string_pretty(&saved).map_err(|source| SaveError::Serialize { source })?;

    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let mut temp = tempfile::Builder::new()
        .prefix(".paper-chess-save-")
        .tempfile_in(directory.unwrap_or_else(|| Path::new(".")))
        .map_err(|source| SaveError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    temp.write_all(text.as_bytes())
        .and_then(|()| temp.flush())
        .map_err(|source| SaveError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    temp.persist(path).map_err(|error| SaveError::Io {
        path: path.to_path_buf(),
        source: error.error,
    })?;
    Ok(())
}

/// Reads and replays the game saved at `path`.
///
/// # Errors
///
/// [`LoadError::Io`] if the file cannot be read, [`LoadError::Syntax`] if it
/// is not valid TOML, or [`LoadError::Corrupt`] if it parses but its events
/// do not replay into a legal game — see that variant's documentation for
/// why those are kept apart.
pub fn load(path: &Path) -> Result<Game, LoadError> {
    let text = fs::read_to_string(path).map_err(|source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let saved: SavedGame = toml::from_str(&text).map_err(|source| LoadError::Syntax {
        path: path.to_path_buf(),
        source,
    })?;
    Game::from_events(&saved.events).map_err(|reason| LoadError::Corrupt {
        path: path.to_path_buf(),
        reason,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{SavedGame, load, save};
    use crate::error::LoadError;
    use crate::game::Game;
    use crate::types::{Move, PieceKind, Square};

    fn sq(name: &str) -> Square {
        Square::parse(name).unwrap_or_else(|| panic!("{name} is a valid square"))
    }

    #[test]
    fn a_saved_game_loads_back_to_the_same_position() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("game.toml");

        let mut game = Game::new();
        game.apply(Move::new(sq("e2"), sq("e4"))).unwrap();
        game.apply(Move::new(sq("e7"), sq("e5"))).unwrap();
        game.apply(Move::new(sq("g1"), sq("f3"))).unwrap();
        save(&game, &path).expect("save succeeds");

        let loaded = load(&path).expect("load succeeds");
        assert_eq!(loaded.side_to_move(), game.side_to_move());
        assert_eq!(loaded.outcome(), game.outcome());
        assert_eq!(
            loaded.piece_at(sq("f3")),
            Some((crate::types::Color::White, PieceKind::Knight))
        );
    }

    #[test]
    fn saving_twice_leaves_only_the_latest_game_on_disk() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("game.toml");

        let mut game = Game::new();
        save(&game, &path).expect("first save succeeds");
        game.apply(Move::new(sq("d2"), sq("d4"))).unwrap();
        save(&game, &path).expect("second save succeeds");

        let loaded = load(&path).expect("load succeeds");
        assert_eq!(
            loaded.piece_at(sq("d4")).map(|(_, piece)| piece),
            Some(PieceKind::Pawn)
        );
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no leftover temp file"
        );
    }

    #[test]
    fn truncated_bytes_are_reported_as_corrupt_input_not_a_panic() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("game.toml");
        fs::write(&path, b"events = [ { type = \"move\", from = \"e2\"").unwrap();

        match load(&path) {
            Err(LoadError::Syntax { .. }) => {}
            other => panic!("expected a syntax error for truncated TOML, got {other:?}"),
        }
    }

    #[test]
    fn a_history_that_does_not_replay_legally_is_reported_as_corrupt() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("game.toml");

        let mut one_move = Game::new();
        one_move.apply(Move::new(sq("e2"), sq("e4"))).unwrap();
        let block = toml::to_string_pretty(&SavedGame {
            events: one_move.events().to_vec(),
        })
        .expect("a one-move history encodes");

        // The same recorded move twice: well-formed TOML, but illegal chess —
        // e2 is empty the second time, and it is not White's turn again.
        fs::write(&path, format!("{block}{block}")).unwrap();

        match load(&path) {
            Err(LoadError::Corrupt { .. }) => {}
            other => panic!("expected a corrupt-replay error, got {other:?}"),
        }
    }

    #[test]
    fn a_write_that_never_gets_renamed_leaves_the_previous_save_loadable() {
        // Simulates a crash between writing the temporary file and the
        // rename that `save` relies on for atomicity: the old file is
        // untouched, and a stray temp file is not mistaken for the save.
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("game.toml");

        let original = Game::new();
        save(&original, &path).expect("first save succeeds");
        let before = fs::read_to_string(&path).unwrap();

        fs::write(
            dir.path().join(".paper-chess-save-stray"),
            b"garbage, never renamed",
        )
        .unwrap();

        let loaded = load(&path).expect("the previous save is still there and still loads");
        assert_eq!(loaded.side_to_move(), original.side_to_move());
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }
}
