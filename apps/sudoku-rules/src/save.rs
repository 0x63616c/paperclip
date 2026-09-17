//! The save format: the puzzle, the board, and where the generator stream got
//! to — four strings and a number.
//!
//! Unlike Chess, replaying an event log would buy nothing: a Sudoku's state is
//! the grid, there is no repetition rule and no history-dependent outcome, and
//! the givens cannot be derived from anything smaller than themselves.
//! Storing three 81-character lines is the whole state, is readable by eye,
//! and is validated on the way back in.
//!
//! [`save`] builds the file in a temporary sibling and renames it into place,
//! so a write interrupted part-way leaves either the previous save or the new
//! one and never something in between — the same durability `paper_chess_rules`
//! relies on, for the same reason.

use std::fs;
use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{LoadError, SaveError};
use crate::game::Game;
use crate::generate::{Difficulty, Puzzle};
use crate::types::{Cell, Grid};

#[derive(Debug, Serialize, Deserialize)]
struct SavedPuzzle {
    /// [`Difficulty::slug`].
    difficulty: String,
    /// Where the generator stream had got to, so the next puzzle is the next
    /// one and not a repeat.
    stream: u64,
    /// The puzzle's clues, 81 characters.
    givens: String,
    /// Its solution, 81 characters.
    solution: String,
    /// The board as it stands, givens included, 81 characters.
    board: String,
}

