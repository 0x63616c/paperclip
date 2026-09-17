//! Generating a puzzle, and the difficulty levels that describe one.

use crate::rng::Rng;
use crate::solve;
use crate::types::{Cell, Grid};

/// How much of the solution a puzzle gives away.
///
/// Clue count, not solving technique. Grading a Sudoku by the hardest
/// technique it needs means implementing those techniques, and a generator
/// that must prove "this one needs an X-wing" is a much larger thing than
/// this app wants. Clue count is the honest approximation: fewer givens
/// generally means a harder puzzle, and the label says nothing it cannot
/// back up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Difficulty {
    /// Roughly 42 givens.
    Easy,
    /// Roughly 34 givens.
    Medium,
    /// Roughly 28 givens.
    Hard,
}

impl Difficulty {
    /// Every level, easiest first.
    pub const ALL: [Difficulty; 3] = [Difficulty::Easy, Difficulty::Medium, Difficulty::Hard];

    /// The label a screen shows.
    pub const fn label(self) -> &'static str {
        match self {
            Difficulty::Easy => "EASY",
            Difficulty::Medium => "MEDIUM",
            Difficulty::Hard => "HARD",
        }
    }

    /// The spelling the save file uses.
    pub const fn slug(self) -> &'static str {
        match self {
            Difficulty::Easy => "easy",
            Difficulty::Medium => "medium",
            Difficulty::Hard => "hard",
        }
    }

    /// The level [`Self::slug`] names, or `None`.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.slug() == text)
    }

    /// How many givens generation aims to leave.
    ///
    /// A target, not a guarantee: removal stops early rather than give up
    /// uniqueness, so a generated puzzle can have more givens than this and
    /// never fewer.
    pub const fn target_givens(self) -> usize {
        match self {
            Difficulty::Easy => 42,
            Difficulty::Medium => 34,
            Difficulty::Hard => 28,
        }
    }

    /// The next level, wrapping — what the DIFFICULTY action steps through.
    pub const fn next(self) -> Self {
        match self {
            Difficulty::Easy => Difficulty::Medium,
            Difficulty::Medium => Difficulty::Hard,
            Difficulty::Hard => Difficulty::Easy,
        }
    }
}

/// A generated puzzle: the givens, and the one solution they have.
///
/// "The one" is checked, not assumed — [`Self::generate`] never removes a
/// clue without confirming with [`solve::solutions`] that what is left still
/// has exactly one solution. A puzzle with two solutions is not a hard
/// puzzle, it is a broken one, and it is the single failure a Sudoku
/// generator can ship that a player cannot work around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Puzzle {
    givens: Grid,
    solution: Grid,
    difficulty: Difficulty,
}

impl Puzzle {
    /// Generates a puzzle at `difficulty` from `seed`.
    ///
    /// Deterministic: the same seed and level always produce the same puzzle.
    pub fn generate(difficulty: Difficulty, seed: u64) -> Self {
        Self::generate_with(difficulty, &mut Rng::new(seed))
    }

    /// [`Self::generate`], advancing a stream the caller keeps.
    pub(crate) fn generate_with(difficulty: Difficulty, rng: &mut Rng) -> Self {
        let solution = solve::complete(rng);
        let mut givens = solution;
        let target = difficulty.target_givens();

        let mut order: Vec<Cell> = Cell::all().collect();
        rng.shuffle(&mut order);
        let mut remaining = givens.filled();
        for cell in order {
            if remaining <= target {
                break;
            }
            let removed = givens.get(cell);
            givens.set(cell, None);
            if solve::solutions(&givens, 2) == 1 {
                remaining -= 1;
            } else {
                // Uniqueness is not negotiable, so this clue stays and the
                // puzzle ends up above its target.
                givens.set(cell, removed);
            }
        }

        Self {
            givens,
            solution,
            difficulty,
        }
    }

    /// The clues the puzzle starts with.
    pub fn givens(&self) -> &Grid {
        &self.givens
    }

    /// The solution those clues have.
    pub fn solution(&self) -> &Grid {
        &self.solution
    }

    /// Which level it was generated at.
    pub fn difficulty(&self) -> Difficulty {
        self.difficulty
    }

    /// Whether `cell` is one of the givens, and therefore not editable.
    pub fn is_given(&self, cell: Cell) -> bool {
        self.givens.get(cell).is_some()
    }

    /// How many clues it starts with.
    pub fn given_count(&self) -> usize {
        self.givens.filled()
    }

