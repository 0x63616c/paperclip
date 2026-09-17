//! Solving, and counting solutions — which is how uniqueness is decided.
//!
//! One backtracking search over bitmask constraint sets, picking the most
//! constrained empty cell first (MRV). The heuristic is not decoration:
//! generation calls [`solutions`] once per cell it tries to remove, so the
//! difference between "pick the tightest cell" and "pick the next cell" is the
//! difference between a puzzle appearing instantly and a visible pause on a
//! 1.8 GHz tablet.

use crate::rng::Rng;
use crate::types::{ALL_DIGITS, Cell, Digit, Grid};

/// How many solutions `grid` has, counting no further than `limit`.
///
/// `limit` is the whole point of the signature: uniqueness is
/// `solutions(grid, 2) == 1`, and stopping at two is what makes it cheap.
/// Counting an under-constrained grid to exhaustion is not something any
/// caller here wants — an empty grid has 6,670,903,752,021,072,936,960 of
/// them.
pub fn solutions(grid: &Grid, limit: usize) -> usize {
    let Some(mut search) = Search::load(grid) else {
        // An already-invalid grid has no solutions, and the search cannot be
        // trusted to discover that: it only ever checks the cells it fills.
        return 0;
    };
    let mut found = 0;
    search.count(limit, &mut found);
    found
}

/// One solution of `grid`, or `None` if it has none.
///
/// Which one is unspecified when there are several; ask [`solutions`] first if
/// that matters.
pub fn solve(grid: &Grid) -> Option<Grid> {
    let mut search = Search::load(grid)?;
    search.fill(&mut None).then(|| search.grid())
}

/// Whether `grid` has exactly one solution.
pub fn is_unique(grid: &Grid) -> bool {
    solutions(grid, 2) == 1
}

/// A complete, legal grid drawn from `rng`.
///
/// Always succeeds: an empty grid is solvable, and randomising the order
/// candidates are tried in cannot make it unsolvable — only slower to find.
pub(crate) fn complete(rng: &mut Rng) -> Grid {
    let mut search = Search::load(&Grid::EMPTY)
        .unwrap_or_else(|| unreachable!("an empty grid breaks no constraint"));
    let filled = search.fill(&mut Some(rng));
    debug_assert!(filled, "an empty grid always completes");
    search.grid()
}

/// The search state: the digits placed so far, and what each unit still
/// allows.
#[derive(Debug, Clone)]
struct Search {
    /// The digit in each cell, `0` for empty.
    values: [u8; 81],
    /// Digits already used, per row, column and block.
    rows: [u16; 9],
    columns: [u16; 9],
    blocks: [u16; 9],
}

impl Search {
    /// Loads `grid`, or `None` if it already breaks a constraint.
    fn load(grid: &Grid) -> Option<Self> {
        let mut search = Self {
            values: [0; 81],
            rows: [0; 9],
            columns: [0; 9],
            blocks: [0; 9],
        };
        for cell in Cell::all() {
            if let Some(digit) = grid.get(cell) {
                if search.used(cell) & digit.mask() != 0 {
                    return None;
                }
                search.place(cell, digit.value());
            }
        }
        Some(search)
    }

    /// The digits `cell`'s row, column and block have between them.
    fn used(&self, cell: Cell) -> u16 {
        self.rows[cell.row() as usize]
            | self.columns[cell.column() as usize]
            | self.blocks[cell.block() as usize]
    }

    fn place(&mut self, cell: Cell, value: u8) {
        let mask = 1u16 << value;
        self.values[cell.index()] = value;
        self.rows[cell.row() as usize] |= mask;
        self.columns[cell.column() as usize] |= mask;
        self.blocks[cell.block() as usize] |= mask;
    }

    fn lift(&mut self, cell: Cell, value: u8) {
        let mask = !(1u16 << value);
        self.values[cell.index()] = 0;
        self.rows[cell.row() as usize] &= mask;
        self.columns[cell.column() as usize] &= mask;
        self.blocks[cell.block() as usize] &= mask;
    }

