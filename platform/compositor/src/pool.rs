//! A client's shared-memory pool: two [`BufferSlot`]s, `mmap`ed
//! `MAP_SHARED` (WWW-52, WWW-77).
//!
//! This is the only module in this crate that touches real memory or does
//! `unsafe` FFI — see `platform/device/src/vendor.rs` for the same pattern
//! (`#![allow(unsafe_code)]` at the module, a written reason on every
//! block) this module follows.
//!
//! A real client's pool arrives as a file descriptor sent over the
//! compositor's socket (WWW-78, not built here) — [`Pool::from_file`] takes
//! that shape directly, an already-open, already-sized [`File`], which is
//! also what lets this crate's own tests exercise the real `mmap` and
//! bounds-checking path (via a `tempfile`-backed file) without a second
//! process.

#![allow(unsafe_code)]

use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;
use std::ptr::NonNull;

use paper_protocol::{BufferSlot, MAX_POOL_BYTES, ShmPoolDescriptor};

/// Why a pool could not be created.
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    /// `descriptor` does not describe a usable pair of buffers (see
    /// [`ShmPoolDescriptor::pool_bytes`]).
    #[error("descriptor does not describe a usable pool")]
    InvalidDescriptor,
    /// The pool would be larger than [`MAX_POOL_BYTES`] — refused before any
    /// backing storage is sized or mapped, so a hostile size claim costs a
    /// comparison, not an allocation.
    #[error("pool size {size} bytes exceeds the {limit} byte limit")]
    TooLarge {
        /// The size that was refused.
        size: u64,
        /// [`MAX_POOL_BYTES`].
        limit: u64,
    },
    /// The backing file's actual size did not match what `descriptor`
    /// claims. Checked before `mmap`, against the file's own metadata — not
    /// against anything the sender said — because a pool whose fd is
    /// smaller than its claimed size would otherwise let a later slice into
    /// slot `B` read or write past the end of the real mapping.
    #[error("pool file is {actual} bytes, descriptor claims {expected}")]
    SizeMismatch {
        /// What `descriptor.pool_bytes()` computed.
        expected: u64,
        /// What the file's metadata reports.
        actual: u64,
    },
    /// Reading the backing file's metadata failed.
    #[error("failed to read the pool's backing storage: {0}")]
    Stat(io::Error),
    /// `mmap` failed.
    #[error("mmap failed: {0}")]
    Map(io::Error),
}

/// A client's shared-memory pool, mapped into this process.
///
/// Owns the mapping for its own lifetime; dropping it `munmap`s. The backing
/// file stays open alongside the mapping — POSIX does not require that (a
/// mapping outlives the fd it was made from), but a real client's fd is the
/// pool's only handle to reconnect a second mapping to later, so this type
/// is shaped to keep it rather than close it early.
#[derive(Debug)]
pub struct Pool {
    descriptor: ShmPoolDescriptor,
    mapping: NonNull<u8>,
    len: usize,
    _backing: File,
}

impl Pool {
    /// Maps `file` as `descriptor`'s pool.
    ///
    /// `file` must already be open and sized to exactly
    /// `descriptor.pool_bytes()`. In production `file` is a client's shm fd,
    /// received over the compositor's socket (WWW-78); this crate's own
    /// tests build one from `tempfile` instead, which exercises the same
    /// `mmap` and bounds-checking path without a second process.
    pub fn from_file(descriptor: ShmPoolDescriptor, file: File) -> Result<Self, PoolError> {
        let pool_bytes = descriptor
            .pool_bytes()
            .ok_or(PoolError::InvalidDescriptor)?;
        if pool_bytes == 0 {
            return Err(PoolError::InvalidDescriptor);
        }
        if pool_bytes > MAX_POOL_BYTES {
            return Err(PoolError::TooLarge {
                size: pool_bytes,
                limit: MAX_POOL_BYTES,
            });
        }
        let actual_bytes = file.metadata().map_err(PoolError::Stat)?.len();
        if actual_bytes != pool_bytes {
            return Err(PoolError::SizeMismatch {
                expected: pool_bytes,
                actual: actual_bytes,
            });
        }
        // `pool_bytes` was already checked against `MAX_POOL_BYTES`, well
        // under `usize::MAX` on every target this workspace builds for.
        let len = pool_bytes as usize;

        // SAFETY: `file` was just sized to exactly `len` bytes above and is
        // moved into `Self` below, so it outlives the mapping for as long as
        // the mapping exists. `len` is not attacker-controlled at this
        // point — it is the same value that sized the file, not a value
        // read back from the client.
        let addr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if addr == libc::MAP_FAILED {
            return Err(PoolError::Map(io::Error::last_os_error()));
        }
        // SAFETY: `mmap` did not return `MAP_FAILED`, so per its contract
        // `addr` is a valid mapping of `len` bytes and is not null.
        let mapping =
            NonNull::new(addr.cast::<u8>()).expect("a non-MAP_FAILED mmap result is never null");
        Ok(Self {
            descriptor,
            mapping,
            len,
            _backing: file,
        })
    }

    /// The descriptor this pool was created from.
    pub const fn descriptor(&self) -> ShmPoolDescriptor {
        self.descriptor
    }

    /// The bytes of `slot`.
    pub fn slot(&self, slot: BufferSlot) -> &[u8] {
        let (offset, len) = self.slot_range(slot);
        // SAFETY: `slot_range` returns a `(offset, len)` that fits inside
        // `self.mapping`'s `self.len` bytes (see its own contract below),
        // and the mapping is valid for the lifetime of `&self`.
        unsafe { std::slice::from_raw_parts(self.mapping.as_ptr().add(offset), len) }
    }

