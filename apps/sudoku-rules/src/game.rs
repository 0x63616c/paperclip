//! A puzzle being played: the board, what may be entered on it, and where the
//! next puzzle comes from.

use crate::error::MoveError;
use crate::generate::{Difficulty, Puzzle};
use crate::rng::Rng;
use crate::types::{Cell, Digit, Grid};

/// One in-progress Sudoku.
///
/// Holds the board — givens and entries together — rather than an overlay,
/// because every question asked of it (is that placement legal, is this
/// solved, what is in this cell) is a question about the board, and rebuilding
/// the board per question is how one of those answers eventually disagrees
/// with the others. Which cells are the player's is [`Puzzle::is_given`]'s
/// answer, and that never changes for a given puzzle.
///
/// The generator stream lives here too, so [`Self::new_puzzle`] needs no
/// argument and no ambient randomness, and so a saved game resumes the
/// sequence rather than restarting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Game {
    puzzle: Puzzle,
    board: Grid,
    rng: Rng,
    revision: u64,
}

impl Game {
    /// Generates a puzzle at `difficulty` from `seed` and starts it.
    pub fn start(difficulty: Difficulty, seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let puzzle = Puzzle::generate_with(difficulty, &mut rng);
        Self {
            board: *puzzle.givens(),
            puzzle,
            rng,
            revision: 0,
        }
    }

    /// Replaces the puzzle with a freshly generated one at `difficulty`,
    /// taking the next puzzle from this game's own stream.
    pub fn new_puzzle(&mut self, difficulty: Difficulty) {
        self.puzzle = Puzzle::generate_with(difficulty, &mut self.rng);
        self.board = *self.puzzle.givens();
        self.revision += 1;
    }

    /// The puzzle being played.
    pub fn puzzle(&self) -> &Puzzle {
        &self.puzzle
    }

    /// The level the current puzzle was generated at.
    pub fn difficulty(&self) -> Difficulty {
        self.puzzle.difficulty()
    }

    /// The board as it stands: givens and entries together.
    pub fn board(&self) -> &Grid {
        &self.board
    }

    /// What is in `cell`, given or entered.
    pub fn digit_at(&self, cell: Cell) -> Option<Digit> {
        self.board.get(cell)
    }

    /// Whether `cell` is one of the puzzle's givens.
    pub fn is_given(&self, cell: Cell) -> bool {
        self.puzzle.is_given(cell)
    }

    /// How many digits the player has entered.
    pub fn entered(&self) -> usize {
        self.board.filled() - self.puzzle.given_count()
    }

    /// How many cells are still empty.
    pub fn remaining(&self) -> usize {
        81 - self.board.filled()
    }

    /// Whether the board is a finished, legal Sudoku.
    pub fn is_solved(&self) -> bool {
        self.board.is_solved()
    }

    /// How many times this game has changed.
    ///
    /// The app persists on a change and not on a selection; comparing this
    /// before and after a tap is that question, and it is the same trick
    /// `paper_chess_rules::Game::ply` serves in Chess.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Enters `digit` in `cell`.
    ///
    /// # Errors
    ///
    /// [`MoveError::Solved`] once the puzzle is finished, [`MoveError::Given`]
    /// for one of the puzzle's own clues, and [`MoveError::Conflict`] when the
    /// digit is already in that row, column or block — naming the cell that
    /// holds it.
    ///
    /// A digit that breaks no constraint is accepted even when it is not the
    /// digit the solution has there. That is deliberate: the alternative is an
    /// app that silently solves the puzzle for you, and a wrong-but-legal
    /// guess is part of playing Sudoku rather than a bug to be prevented.
    pub fn set(&mut self, cell: Cell, digit: Digit) -> Result<(), MoveError> {
        if self.is_solved() {
            return Err(MoveError::Solved);
        }
        if self.is_given(cell) {
            return Err(MoveError::Given);
        }
        if self.board.get(cell) == Some(digit) {
            return Ok(());
        }
        // Checked against a board with the cell emptied, so replacing a digit
        // never conflicts with the digit being replaced.
        let previous = self.board.get(cell);
        self.board.set(cell, None);
        if let Some(with) = self.board.conflict(cell, digit) {
            self.board.set(cell, previous);
            return Err(MoveError::Conflict { with });
        }
        self.board.set(cell, Some(digit));
        self.revision += 1;
        Ok(())
    }

    /// Empties `cell`.
    ///
    /// # Errors
    ///
    /// [`MoveError::Solved`] once the puzzle is finished, and
    /// [`MoveError::Given`] for one of the puzzle's own clues. Clearing an
    /// already-empty cell succeeds and changes nothing.
    pub fn clear(&mut self, cell: Cell) -> Result<(), MoveError> {
        if self.is_solved() {
            return Err(MoveError::Solved);
        }
        if self.is_given(cell) {
            return Err(MoveError::Given);
        }
        if self.board.get(cell).is_none() {
            return Ok(());
        }
        self.board.set(cell, None);
        self.revision += 1;
        Ok(())
    }

    /// Where the generator stream has got to, for the save format.
    pub(crate) fn rng_state(&self) -> u64 {
        self.rng.state()
    }

