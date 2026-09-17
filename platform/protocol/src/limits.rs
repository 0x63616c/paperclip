//! Every bound the contract enforces, in one place.
//!
//! Single-sourced deliberately. These numbers are enforced in three different
//! processes — the app's SDK refuses to *send* something over a limit, the
//! host refuses to accept it, and the supervisor kills a process that sits on
//! a deadline — and a limit that disagreed between two of them would be a
//! protocol where a conforming app gets killed for a message the SDK told it
//! was fine.

use std::time::Duration;

/// Largest encoded message, in bytes, in either direction.
///
/// Control messages only: the largest thing on this wire is a
/// [`Hello`](crate::Hello) with its paths and grants, which is a few hundred
/// bytes. Pixels never travel through here — an app draws into a surface the
/// host gave it and sends a [`FrameDone`](crate::FrameDone) naming the damage
/// — so there is no legitimate reason for a frame to approach this, and the
/// limit is set to leave room for a long path rather than for a payload.
///
/// The length prefix is checked against this *before* anything is allocated,
/// so a hostile 4 GiB prefix costs four bytes and a rejection, not 4 GiB.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Largest diagnostic message body, in bytes.
///
/// Small on purpose. A diagnostic is a line in a log on a tablet with a few
/// gigabytes of storage; an app that wants to emit more than this is an app
/// dumping state, which is the thing most likely to carry a secret into a log
/// by accident.
pub const MAX_DIAGNOSTIC_BYTES: usize = 1024;

/// Most shared-storage grants a single app can be handed.
///
/// Sharing is explicit and user-initiated (§4), so a realistic count is zero
/// or one. The bound exists so a [`Hello`](crate::Hello) has a computable
/// maximum size rather than one that depends on install history.
pub const MAX_SHARED_GRANTS: usize = 16;

/// Most damage rectangles an app may claim in one frame.
///
/// Past a handful, unioning them costs more than presenting the bounding box,
/// and the host is free to do exactly that. The limit stops a frame message
/// from being an allocation attack dressed as precision.
pub const MAX_DAMAGE_RECTS: usize = 32;

/// How long an app has to answer [`Hello`](crate::Hello) with
/// [`Ready`](crate::Ready).
///
/// Generous for a process whose job at this point is to map a surface and say
/// so. An app that cannot manage it in two seconds is an app doing work in its
/// constructor, and the supervisor takes the launch as failed.
pub const READY_DEADLINE: Duration = Duration::from_secs(2);

/// How long an app has to answer a draw request with
/// [`FrameDone`](crate::FrameDone).
///
/// This is the UI loop's budget, and the reason no app may do long-running
/// work inside `draw`. Missing it does not kill the app — the host presents
/// what is already in the surface and carries on — but it is a diagnostic
/// worth seeing.
pub const FRAME_DEADLINE: Duration = Duration::from_millis(500);

/// The default deadline carried by
/// [`LifecycleEvent::PrepareToExit`](crate::LifecycleEvent::PrepareToExit).
///
/// The actual value travels in the message, because a shutdown has less time
/// than a switch away does. This is the one used when nothing more pressing
/// is going on.
pub const EXIT_DEADLINE: Duration = Duration::from_secs(3);

/// Shortest deadline an app will ever be given to save.
///
/// Stated so an app can size its save path against a worst case rather than
/// against the deadline it happened to see. A host that cannot afford even
/// this does not send [`PrepareToExit`](crate::LifecycleEvent::PrepareToExit)
/// at all — it kills the process, and the app finds out by being restarted.
pub const MIN_EXIT_DEADLINE: Duration = Duration::from_millis(250);