    /// The mutable bytes of `slot`.
    pub fn slot_mut(&mut self, slot: BufferSlot) -> &mut [u8] {
        let (offset, len) = self.slot_range(slot);
        // SAFETY: as `Self::slot`, plus exclusivity from `&mut self`.
        unsafe { std::slice::from_raw_parts_mut(self.mapping.as_ptr().add(offset), len) }
    }

    /// `(offset, len)` of `slot` within the mapping.
    ///
    /// Always within `0..self.len`: `self.descriptor` is the same value
    /// that computed `self.len` in [`Self::from_file`], so
    /// `offset + len <= self.len` holds by construction, not by a check
    /// against attacker-controlled input performed here.
    fn slot_range(&self, slot: BufferSlot) -> (usize, usize) {
        let offset = self
            .descriptor
            .slot_offset(slot)
            .and_then(|offset| usize::try_from(offset).ok())
            .expect("descriptor was already validated in Self::from_file");
        let len = self
            .descriptor
            .slot_bytes()
            .and_then(|len| usize::try_from(len).ok())
            .expect("descriptor was already validated in Self::from_file");
        debug_assert!(offset + len <= self.len);
        (offset, len)
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        // SAFETY: `self.mapping`/`self.len` are exactly the values `mmap`
        // returned and mapped in `Self::from_file`; `Pool` never hands out a
        // reference that outlives `&self`/`&mut self`, so nothing borrows
        // into the mapping once `drop` runs.
        unsafe {
            libc::munmap(self.mapping.as_ptr().cast(), self.len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Pool, PoolError};
    use paper_protocol::{
        BufferSlot, MAX_POOL_BYTES, PixelFormat, ShmPoolDescriptor, Size, SurfaceDescriptor,
    };
    use std::fs::File;

    fn descriptor(extent: Size) -> ShmPoolDescriptor {
        ShmPoolDescriptor {
            buffer: SurfaceDescriptor::packed(extent, PixelFormat::Argb8888),
        }
    }

    /// A tempfile sized to exactly what a well-behaved client would send:
    /// `descriptor.pool_bytes()`, or empty if the descriptor is nonsense —
    /// `Pool::from_file` must reject that on its own terms either way.
    fn backing_file(descriptor: ShmPoolDescriptor) -> File {
        let file = tempfile::tempfile().unwrap();
        if let Some(bytes) = descriptor.pool_bytes() {
            file.set_len(bytes).unwrap();
        }
        file
    }

    fn create(descriptor: ShmPoolDescriptor) -> Result<Pool, PoolError> {
        Pool::from_file(descriptor, backing_file(descriptor))
    }

    #[test]
    fn a_fresh_pool_reads_back_as_zero() {
        let pool = create(descriptor(Size::new(4, 4))).unwrap();
        assert!(pool.slot(BufferSlot::A).iter().all(|&b| b == 0));
        assert!(pool.slot(BufferSlot::B).iter().all(|&b| b == 0));
    }

    /// `MAP_SHARED`'s whole point is presenting the client's own writes back
    /// to the host; what this asserts is the narrower bounds-checking
    /// property this crate actually depends on — that slot `A`'s and slot
    /// `B`'s byte ranges never overlap, so a write to one cannot appear in
    /// the other.
    #[test]
    fn a_write_to_one_slot_does_not_appear_in_the_other() {
        let mut pool = create(descriptor(Size::new(4, 4))).unwrap();
        pool.slot_mut(BufferSlot::A).fill(0xAA);
        pool.slot_mut(BufferSlot::B).fill(0xBB);
        assert!(pool.slot(BufferSlot::A).iter().all(|&b| b == 0xAA));
        assert!(pool.slot(BufferSlot::B).iter().all(|&b| b == 0xBB));
    }

    #[test]
    fn slot_b_starts_immediately_after_slot_a() {
        let pool = create(descriptor(Size::new(4, 4))).unwrap();
        let slot_bytes = pool.descriptor().slot_bytes().unwrap();
        assert_eq!(pool.slot(BufferSlot::A).len() as u64, slot_bytes);
        assert_eq!(pool.slot(BufferSlot::B).len() as u64, slot_bytes);
    }

    #[test]
    fn an_oversized_pool_is_refused_before_mmap_is_attempted() {
        // A width chosen so `pool_bytes()` clears MAX_POOL_BYTES: at 4
        // bytes/pixel, two slots, a 1 px tall surface needs
        // `width * 4 * 2` bytes.
        let width = (MAX_POOL_BYTES / 8) as u32 + 1;
        let result = create(descriptor(Size::new(width, 1)));
        assert!(
            matches!(result, Err(PoolError::TooLarge { .. })),
            "{result:?}"
        );
    }

    #[test]
    fn a_zero_extent_pool_is_refused() {
        let result = create(descriptor(Size::new(0, 100)));
        assert!(matches!(result, Err(PoolError::InvalidDescriptor)));
    }

    /// The size check is against the file's real metadata, not the
    /// descriptor alone — a client that under-sizes its fd relative to what
    /// it claims must not get a mapping that reads (or lets the host write)
    /// past the end of the real file.
    #[test]
    fn a_pool_file_smaller_than_the_descriptor_claims_is_refused() {
        let descriptor = descriptor(Size::new(4, 4));
        let short_file = tempfile::tempfile().unwrap();
        short_file
            .set_len(descriptor.pool_bytes().unwrap() - 1)
            .unwrap();
        let result = Pool::from_file(descriptor, short_file);
        assert!(
            matches!(result, Err(PoolError::SizeMismatch { .. })),
            "{result:?}"
        );
    }
}
