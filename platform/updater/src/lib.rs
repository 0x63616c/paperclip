//! Making Paperclip itself replaceable, and replaceable safely (§13, §14).
//!
//! # Why this is not part of the host
//!
//! §13 requires an updater **independent of the host being replaced**. The
//! reason is narrow and decisive: a supervisor cannot replace its own binary
//! and then honestly report whether the replacement came back. Whatever it
//! says about the new release, it said with the old one's code, or it did not
//! survive to say anything. So the updater lives beside the host, outside
//! `releases/`, and an ordinary platform update cannot touch it.
//!
//! ```text
//!   apps                    installed from the catalog, independently versioned
//!   ─────────────────────────────────────────────────────────────────────────
//!   Paperclip               host + Home + App Store + Settings: ONE release
//!   ─────────────────────────────────────────────────────────────────────────
//!   updater + paperctl      replaces the layer above; not replaced by it
//!   ─────────────────────────────────────────────────────────────────────────
//!   reMarkable OS           A/B rootfs, SWUpdate, not ours
//! ```
//!
//! Each layer may replace the one above it and never the one below. An app
//! install cannot replace the host; a platform update cannot replace the
//! updater. Both are structural here rather than conventional — different
//! trees ([`layout::PlatformLayout::separate_from`]), and different signing
//! domains ([`Domain::PLATFORM`](paper_packages::signing::Domain::PLATFORM)).
//!
//! # The shape
//!
//! ```text
//!   layout     where releases live, and the two symlinks ──┐
//!   manifest   what a signed platform release declares     │ Mac tests prove
//!   journal    what survives a power cut                   ├ the transaction
//!   health     what counts as a release having come up     │ is right
//!   upgrade    the transaction itself ────────────────────-┘
//!
//!   linux      systemd, sysfs, the status file ──────────── the VM harness
//!                                                           proves it is real
//! ```
//!
//! The seam is [`health::SessionControl`]. A green `cargo test` on a Mac says
//! the transaction rolls back when a candidate stalls. Only
//! `tests/failure-harness`, in a Linux VM with systemd, says a candidate that
//! stalls is noticed. Nothing here lets the first be mistaken for the second.
//!
//! # The word "updater"
//!
//! Internal. The user-facing verb is `paperctl upgrade`, and later a button
//! that says "Update Paperclip". Nothing in a screen or a user-facing document
//! should name this crate.

#[cfg(feature = "publishing")]
pub mod bundle;
pub mod error;
pub mod health;
pub mod journal;
pub mod layout;
pub mod manifest;
pub mod remove;
pub mod upgrade;

#[cfg(target_os = "linux")]
pub mod linux;

pub use error::{Unhealthy, UpdateError};
pub use health::{Budget, Clock, HealthReport, Observation, SessionControl, SystemClock};
pub use journal::{Journal, Phase, Record};
pub use layout::PlatformLayout;
pub use manifest::{Component, ComponentPolicy, PlatformManifest, VerifiedPlatform};
pub use upgrade::{Maintenance, Outcome, Reconciled, Status, Upgrade};