    /// The empty cell with the fewest candidates, and that candidate mask.
    ///
    /// `None` when the grid is full. A returned mask of `0` is a dead end,
    /// which is exactly what the caller needs to know before recursing.
    fn most_constrained(&self) -> Option<(Cell, u16)> {
        let mut best: Option<(Cell, u16, u32)> = None;
        for cell in Cell::all() {
            if self.values[cell.index()] != 0 {
                continue;
            }
            let candidates = ALL_DIGITS & !self.used(cell);
            let count = candidates.count_ones();
            if count == 0 {
                return Some((cell, 0));
            }
            if best.is_none_or(|(_, _, fewest)| count < fewest) {
                best = Some((cell, candidates, count));
            }
            if count == 1 {
                break;
            }
        }
        best.map(|(cell, candidates, _)| (cell, candidates))
    }

    /// Counts solutions into `found`, stopping once it reaches `limit`.
    fn count(&mut self, limit: usize, found: &mut usize) {
        if *found >= limit {
            return;
        }
        let Some((cell, candidates)) = self.most_constrained() else {
            *found += 1;
            return;
        };
        for digit in Digit::ALL {
            if candidates & digit.mask() == 0 {
                continue;
            }
            self.place(cell, digit.value());
            self.count(limit, found);
            self.lift(cell, digit.value());
            if *found >= limit {
                return;
            }
        }
    }

    /// Fills the grid, taking the first solution found. With `rng`, candidate
    /// order is shuffled, which is what makes generated puzzles differ.
    fn fill(&mut self, rng: &mut Option<&mut Rng>) -> bool {
        let Some((cell, candidates)) = self.most_constrained() else {
            return true;
        };
        let mut order: Vec<Digit> = Digit::ALL
            .into_iter()
            .filter(|digit| candidates & digit.mask() != 0)
            .collect();
        if let Some(rng) = rng.as_deref_mut() {
            rng.shuffle(&mut order);
        }
        for digit in order {
            self.place(cell, digit.value());
            if self.fill(rng) {
                return true;
            }
            self.lift(cell, digit.value());
        }
        false
    }

    fn grid(&self) -> Grid {
        let mut grid = Grid::EMPTY;
        for cell in Cell::all() {
            grid.set(cell, Digit::new(self.values[cell.index()]));
        }
        grid
    }
}

#[cfg(test)]
mod tests {
    use super::{complete, is_unique, solutions, solve};
    use crate::rng::Rng;
    use crate::types::{Cell, Digit, Grid};

    /// The puzzle from Wikipedia's Sudoku article, and its solution.
    const PUZZLE: &str =
        "53..7....6..195....98....6.8...6...34..8.3..17...2...6.6....28....419..5....8..79";
    const SOLUTION: &str =
        "534678912672195348198342567859761423426853791713924856961537284287419635345286179";

    fn grid(line: &str) -> Grid {
        Grid::parse_line(line).expect("a valid grid line")
    }

    #[test]
    fn a_published_puzzle_solves_to_its_published_solution() {
        let solved = solve(&grid(PUZZLE)).expect("it solves");
        assert_eq!(solved.to_line(), SOLUTION);
        assert!(solved.is_solved());
    }

    #[test]
    fn a_published_puzzle_has_exactly_one_solution() {
        assert_eq!(solutions(&grid(PUZZLE), 2), 1);
        assert!(is_unique(&grid(PUZZLE)));
    }

    #[test]
    fn a_solved_grid_is_its_own_single_solution() {
        assert_eq!(solutions(&grid(SOLUTION), 2), 1);
        assert_eq!(
            solve(&grid(SOLUTION)).map(|solved| solved.to_line()),
            Some(SOLUTION.to_owned())
        );
    }

