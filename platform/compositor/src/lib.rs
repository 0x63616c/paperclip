//! The compositor: shared-memory client pools, the pending/current buffer
//! lifecycle that keeps a commit atomic, the socket accept loop, and
//! crash/hang isolation (WWW-52, WWW-77, WWW-78).
//!
//! Four pieces:
//!
//! - [`pool`] is the only place that touches real memory directly: it owns
//!   an `mmap`ed `MAP_SHARED` region and hands out bounds-checked slices
//!   into its two [`BufferSlot`](paper_protocol::BufferSlot)s.
//! - [`surface`] is pure state: attach, damage and commit rules, validated
//!   against a [`Size`](paper_protocol::Size) and a slot's release state,
//!   with no `unsafe` and no I/O anywhere in it.
//! - [`wire`] and [`fdpass`] are the client protocol: a small message set
//!   distinct from [`paper_protocol::HostMessage`]/[`paper_protocol::AppMessage`]
//!   (ADR-0033 left that decision open; this crate makes it), and the
//!   `SCM_RIGHTS` plumbing a client's pool fd arrives through.
//! - [`server`] is the accept loop: [`server::Compositor`] owns the panel
//!   exclusively, is non-blocking on every fd it ever touches, and tears
//!   down exactly the client that dies or hangs — see its own module doc
//!   for how.
//!
//! A malformed damage rect is rejected by [`surface`] alone, before [`pool`]
//! is ever asked to hand out a slice for it — see
//! `surface::tests::an_out_of_bounds_rect_never_reaches_the_pool` for the
//! test that pins this ordering down.
//!
//! [`gesture`] is a third, independent piece: a pure detector that turns a
//! stream of pointer events into a per-event app-vs-system [`gesture::
//! Verdict`], with no dependency on `pool` or `surface` and none on a clock
//! or an event loop (WWW-52, WWW-80).
//!
//! [`chrome`] is a fourth, independent piece: the compositor's own status
//! bar, drawn outside any client's surface and composed on top of one
//! (WWW-79). It does not depend on `pool`, `surface` or `gesture` — see its
//! module doc for what still has to drive it.
//!
//! [`sleep`] is a fifth, independent piece: the sleep cover and a basic lock
//! screen, drawn above everything else — client surfaces and chrome both
//! (WWW-82). [`server::Compositor::sleep`]/[`server::Compositor::wake`] are
//! the seam that drives it; see `sleep`'s module doc for what still has to
//! drive *that*.
//!
//! ## What this crate does not do yet
//!
//! [`server::Compositor`] is a library, not a running system service: it has
//! no `[[bin]]`, no well-known socket path, and no wiring to the vendor
//! panel or to a real client. Moving a real client (Home, the test card,
//! ...) onto this wire, and turning this into something the supervisor
//! starts at boot, are later work (WWW-81 and beyond) — this ticket proves
//! the accept loop and crash/hang isolation against `MemoryPanel` and real
//! Unix-socket peers, not against a boot-time service.
//!
//! [`gesture`]'s own doc says it "is wired into that same event loop by
//! WWW-78 too" — it is not, yet: [`server::Compositor`]'s wire
//! ([`wire::ClientRequest`]) carries `Attach`/`Commit` only, nothing about
//! pointer input, so [`gesture::GestureDetector`] has no event loop feeding
//! it here. That wiring, and [`chrome`]'s composition into a presented
//! frame, are left for whichever ticket moves a real client (with real
//! pointer events) onto this wire.

pub mod chrome;
pub mod client;
pub mod fdpass;
pub mod gesture;
pub mod pool;
pub mod present;
pub mod server;
pub mod sleep;
pub mod stopped;
pub mod surface;
pub mod wire;

pub use chrome::{ChromeState, content_rect, draw as draw_chrome, reserved_rect};
pub use client::{
    CompositorSurface, CompositorSurfaces, ConnectError, DEFAULT_SOCKET, SOCKET_ENV, socket_path,
};
pub use gesture::{Edge, GestureDetector, SystemGesture, Verdict};
pub use pool::{Pool, PoolError};
pub use present::present_pool_slot;
pub use server::{BindError, ClientId, Compositor, CompositorEvent, HELLO_DEADLINE};
pub use sleep::render_lock_frame;
pub use stopped::render_stopped_frame;
pub use surface::{Committed, Surface, SurfaceError};
pub use wire::{ClientHello, ClientRequest, ClientRole, HostEvent};
