//! Pending vs current surface state, and the buffer lifecycle that makes a
//! commit atomic (WWW-52, WWW-77).
//!
//! [`Surface::commit`] is the one function that moves pending state into
//! current: a damage claim that was never committed, or a buffer that was
//! attached but not yet swapped in, is not a state anything reading
//! [`Surface::current`] can ever observe. That is the "structurally
//! impossible" half-drawn frame WWW-52 asks for.
//!
//! This module has no `unsafe` and touches no memory — it is pure
//! bookkeeping over [`BufferSlot`] and [`Rect`], which is what lets the
//! damage-bounds and buffer-release rules be tested without a real `mmap` at
//! all (`pool` is where those rules meet real memory).

use paper_protocol::{BufferSlot, Damage, MAX_DAMAGE_RECTS, Rect, ShmPoolDescriptor, Size};

/// Why a client's message was refused.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum SurfaceError {
    /// `attach` named a slot the host has not released back to the client —
    /// the state `wl_buffer.release` exists to prevent a client writing
    /// into. Attaching (and so drawing into) a slot in this state is the
    /// violation this ticket's acceptance criteria call "a client writing
    /// before release."
    #[error("buffer slot {0:?} has not been released")]
    BufferBusy(BufferSlot),
    /// `commit` was called with no buffer attached in the pending state.
    #[error("commit with no buffer attached")]
    NothingAttached,
    /// More damage rectangles were claimed than
    /// [`MAX_DAMAGE_RECTS`] allows.
    #[error("{0} damage rects exceeds MAX_DAMAGE_RECTS ({MAX_DAMAGE_RECTS})")]
    TooManyDamageRects(usize),
    /// A damage rectangle is not finite, has a negative origin or extent, or
    /// extends past the surface's own extent.
    #[error("damage rect {0:?} is outside the surface")]
    DamageOutOfBounds(Rect),
    /// `release` named a slot the host is not currently presenting.
    #[error("buffer slot {0:?} is not in flight")]
    NotInFlight(BufferSlot),
}

/// Where a [`BufferSlot`] sits in the attach/commit/release cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    /// Free for the client to attach.
    Free,
    /// Attached in the pending state; not yet committed.
    Attached,
    /// Committed at least once. Only [`Surface::release`] moves a slot out
    /// of this state — a client that draws into it before that is the
    /// exact corruption `wl_buffer.release` exists to prevent.
    Presenting,
}

/// The frame [`Surface::commit`] last swapped into current.
#[derive(Debug, Clone, PartialEq)]
pub struct Committed {
    /// Which slot holds this frame's pixels.
    pub buffer: BufferSlot,
    /// What changed, validated against the surface's extent at commit time.
    pub damage: Damage,
}

/// One client's surface: its extent, its two buffers' release state, and the
/// pending/current split.
#[derive(Debug, Clone)]
pub struct Surface {
    extent: Size,
    slots: [SlotState; 2],
    pending_buffer: Option<BufferSlot>,
    current: Option<Committed>,
}

impl Surface {
    /// A new surface over a pool of this shape. Both slots start `Free`: a
    /// client has written into neither yet, so neither needs releasing
    /// before its first attach.
    pub fn new(descriptor: ShmPoolDescriptor) -> Self {
        Self {
            extent: descriptor.buffer.extent,
            slots: [SlotState::Free, SlotState::Free],
            pending_buffer: None,
            current: None,
        }
    }

    /// Records `slot` as the buffer the next commit will present.
    ///
    /// Refuses a slot the host has not released
    /// ([`SurfaceError::BufferBusy`]) — a client may attach only a buffer it
    /// is actually allowed to be writing into right now. Re-attaching a
    /// different slot before committing frees the one that was pending: it
    /// was never committed, so nothing needs releasing to drop it.
    pub fn attach(&mut self, slot: BufferSlot) -> Result<(), SurfaceError> {
        if self.slot_state(slot) != SlotState::Free {
            return Err(SurfaceError::BufferBusy(slot));
        }
        if let Some(previous) = self.pending_buffer.take()
            && previous != slot
        {
            self.set_slot_state(previous, SlotState::Free);
        }
        self.set_slot_state(slot, SlotState::Attached);
        self.pending_buffer = Some(slot);
        Ok(())
    }

