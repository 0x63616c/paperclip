//! The generator's randomness: SplitMix64, and nothing else.
//!
//! Deliberately not `rand`. What this crate needs is a seedable, reproducible
//! stream of bits whose exact sequence is part of the save format — a puzzle
//! is restored from its stored grid, but the *next* puzzle comes from the
//! stream's position, so the algorithm cannot change under a dependency bump
//! without changing what a saved game does next. Eleven lines of SplitMix64
//! pinned here are worth more than a dependency here.
//!
//! It is not cryptographic and does not need to be: the one thing it decides
//! is which Sudoku you are handed.

/// A seeded SplitMix64 stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rng {
    state: u64,
}

impl Rng {
    /// A stream at `seed`.
    pub(crate) const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Where the stream has got to, for the save format.
    pub(crate) const fn state(self) -> u64 {
        self.state
    }

    /// The next 64 bits.
    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number below `bound`, or `0` if `bound` is `0`.
    ///
    /// Lemire's multiply-shift rather than a modulo: the bias a modulo leaves
    /// is tiny at these bounds, but "tiny bias in the shuffle that lays out a
    /// puzzle" is not a thing worth having to reason about later.
    pub(crate) fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        let draw = u128::from(self.next_u64()) * bound as u128;
        (draw >> 64) as usize
    }

    /// Fisher-Yates, in place.
    pub(crate) fn shuffle<T>(&mut self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            let other = self.below(index + 1);
            items.swap(index, other);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn the_same_seed_is_the_same_stream() {
        let mut one = Rng::new(7);
        let mut two = Rng::new(7);
        let mut three = Rng::new(8);
        let from_one: Vec<_> = (0..8).map(|_| one.next_u64()).collect();
        let from_two: Vec<_> = (0..8).map(|_| two.next_u64()).collect();
        let from_three: Vec<_> = (0..8).map(|_| three.next_u64()).collect();
        assert_eq!(from_one, from_two);
        assert_ne!(from_one, from_three);
    }

    #[test]
    fn a_stream_can_be_resumed_from_its_recorded_state() {
        let mut original = Rng::new(42);
        let _ = original.next_u64();
        let _ = original.next_u64();
        let mut resumed = Rng::new(original.state());
        assert_eq!(original.next_u64(), resumed.next_u64());
    }

    #[test]
    fn draws_stay_below_their_bound_and_reach_both_ends() {
        let mut rng = Rng::new(1);
        let mut low = false;
        let mut high = false;
        for _ in 0..1_000 {
            let value = rng.below(9);
            assert!(value < 9);
            low |= value == 0;
            high |= value == 8;
        }
        assert!(low && high, "1000 draws from 0..9 covered neither end");
        assert_eq!(rng.below(0), 0, "a zero bound has no answer but zero");
        assert_eq!(rng.below(1), 0);
    }

    #[test]
    fn a_shuffle_permutes_rather_than_replaces() {
        let mut rng = Rng::new(99);
        let mut items: Vec<usize> = (0..81).collect();
        rng.shuffle(&mut items);
        assert_ne!(items, (0..81).collect::<Vec<_>>(), "nothing moved");
        let mut sorted = items.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..81).collect::<Vec<_>>(), "items were lost");
    }
}
