//! The compositor core: shared-memory client pools, and the pending/current
//! buffer lifecycle that keeps a commit atomic (WWW-52, WWW-77).
//!
//! Two pieces, deliberately kept apart:
//!
//! - [`pool`] is the only place this crate touches real memory: it owns an
//!   `mmap`ed `MAP_SHARED` region and hands out bounds-checked slices into
//!   its two [`BufferSlot`](paper_protocol::BufferSlot)s.
//! - [`surface`] is pure state: attach, damage and commit rules, validated
//!   against a [`Size`](paper_protocol::Size) and a slot's release state,
//!   with no `unsafe` and no I/O anywhere in it.
//!
//! A malformed damage rect is therefore rejected by [`surface`] alone,
//! before [`pool`] is ever asked to hand out a slice for it — see
//! `surface::tests::an_out_of_bounds_rect_never_reaches_the_pool` for the
//! test that pins this ordering down.
//!
//! ## What this crate does not do yet
//!
//! There is no socket, no client connection, and no running process here.
//! WWW-52 split those out deliberately: the non-blocking accept loop and
//! crash/hang teardown are WWW-78, and moving a real client (Home, the test
//! card, ...) onto this wire is WWW-81. Both need the state this crate
//! defines to exist first, which is what makes this the foundation rather
//! than a partial version of either.

pub mod pool;
pub mod surface;

pub use pool::{Pool, PoolError};
pub use surface::{Committed, Surface, SurfaceError};