    /// Rebuilds a puzzle from stored parts, checking they belong together.
    ///
    /// What is checked is consistency, not uniqueness: that the solution is a
    /// finished legal grid and that every given agrees with it. Re-deriving
    /// uniqueness would mean a full solution count on every load, and a
    /// save file whose solution matches its givens cannot mislead a player
    /// even if its clue set would also admit some other solution.
    pub(crate) fn from_parts(
        givens: Grid,
        solution: Grid,
        difficulty: Difficulty,
    ) -> Result<Self, String> {
        if !solution.is_solved() {
            return Err("the stored solution is not a finished, legal grid".to_owned());
        }
        for cell in Cell::all() {
            if let Some(digit) = givens.get(cell)
                && solution.get(cell) != Some(digit)
            {
                return Err(format!(
                    "the given at {} does not match the stored solution",
                    cell.name()
                ));
            }
        }
        Ok(Self {
            givens,
            solution,
            difficulty,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Difficulty, Puzzle};
    use crate::solve;
    use crate::types::{Cell, Grid};

    #[test]
    fn a_generated_puzzle_has_exactly_one_solution() {
        for (index, difficulty) in Difficulty::ALL.into_iter().enumerate() {
            let puzzle = Puzzle::generate(difficulty, 1_000 + index as u64);
            assert_eq!(
                solve::solutions(puzzle.givens(), 2),
                1,
                "{} is not a unique puzzle",
                difficulty.label()
            );
        }
    }

    #[test]
    fn a_generated_puzzles_solution_is_the_solution_of_its_givens() {
        let puzzle = Puzzle::generate(Difficulty::Medium, 7);
        assert!(puzzle.solution().is_solved());
        assert_eq!(
            solve::solve(puzzle.givens()).as_ref(),
            Some(puzzle.solution())
        );
        for cell in Cell::all() {
            if let Some(digit) = puzzle.givens().get(cell) {
                assert_eq!(puzzle.solution().get(cell), Some(digit));
                assert!(puzzle.is_given(cell));
            } else {
                assert!(!puzzle.is_given(cell));
            }
        }
    }

    #[test]
    fn fewer_givens_is_what_a_harder_level_means() {
        // Per level, and in order: the clue count reaches its target, and a
        // harder level leaves fewer clues than an easier one.
        let mut counts = Vec::new();
        for difficulty in Difficulty::ALL {
            let puzzle = Puzzle::generate(difficulty, 4);
            let givens = puzzle.given_count();
            assert!(
                givens >= difficulty.target_givens(),
                "{} came out below its target: {givens}",
                difficulty.label()
            );
            assert!(
                givens <= difficulty.target_givens() + 8,
                "{} left {givens} clues, far above its {} target",
                difficulty.label(),
                difficulty.target_givens()
            );
            counts.push(givens);
        }
        assert!(
            counts[0] > counts[1],
            "easy should give more away: {counts:?}"
        );
        assert!(
            counts[1] > counts[2],
            "hard should give less away: {counts:?}"
        );
    }

    #[test]
    fn generation_is_deterministic_in_its_seed() {
        let one = Puzzle::generate(Difficulty::Hard, 12_345);
        let again = Puzzle::generate(Difficulty::Hard, 12_345);
        let other = Puzzle::generate(Difficulty::Hard, 12_346);
        assert_eq!(one, again);
        assert_ne!(one.givens(), other.givens());
    }

    #[test]
    fn the_difficulty_labels_round_trip_and_cycle() {
        for difficulty in Difficulty::ALL {
            assert_eq!(Difficulty::parse(difficulty.slug()), Some(difficulty));
            assert!(!difficulty.label().is_empty());
        }
        assert_eq!(Difficulty::parse("impossible"), None);
        assert_eq!(Difficulty::Easy.next(), Difficulty::Medium);
        assert_eq!(Difficulty::Hard.next(), Difficulty::Easy);
    }

    #[test]
    fn stored_parts_that_do_not_belong_together_are_refused() {
        let puzzle = Puzzle::generate(Difficulty::Easy, 3);
        let solution = *puzzle.solution();

        assert!(
            Puzzle::from_parts(*puzzle.givens(), solution, Difficulty::Easy).is_ok(),
            "its own parts are rejected"
        );

        // A solution with a hole in it is not a solution.
        let mut holed = solution;
        holed.set(Cell::new(0, 0).unwrap(), None);
        assert!(Puzzle::from_parts(*puzzle.givens(), holed, Difficulty::Easy).is_err());

        // A given that contradicts the solution is the corruption that
        // matters: it would show the player a clue no solution agrees with.
        let mut lying = *puzzle.givens();
        let cell = Cell::all()
            .find(|cell| puzzle.is_given(*cell))
            .expect("a puzzle has givens");
        let wrong = crate::types::Digit::ALL
            .into_iter()
            .find(|digit| Some(*digit) != solution.get(cell))
            .expect("some other digit exists");
        lying.set(cell, Some(wrong));
        assert!(Puzzle::from_parts(lying, solution, Difficulty::Easy).is_err());

        assert!(Puzzle::from_parts(Grid::EMPTY, solution, Difficulty::Easy).is_ok());
    }
}