    /// Rebuilds a game from a save file's parts.
    pub(crate) fn restore(puzzle: Puzzle, board: Grid, rng_state: u64) -> Self {
        Self {
            puzzle,
            board,
            rng: Rng::new(rng_state),
            revision: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Game;
    use crate::error::MoveError;
    use crate::generate::Difficulty;
    use crate::types::{Cell, Digit};

    fn started() -> Game {
        Game::start(Difficulty::Easy, 2_026)
    }

    /// The first cell the player is allowed to write in.
    fn first_empty(game: &Game) -> Cell {
        Cell::all()
            .find(|cell| game.digit_at(*cell).is_none())
            .expect("a puzzle has empty cells")
    }

    #[test]
    fn a_started_game_is_its_puzzles_givens_and_nothing_else() {
        let game = started();
        assert_eq!(game.board(), game.puzzle().givens());
        assert_eq!(game.entered(), 0);
        assert_eq!(game.remaining(), 81 - game.puzzle().given_count());
        assert!(!game.is_solved());
        assert_eq!(game.revision(), 0);
    }

    #[test]
    fn a_given_cannot_be_overwritten_or_cleared() {
        let mut game = started();
        let given = Cell::all()
            .find(|cell| game.is_given(*cell))
            .expect("a puzzle has givens");
        let held = game.digit_at(given).expect("a given holds a digit");

        assert_eq!(
            game.set(given, Digit::new(1).unwrap()),
            Err(MoveError::Given)
        );
        assert_eq!(game.clear(given), Err(MoveError::Given));
        assert_eq!(game.digit_at(given), Some(held));
        assert_eq!(game.revision(), 0, "a refused move is not a change");
    }

    #[test]
    fn an_invalid_placement_is_rejected_and_names_the_cell_in_the_way() {
        let mut game = started();
        let cell = first_empty(&game);
        // Some digit is already in this cell's row, column or block: the
        // puzzle has 42 givens across 81 cells, so an empty cell with none of
        // its twenty peers filled does not occur.
        let (digit, occupied) = Digit::ALL
            .into_iter()
            .find_map(|digit| game.board().conflict(cell, digit).map(|with| (digit, with)))
            .expect("an empty cell has some digit ruled out");

        assert_eq!(
            game.set(cell, digit),
            Err(MoveError::Conflict { with: occupied })
        );
        assert_eq!(game.digit_at(cell), None);
        assert_eq!(game.revision(), 0);
        assert!(
            format!("{}", MoveError::Conflict { with: occupied }).contains(&occupied.name()),
            "the message should name the cell"
        );
    }

    #[test]
    fn a_legal_entry_lands_and_can_be_replaced_or_cleared() {
        let mut game = started();
        let cell = first_empty(&game);
        let first = Digit::ALL
            .into_iter()
            .find(|digit| game.board().accepts(cell, *digit))
            .expect("something is placeable");

        game.set(cell, first).expect("a legal entry");
        assert_eq!(game.digit_at(cell), Some(first));
        assert_eq!(game.entered(), 1);
        assert_eq!(game.revision(), 1);

        // Re-entering the same digit is a no-op rather than a conflict with
        // itself, and does not count as a change.
        game.set(cell, first).expect("the same digit again");
        assert_eq!(game.revision(), 1);

        if let Some(second) = Digit::ALL
            .into_iter()
            .find(|digit| *digit != first && game.board().accepts(cell, *digit))
        {
            game.set(cell, second).expect("replacing an entry");
            assert_eq!(game.digit_at(cell), Some(second));
        }

        game.clear(cell).expect("clearing an entry");
        assert_eq!(game.digit_at(cell), None);
        assert_eq!(game.entered(), 0);
        game.clear(cell).expect("clearing an empty cell is fine");
    }

    #[test]
    fn filling_in_the_solution_is_recognised_as_solved_and_ends_the_game() {
        let mut game = started();
        let solution = *game.puzzle().solution();
        for cell in Cell::all() {
            if !game.is_given(cell) {
                let digit = solution.get(cell).expect("a solution is complete");
                game.set(cell, digit).expect("the solution is legal");
            }
        }

        assert!(game.is_solved());
        assert_eq!(game.remaining(), 0);
        assert_eq!(game.board(), &solution);

        let cell = Cell::all()
            .find(|cell| !game.is_given(*cell))
            .expect("a puzzle has non-given cells");
        assert_eq!(
            game.set(cell, Digit::new(1).unwrap()),
            Err(MoveError::Solved)
        );
        assert_eq!(game.clear(cell), Err(MoveError::Solved));
    }

    #[test]
    fn a_new_puzzle_replaces_the_board_and_does_not_repeat_itself() {
        let mut game = started();
        let cell = first_empty(&game);
        let digit = Digit::ALL
            .into_iter()
            .find(|digit| game.board().accepts(cell, *digit))
            .expect("something is placeable");
        game.set(cell, digit).expect("a legal entry");

        let first = *game.puzzle();
        game.new_puzzle(Difficulty::Hard);
        assert_ne!(game.puzzle(), &first, "the same puzzle came back");
        assert_eq!(game.difficulty(), Difficulty::Hard);
        assert_eq!(game.board(), game.puzzle().givens(), "entries carried over");
        assert_eq!(game.entered(), 0);

        let second = *game.puzzle();
        game.new_puzzle(Difficulty::Hard);
        assert_ne!(game.puzzle(), &second, "the stream did not advance");
    }
}
