//! The two acceptance criteria WWW-77 owns, proven end to end against a
//! real `mmap`ed [`Pool`] rather than against `surface`'s pure state alone.

use paper_compositor::{Pool, Surface};
use paper_protocol::{
    BufferSlot, Damage, PixelFormat, Rect, ShmPoolDescriptor, Size, SurfaceDescriptor,
};

fn pool_of(extent: Size) -> (ShmPoolDescriptor, Pool) {
    let descriptor = ShmPoolDescriptor {
        buffer: SurfaceDescriptor::packed(extent, PixelFormat::Argb8888),
    };
    let file = tempfile::tempfile().unwrap();
    file.set_len(descriptor.pool_bytes().unwrap()).unwrap();
    (descriptor, Pool::from_file(descriptor, file).unwrap())
}

/// A malformed damage rect is rejected without touching memory outside the
/// pool.
///
/// "Without touching memory outside the pool" is checked literally here: the
/// whole mapping — both slots — is filled with a sentinel byte before the
/// rejected commit, and every byte of it, including the region the bad rect
/// claimed, is still the sentinel afterwards. `Surface::commit` never had a
/// `Pool` to write through in the first place (see `surface`'s module doc),
/// which is what makes this true rather than merely tested.
#[test]
fn a_malformed_damage_rect_is_rejected_without_touching_the_pool() {
    let extent = Size::new(64, 64);
    let (descriptor, mut pool) = pool_of(extent);
    pool.slot_mut(BufferSlot::A).fill(0x5A);
    pool.slot_mut(BufferSlot::B).fill(0x5A);

    let mut surface = Surface::new(descriptor);
    surface.attach(BufferSlot::A).unwrap();

    let out_of_bounds = Rect {
        x: 60.0,
        y: 60.0,
        width: 100.0,
        height: 100.0,
    };
    let result = surface.commit(Damage::Regions {
        regions: vec![out_of_bounds],
    });
    assert!(result.is_err(), "malformed rect was accepted: {result:?}");
    assert!(surface.current().is_none());

    assert!(pool.slot(BufferSlot::A).iter().all(|&b| b == 0x5A));
    assert!(pool.slot(BufferSlot::B).iter().all(|&b| b == 0x5A));
}

/// Buffer release is implemented, and a client writing before release is
/// detected.
///
/// "Writing before release" is a client calling `attach` on a slot the host
/// has not released — the only way this crate lets a client start drawing
/// into a slot at all. A slot committed once and never released cannot be
/// attached again; only after `Surface::release` does the same slot become
/// legal to attach and, through `Pool::slot_mut`, legal to write into.
#[test]
fn a_client_cannot_reattach_a_buffer_before_the_host_releases_it() {
    let extent = Size::new(4, 4);
    let (descriptor, mut pool) = pool_of(extent);

    let mut surface = Surface::new(descriptor);
    surface.attach(BufferSlot::A).unwrap();
    pool.slot_mut(BufferSlot::A).fill(0x11);
    surface.commit(Damage::Full).unwrap();

    // The host is still "presenting" A: a client trying to write the next
    // frame into the same slot is refused at the protocol level.
    assert!(surface.attach(BufferSlot::A).is_err());

    surface.release(BufferSlot::A).unwrap();
    assert!(surface.attach(BufferSlot::A).is_ok());
    pool.slot_mut(BufferSlot::A).fill(0x22);
    surface.commit(Damage::Full).unwrap();
    assert!(pool.slot(BufferSlot::A).iter().all(|&b| b == 0x22));
}