    #[test]
    fn a_swappable_rectangle_has_two_solutions_and_is_not_reported_unique() {
        // The one way a Sudoku can be ambiguous that is easy to construct:
        // four cells at the corners of a rectangle whose two columns share a
        // block, holding two digits diagonally. Emptying them leaves a grid
        // that can be filled either way round, and every other cell forced.
        // This is the "unavoidable set" a generator must never remove whole,
        // which is exactly what the uniqueness count is there to catch.
        let solved = grid(SOLUTION);
        let corners = Cell::all()
            .flat_map(|first| Cell::all().map(move |second| (first, second)))
            .find(|(first, second)| {
                first.row() < second.row()
                    && first.column() < second.column()
                    && first.column() / 3 == second.column() / 3
                    && solved.get(*first)
                        == solved.get(Cell::new(second.row(), second.column()).unwrap())
                    && solved.get(Cell::new(first.row(), second.column()).unwrap())
                        == solved.get(Cell::new(second.row(), first.column()).unwrap())
            })
            .map(|(first, second)| {
                [
                    first,
                    Cell::new(first.row(), second.column()).unwrap(),
                    Cell::new(second.row(), first.column()).unwrap(),
                    second,
                ]
            })
            .expect("a full grid contains a swappable rectangle");

        let mut ambiguous = solved;
        for cell in corners {
            ambiguous.set(cell, None);
        }
        assert!(ambiguous.is_valid());
        assert_eq!(solutions(&ambiguous, 2), 2);
        assert!(!is_unique(&ambiguous));
    }

    #[test]
    fn enough_clues_removed_always_costs_uniqueness() {
        // The other direction, without constructing anything: a solved grid is
        // unique, and stripping it cell by cell must eventually stop being so.
        let mut stripped = grid(SOLUTION);
        assert!(is_unique(&stripped));
        let lost = Cell::all().position(|cell| {
            stripped.set(cell, None);
            !is_unique(&stripped)
        });
        assert!(
            lost.is_some(),
            "an empty grid was still reported as having one solution"
        );
    }

    #[test]
    fn an_unsolvable_grid_reports_no_solutions_rather_than_looping() {
        // Two cells in one row leave the third nothing legal to be: 1..8 are
        // spoken for in the row and 9 is spoken for in the column.
        let mut stuck = Grid::EMPTY;
        for column in 0..8u8 {
            stuck.set(Cell::new(0, column).unwrap(), Digit::new(column + 1));
        }
        stuck.set(Cell::new(1, 8).unwrap(), Digit::new(9));
        assert_eq!(solutions(&stuck, 2), 0);
        assert_eq!(solve(&stuck), None);
    }

    #[test]
    fn an_already_invalid_grid_has_no_solutions() {
        let mut duplicated = Grid::EMPTY;
        duplicated.set(Cell::new(0, 0).unwrap(), Digit::new(4));
        duplicated.set(Cell::new(0, 5).unwrap(), Digit::new(4));
        assert!(!duplicated.is_valid());
        assert_eq!(solutions(&duplicated, 2), 0);
        assert_eq!(solve(&duplicated), None);
    }

    #[test]
    fn the_limit_is_respected_rather_than_counting_forever() {
        // An empty grid has 6.67e21 solutions. Counting to five must return
        // five, quickly.
        assert_eq!(solutions(&Grid::EMPTY, 5), 5);
        assert_eq!(solutions(&Grid::EMPTY, 1), 1);
        assert_eq!(solutions(&Grid::EMPTY, 0), 0);
    }

    #[test]
    fn a_random_complete_grid_is_a_legal_finished_sudoku() {
        let mut rng = Rng::new(2026);
        let first = complete(&mut rng);
        assert!(first.is_solved());
        let second = complete(&mut rng);
        assert!(second.is_solved());
        assert_ne!(first, second, "the stream produced the same grid twice");
    }

    #[test]
    fn a_complete_grid_is_deterministic_in_its_seed() {
        assert_eq!(
            complete(&mut Rng::new(11)).to_line(),
            complete(&mut Rng::new(11)).to_line()
        );
        assert_ne!(
            complete(&mut Rng::new(11)).to_line(),
            complete(&mut Rng::new(12)).to_line()
        );
    }
}
