//! Accumulating per-frame damage claims under ADR-0021's rules.
//!
//! Three apps each carried the same pair of fields — "what has been claimed
//! since the last frame" and "what the last frame drawn actually claimed,
//! held for [`App::damage`](crate::App::damage) to hand back" — and each
//! re-derived the same three absorption rules on top of them:
//! [`Damage::Full`] is absorbing, more rectangles than
//! [`MAX_DAMAGE_RECTS`] collapses to it, and a draw with nothing accumulated
//! behind it also claims everything, because there is no way to tell "nothing
//! changed" apart from "nothing was watching" once the frame is gone.
//!
//! [`DamageAccumulator`] is that pair of fields and those three rules, once.
//! An app that decides what changed by *claiming* it as the change happens —
//! `paper_sudoku`'s per-press claim and `paper_app_store`'s per-action claim
//! are the two shapes this crate has seen — accumulates into one of these and
//! reads [`Self::take_frame`] back in [`App::draw`](crate::App::draw). An app
//! that instead *derives* its damage by diffing the state it painted last
//! time against the state it is about to paint (`paper_settings`) has a
//! different, already-minimal computation and has no claims to accumulate
//! between draws — it has nothing to gain from this type and does not use it.
use paper_protocol::{Damage, MAX_DAMAGE_RECTS, Rect};

/// Accumulates [`Damage`] claims between two frames and resolves them into
/// what [`App::damage`](crate::App::damage) should answer for the frame just
/// drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct DamageAccumulator {
    pending: Damage,
}

impl DamageAccumulator {
    /// Starts claiming everything — the correct answer for a first frame,
    /// where there is no previous frame to be tighter than.
    pub fn new() -> Self {
        Self {
            pending: Damage::Full,
        }
    }

    /// Folds `damage` into what has been claimed since the last
    /// [`Self::take_frame`].
    ///
    /// [`Damage::Full`] is absorbing in both directions, and more rectangles
    /// than [`MAX_DAMAGE_RECTS`] collapses to it too: over-claiming costs one
    /// larger panel update, while silently dropping the overflow costs a
    /// stale rectangle that stays wrong until something else repaints it.
    pub fn claim(&mut self, damage: Damage) {
        let Damage::Regions { regions: added } = damage else {
            self.pending = Damage::Full;
            return;
        };
        let Damage::Regions { regions: pending } = &mut self.pending else {
            // Already `Full`; absorbing.
            return;
        };
        pending.extend(added);
        if pending.len() > MAX_DAMAGE_RECTS {
            self.pending = Damage::Full;
        }
    }

    /// Convenience for claiming one rectangle.
    pub fn claim_rect(&mut self, rect: Rect) {
        self.claim(Damage::Regions {
            regions: vec![rect],
        });
    }

    /// Resolves what has been claimed into this frame's answer, and starts a
    /// fresh, empty claim for the next one.
    ///
    /// A draw with no claim behind it — a first frame, a remapped surface, a
    /// host that simply wants one — cannot safely claim less than everything:
    /// claiming nothing would present nothing.
    pub fn take_frame(&mut self) -> Damage {
        let claimed = std::mem::replace(
            &mut self.pending,
            Damage::Regions {
                regions: Vec::new(),
            },
        );
        match claimed {
            Damage::Regions { ref regions } if regions.is_empty() => Damage::Full,
            claimed => claimed,
        }
    }
}

impl Default for DamageAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::DamageAccumulator;
    use paper_protocol::{Damage, MAX_DAMAGE_RECTS, Rect};

    #[test]
    fn the_first_frame_claims_everything_with_nothing_claimed() {
        let mut acc = DamageAccumulator::new();
        assert_eq!(acc.take_frame(), Damage::Full);
    }

    #[test]
    fn a_frame_with_no_claim_behind_it_also_claims_everything() {
        let mut acc = DamageAccumulator::new();
        acc.take_frame();
        assert_eq!(acc.take_frame(), Damage::Full);
    }

    #[test]
    fn two_claimed_rects_union_into_one_frame() {
        let mut acc = DamageAccumulator::new();
        acc.take_frame();
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(20.0, 20.0, 10.0, 10.0);
        acc.claim_rect(a);
        acc.claim_rect(b);
        assert_eq!(
            acc.take_frame(),
            Damage::Regions {
                regions: vec![a, b]
            }
        );
    }

    #[test]
    fn a_full_claim_absorbs_a_region_claimed_either_before_or_after() {
        let mut acc = DamageAccumulator::new();
        acc.take_frame();
        acc.claim_rect(Rect::new(0.0, 0.0, 10.0, 10.0));
        acc.claim(Damage::Full);
        acc.claim_rect(Rect::new(20.0, 20.0, 10.0, 10.0));
        assert_eq!(acc.take_frame(), Damage::Full);
    }

    #[test]
    fn more_rectangles_than_the_cap_collapses_to_full() {
        let mut acc = DamageAccumulator::new();
        acc.take_frame();
        for index in 0..=MAX_DAMAGE_RECTS {
            acc.claim_rect(Rect::new(index as f32, 0.0, 1.0, 1.0));
        }
        assert_eq!(acc.take_frame(), Damage::Full);
    }

    #[test]
    fn take_frame_starts_a_fresh_claim_for_the_next_one() {
        let mut acc = DamageAccumulator::new();
        acc.take_frame();
        acc.claim_rect(Rect::new(0.0, 0.0, 10.0, 10.0));
        acc.take_frame();
        // Nothing claimed since — the next frame has nothing behind it,
        // which is the "no claim" case, not the previous claim leaking
        // forward.
        assert_eq!(acc.take_frame(), Damage::Full);
    }
}