    /// Validates `damage` against this surface's extent and, if it is
    /// legal, swaps the pending buffer into [`Self::current`].
    ///
    /// Validation happens before anything else: an invalid rect leaves
    /// `pending_buffer` and every slot's state exactly as they were, so a
    /// rejected commit is a no-op rather than a partial one. No pixel data
    /// is read or written by this call — that is `pool`'s job, once there
    /// is something (WWW-78's event loop) driving it.
    pub fn commit(&mut self, damage: Damage) -> Result<BufferSlot, SurfaceError> {
        let slot = self.pending_buffer.ok_or(SurfaceError::NothingAttached)?;
        validate_damage(&damage, self.extent)?;
        self.set_slot_state(slot, SlotState::Presenting);
        self.pending_buffer = None;
        self.current = Some(Committed {
            buffer: slot,
            damage,
        });
        Ok(slot)
    }

    /// The host is done reading `slot`'s content; the client may attach it
    /// again.
    ///
    /// Refuses a slot that is not currently presenting
    /// ([`SurfaceError::NotInFlight`]) — releasing a buffer nothing
    /// committed, or releasing the same buffer twice, is a host bug this
    /// catches rather than lets silently double-free the slot.
    pub fn release(&mut self, slot: BufferSlot) -> Result<(), SurfaceError> {
        if self.slot_state(slot) != SlotState::Presenting {
            return Err(SurfaceError::NotInFlight(slot));
        }
        self.set_slot_state(slot, SlotState::Free);
        Ok(())
    }

    /// The most recently committed frame, or `None` before the first
    /// commit.
    pub fn current(&self) -> Option<&Committed> {
        self.current.as_ref()
    }

    fn slot_state(&self, slot: BufferSlot) -> SlotState {
        self.slots[slot_index(slot)]
    }

    fn set_slot_state(&mut self, slot: BufferSlot, state: SlotState) {
        self.slots[slot_index(slot)] = state;
    }
}

const fn slot_index(slot: BufferSlot) -> usize {
    match slot {
        BufferSlot::A => 0,
        BufferSlot::B => 1,
    }
}

fn validate_damage(damage: &Damage, extent: Size) -> Result<(), SurfaceError> {
    // `Damage::regions()` is `None` for `Full` — always valid — and is how
    // `session.rs` reads the same enum, which keeps this crate from needing
    // its own wildcard arm over a `#[non_exhaustive]` type it does not own.
    let Some(regions) = damage.regions() else {
        return Ok(());
    };
    if regions.len() > MAX_DAMAGE_RECTS {
        return Err(SurfaceError::TooManyDamageRects(regions.len()));
    }
    for &rect in regions {
        validate_rect(rect, extent)?;
    }
    Ok(())
}

/// Checked purely arithmetically — no slicing, no pointer arithmetic — so a
/// rejection here can never have touched memory outside (or inside) the
/// pool. `finite`/`non_negative` catch NaN, infinity and a negative origin
/// or extent before `within` even compares them to `extent`; without that
/// order, `x + width <= extent.width` would accept e.g. `x = -1_000_000.0,
/// width = 1_000_001.0`.
fn validate_rect(rect: Rect, extent: Size) -> Result<(), SurfaceError> {
    let finite = rect.x.is_finite()
        && rect.y.is_finite()
        && rect.width.is_finite()
        && rect.height.is_finite();
    let non_negative = rect.x >= 0.0 && rect.y >= 0.0 && rect.width >= 0.0 && rect.height >= 0.0;
    let within =
        rect.x + rect.width <= extent.width as f32 && rect.y + rect.height <= extent.height as f32;
    if finite && non_negative && within {
        Ok(())
    } else {
        Err(SurfaceError::DamageOutOfBounds(rect))
    }
}

#[cfg(test)]
mod tests {
    use super::{Surface, SurfaceError};
    use paper_protocol::{
        BufferSlot, Damage, PixelFormat, Rect, ShmPoolDescriptor, Size, SurfaceDescriptor,
    };

    fn pool() -> ShmPoolDescriptor {
        ShmPoolDescriptor {
            buffer: SurfaceDescriptor::packed(Size::new(100, 100), PixelFormat::Argb8888),
        }
    }

    #[test]
    fn a_fresh_surface_has_no_current_frame() {
        let surface = Surface::new(pool());
        assert!(surface.current().is_none());
    }

    #[test]
    fn attach_damage_commit_becomes_the_current_frame() {
        let mut surface = Surface::new(pool());
        surface.attach(BufferSlot::A).unwrap();
        let slot = surface.commit(Damage::Full).unwrap();
        assert_eq!(slot, BufferSlot::A);
        assert_eq!(surface.current().unwrap().buffer, BufferSlot::A);
        assert_eq!(surface.current().unwrap().damage, Damage::Full);
    }

    #[test]
    fn commit_with_nothing_attached_is_refused() {
        let mut surface = Surface::new(pool());
        assert_eq!(
            surface.commit(Damage::Full),
            Err(SurfaceError::NothingAttached)
        );
    }