/// Writes `game` to `path`, replacing whatever was there atomically.
///
/// # Errors
///
/// If the game cannot be encoded, or the temporary file cannot be created,
/// written or renamed into place. In every one of those cases `path` still
/// holds whatever it held before the call.
pub fn save(game: &Game, path: &Path) -> Result<(), SaveError> {
    let saved = SavedPuzzle {
        difficulty: game.difficulty().slug().to_owned(),
        stream: game.rng_state(),
        givens: game.puzzle().givens().to_line(),
        solution: game.puzzle().solution().to_line(),
        board: game.board().to_line(),
    };
    let text = toml::to_string_pretty(&saved).map_err(|source| SaveError::Serialize { source })?;

    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let mut temp = tempfile::Builder::new()
        .prefix(".paper-sudoku-save-")
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

/// Reads the game saved at `path`.
///
/// # Errors
///
/// [`LoadError::Io`] if the file cannot be read, [`LoadError::Syntax`] if it
/// is not valid TOML, or [`LoadError::Corrupt`] if it parses but is not a
/// playable puzzle — see [`LoadError::Corrupt`] for why those are kept apart.
pub fn load(path: &Path) -> Result<Game, LoadError> {
    let text = fs::read_to_string(path).map_err(|source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let saved: SavedPuzzle = toml::from_str(&text).map_err(|source| LoadError::Syntax {
        path: path.to_path_buf(),
        source,
    })?;
    rebuild(&saved).map_err(|reason| LoadError::Corrupt {
        path: path.to_path_buf(),
        reason,
    })
}

/// Turns a parsed save back into a game, or says what is wrong with it.
fn rebuild(saved: &SavedPuzzle) -> Result<Game, String> {
    let difficulty = Difficulty::parse(&saved.difficulty)
        .ok_or_else(|| format!("`{}` is not a difficulty", saved.difficulty))?;
    let givens = grid(&saved.givens, "givens")?;
    let solution = grid(&saved.solution, "solution")?;
    let board = grid(&saved.board, "board")?;

    let puzzle = Puzzle::from_parts(givens, solution, difficulty)?;

    // The board must be the givens plus legal entries: a save that dropped a
    // clue, or invented one, is not this puzzle any more.
    for cell in Cell::all() {
        match (givens.get(cell), board.get(cell)) {
            (Some(given), held) if held != Some(given) => {
                return Err(format!(
                    "the board does not hold the given at {}",
                    cell.name()
                ));
            }
            _ => {}
        }
    }
    if !board.is_valid() {
        return Err("the board breaks a row, column or block constraint".to_owned());
    }

    Ok(Game::restore(puzzle, board, saved.stream))
}

fn grid(line: &str, what: &str) -> Result<Grid, String> {
    Grid::parse_line(line).ok_or_else(|| format!("the {what} are not 81 valid cells"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{SavedPuzzle, load, save};
    use crate::error::LoadError;
    use crate::game::Game;
    use crate::generate::Difficulty;
    use crate::types::{Cell, Digit, Grid};

    /// A game with one digit entered, and the cell it went in.
    fn played() -> (Game, Cell) {
        let mut game = Game::start(Difficulty::Medium, 555);
        let cell = Cell::all()
            .find(|cell| game.digit_at(*cell).is_none())
            .expect("a puzzle has empty cells");
        let digit = Digit::ALL
            .into_iter()
            .find(|digit| game.board().accepts(cell, *digit))
            .expect("something is placeable");
        game.set(cell, digit).expect("a legal entry");
        (game, cell)
    }

    fn saved_parts(game: &Game) -> SavedPuzzle {
        SavedPuzzle {
            difficulty: game.difficulty().slug().to_owned(),
            stream: 0,
            givens: game.puzzle().givens().to_line(),
            solution: game.puzzle().solution().to_line(),
            board: game.board().to_line(),
        }
    }

    fn write_parts(path: &std::path::Path, parts: &SavedPuzzle) {
        fs::write(path, toml::to_string_pretty(parts).expect("encodes")).expect("writes");
    }

    #[test]
    fn a_saved_game_loads_back_to_the_same_board() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("puzzle.toml");
        let (game, cell) = played();

        save(&game, &path).expect("save succeeds");
        let loaded = load(&path).expect("load succeeds");

        assert_eq!(loaded.board(), game.board());
        assert_eq!(loaded.puzzle(), game.puzzle());
        assert_eq!(loaded.difficulty(), game.difficulty());
        assert_eq!(loaded.digit_at(cell), game.digit_at(cell));
        assert_eq!(loaded.entered(), 1);
        assert!(!loaded.is_given(cell), "an entry came back as a given");
    }

    #[test]
    fn a_resumed_game_carries_on_the_generator_stream() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("puzzle.toml");
        let (mut game, _) = played();

        save(&game, &path).expect("save succeeds");
        let mut loaded = load(&path).expect("load succeeds");

        game.new_puzzle(Difficulty::Easy);
        loaded.new_puzzle(Difficulty::Easy);
        assert_eq!(
            loaded.puzzle(),
            game.puzzle(),
            "a resumed game drew a different next puzzle"
        );
    }

    #[test]
    fn saving_twice_leaves_only_the_latest_game_on_disk() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("puzzle.toml");
        let (mut game, cell) = played();

        save(&game, &path).expect("first save succeeds");
        game.clear(cell).expect("clearing an entry");
        save(&game, &path).expect("second save succeeds");

        assert_eq!(load(&path).expect("loads").digit_at(cell), None);
        assert_eq!(
            fs::read_dir(dir.path()).expect("reads").count(),
            1,
            "no leftover temp file"
        );
    }

    #[test]
    fn a_write_that_never_gets_renamed_leaves_the_previous_save_loadable() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("puzzle.toml");
        let (game, _) = played();

        save(&game, &path).expect("first save succeeds");
        let before = fs::read_to_string(&path).expect("reads");
        fs::write(
            dir.path().join(".paper-sudoku-save-stray"),
            b"garbage, never renamed",
        )
        .expect("writes");

        assert_eq!(load(&path).expect("still loads").board(), game.board());
        assert_eq!(fs::read_to_string(&path).expect("reads"), before);
    }

    #[test]
    fn truncated_bytes_are_reported_as_a_syntax_error_not_a_panic() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("puzzle.toml");
        fs::write(&path, b"difficulty = \"easy\"\ngivens = \"53..7").expect("writes");

        match load(&path) {
            Err(LoadError::Syntax { .. }) => {}
            other => panic!("expected a syntax error, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_file_is_an_io_error_rather_than_an_empty_game() {
        let dir = tempfile::tempdir().expect("a temp dir");
        match load(&dir.path().join("nothing.toml")) {
            Err(LoadError::Io { .. }) => {}
            other => panic!("expected an I/O error, got {other:?}"),
        }
    }

    #[test]
    fn every_way_a_parsed_save_can_still_be_unplayable_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("puzzle.toml");
        let (game, cell) = played();
        let solved_cell = Cell::all()
            .find(|cell| game.is_given(*cell))
            .expect("a puzzle has givens");

        let mut cases: Vec<(&str, SavedPuzzle)> = Vec::new();

        let mut unknown_level = saved_parts(&game);
        unknown_level.difficulty = "impossible".to_owned();
        cases.push(("an unknown difficulty", unknown_level));

        let mut short = saved_parts(&game);
        short.board.truncate(80);
        cases.push(("a board that is not 81 cells", short));

        let mut nonsense = saved_parts(&game);
        nonsense.givens = "x".repeat(81);
        cases.push(("givens that are not digits", nonsense));

        let mut unsolved = saved_parts(&game);
        unsolved.solution = game.puzzle().givens().to_line();
        cases.push(("a solution that is not solved", unsolved));

        let mut dropped = saved_parts(&game);
        let mut without = *game.board();
        without.set(solved_cell, None);
        dropped.board = without.to_line();
        cases.push(("a board missing one of the givens", dropped));

        let mut invented = saved_parts(&game);
        let mut extra = *game.puzzle().givens();
        extra.set(cell, Digit::new(1));
        invented.givens = extra.to_line();
        cases.push(("a given the solution disagrees with", invented));

        let mut illegal = saved_parts(&game);
        let mut broken = *game.board();
        let (row, digit) = Cell::all()
            .find_map(|target| {
                let digit = broken.get(target)?;
                Some((target, digit))
            })
            .expect("the board holds something");
        let free = Cell::all()
            .find(|candidate| {
                broken.get(*candidate).is_none()
                    && candidate.row() == row.row()
                    && !game.is_given(*candidate)
            })
            .expect("some empty cell shares a row with a filled one");
        broken.set(free, Some(digit));
        illegal.board = broken.to_line();
        cases.push(("a board with a duplicate in a row", illegal));

        for (what, parts) in cases {
            write_parts(&path, &parts);
            match load(&path) {
                Err(LoadError::Corrupt { .. }) => {}
                other => panic!("{what} should be corrupt, got {other:?}"),
            }
        }

        // And the untouched parts still load, so the cases above are failing
        // for the reason they claim rather than because the fixture is broken.
        write_parts(&path, &saved_parts(&game));
        assert_eq!(
            load(&path).expect("the fixture loads").board(),
            game.board()
        );
    }

    #[test]
    fn the_save_file_is_readable_by_eye() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("puzzle.toml");
        let (game, _) = played();
        save(&game, &path).expect("save succeeds");

        let text = fs::read_to_string(&path).expect("reads");
        assert!(text.contains("difficulty = \"medium\""), "{text}");
        assert!(text.contains(&game.board().to_line()), "{text}");
        assert_eq!(
            Grid::parse_line(&game.board().to_line()),
            Some(*game.board())
        );
    }
}
