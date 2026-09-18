//! Boot-time autostart: deciding whether this boot starts Paperclip, and
//! recording durably whether it worked (§10, WWW-53).
//!
//! ADR-0008 deferred autostart because the device has nowhere durable to put
//! a systemd unit *and* keeps every Paperclip failure from surviving a
//! reboot — the reboot-always-returns-to-stock guarantee. This crate is the
//! replacement ADR-0008's WWW-53 amendment records: autostart is bought back
//! by adding a durable counter that stops trying after
//! [`counter::MAX_BOOT_ATTEMPTS`] consecutive failures, so a wedged release
//! degrades to the same stock boot ADR-0008 always guaranteed — slower (up
//! to three boots instead of one), but not lost.
//!
//! # What this crate is not
//!
//! **Not a second A/B mechanism.** `paper_updater::PlatformLayout` already
//! has one: `current`/`previous` symlinks, swapped by a single `rename(2)`
//! over a symlink (`platform/updater/src/layout.rs`, `PlatformLayout::select`).
//! This crate selects nothing and swaps nothing; it only decides *whether*
//! to start whatever `current` already names, and durably records how that
//! went. Reusing the existing swap rather than inventing a parallel one is
//! deliberate — see WWW-53's result comment for where the ticket's "A/B
//! slots" language and the pre-existing mechanism were reconciled.
//!
//! **Not a second recovery path.** `paperclip-restore-stock.service`
//! (`platform/host`, ADR-0012) is still what brings stock back when a
//! *running* session fails. This crate answers a question one layer up —
//! whether a session gets a chance to run at all this boot — and once it
//! decides to launch, everything after that point is `platform/host`'s and
//! `paper_updater::linux::SystemdSession`'s, reused rather than duplicated.
//!
//! # The split
//!
//! ```text
//!   policy      the decision itself: Launch or Skip, and why ─┐ Mac tests
//!   counter     the durable, power-loss-surviving attempt count │ prove the
//!   autostart   the disable marker `paperctl` writes over SSH  ─┘ decisions
//!
//!   units       the two persistent unit files this needs ────── generated
//!                                                                text, Mac-
//!                                                                tested
//!
//!   bin/paperclip-launcher   ties the above to a real boot ──── the VM
//!                                                                harness
//! ```
//!
//! The seam is the same one `paper_updater` uses:
//! [`paper_updater::health::SessionControl`], implemented once by
//! [`paper_updater::linux::SystemdSession`] and reused here rather than
//! reimplemented, so the boot launcher and the running-system updater cannot
//! grow two different ideas of what "stand the session down" means.

pub mod autostart;
pub mod counter;
pub mod error;
pub mod policy;
pub mod units;

pub use counter::{BootCounter, MAX_BOOT_ATTEMPTS};
pub use error::BootError;
pub use policy::{Decision, SkipReason, decide};