    /// The acceptance criterion this pins down: a client that tries to
    /// attach (and so start writing into) a buffer the host has not
    /// released is refused, not silently allowed to corrupt the slot the
    /// host may still be presenting from.
    #[test]
    fn attaching_a_buffer_before_it_is_released_is_refused() {
        let mut surface = Surface::new(pool());
        surface.attach(BufferSlot::A).unwrap();
        surface.commit(Damage::Full).unwrap();

        assert_eq!(
            surface.attach(BufferSlot::A),
            Err(SurfaceError::BufferBusy(BufferSlot::A))
        );

        surface.release(BufferSlot::A).unwrap();
        assert_eq!(surface.attach(BufferSlot::A), Ok(()));
    }

    #[test]
    fn releasing_a_buffer_that_is_not_in_flight_is_refused() {
        let mut surface = Surface::new(pool());
        assert_eq!(
            surface.release(BufferSlot::A),
            Err(SurfaceError::NotInFlight(BufferSlot::A))
        );

        surface.attach(BufferSlot::A).unwrap();
        assert_eq!(
            surface.release(BufferSlot::A),
            Err(SurfaceError::NotInFlight(BufferSlot::A))
        );
    }

    #[test]
    fn both_slots_may_be_in_flight_at_once() {
        let mut surface = Surface::new(pool());
        surface.attach(BufferSlot::A).unwrap();
        surface.commit(Damage::Full).unwrap();
        surface.attach(BufferSlot::B).unwrap();
        surface.commit(Damage::Full).unwrap();

        // Neither is writable again until each is individually released —
        // a client cannot get more than two frames ahead of the host.
        assert_eq!(
            surface.attach(BufferSlot::A),
            Err(SurfaceError::BufferBusy(BufferSlot::A))
        );
        assert_eq!(
            surface.attach(BufferSlot::B),
            Err(SurfaceError::BufferBusy(BufferSlot::B))
        );
    }

    /// The acceptance criterion this pins down: a malformed rect is
    /// rejected, and rejected by pure comparison against `extent` — nothing
    /// in `validate_rect` slices, indexes or dereferences anything, so a
    /// rect built to overflow into memory outside the pool never gets the
    /// chance to.
    #[test]
    fn an_out_of_bounds_rect_never_reaches_the_pool() {
        let mut surface = Surface::new(pool());
        surface.attach(BufferSlot::A).unwrap();

        let malformed = Damage::Regions {
            regions: vec![Rect {
                x: 90.0,
                y: 90.0,
                width: 50.0,
                height: 50.0,
            }],
        };
        assert_eq!(
            surface.commit(malformed),
            Err(SurfaceError::DamageOutOfBounds(Rect {
                x: 90.0,
                y: 90.0,
                width: 50.0,
                height: 50.0,
            }))
        );
        // A rejected commit is a no-op: `A` is still the attached, pending
        // buffer, not silently freed or swapped in with bad damage.
        assert!(surface.current().is_none());
        assert_eq!(
            surface.attach(BufferSlot::A),
            Err(SurfaceError::BufferBusy(BufferSlot::A))
        );
    }

    #[test]
    fn a_negative_or_non_finite_rect_is_rejected() {
        for rect in [
            Rect {
                x: -1.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            Rect {
                x: 0.0,
                y: 0.0,
                width: f32::NAN,
                height: 10.0,
            },
            Rect {
                x: 0.0,
                y: 0.0,
                width: f32::INFINITY,
                height: 10.0,
            },
        ] {
            // A fresh surface per case: a rejected commit leaves `A` still
            // attached (see `an_out_of_bounds_rect_never_reaches_the_pool`),
            // and NaN is never `==` its own copy, so this checks the
            // variant rather than an exact `Rect` a NaN case could never
            // satisfy.
            let mut surface = Surface::new(pool());
            surface.attach(BufferSlot::A).unwrap();
            let result = surface.commit(Damage::Regions {
                regions: vec![rect],
            });
            assert!(
                matches!(result, Err(SurfaceError::DamageOutOfBounds(_))),
                "{result:?}"
            );
        }
    }

    #[test]
    fn too_many_damage_rects_is_rejected_before_any_bounds_check() {
        let mut surface = Surface::new(pool());
        surface.attach(BufferSlot::A).unwrap();

        let regions = vec![
            Rect {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0
            };
            paper_protocol::MAX_DAMAGE_RECTS + 1
        ];
        let count = regions.len();
        assert_eq!(
            surface.commit(Damage::Regions { regions }),
            Err(SurfaceError::TooManyDamageRects(count))
        );
    }
}
