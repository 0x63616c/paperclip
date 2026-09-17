//! The Paperclip host: the thing that makes a custom foreground session
//! recoverable (§10, §11).
//!
//! # What this crate is for
//!
//! Home, Chess and the App Store are all worth nothing on a tablet that can be
//! left with a blank screen. This crate is the supervisor those depend on, and
//! it is built before them on purpose.
//!
//! # The shape, and why
//!
//! ```text
//!   state      pure decisions   ── testable anywhere ──┐
//!   facilities what is enforced                        ├─ Mac tests prove the
//!   units      what gets written                       │  rules are right
//!   report     what may be claimed ───────────────────-┘
//!
//!   linux::*   signals, cgroups, systemd, sysfs ─────── VM harness proves the
//!                                                       enforcement is real
//! ```
//!
//! The split exists because those two claims are not the same claim, and the
//! project's standing rule is that mocks are never qualification. A green
//! `cargo test` on a Mac says the §10 table is decided correctly. Only
//! `tests/failure-harness`, run in an aarch64 Linux VM with systemd, says the
//! decisions are carried out.
//!
//! # The rules that outrank everything here
//!
//! * **Stock Xochitl is started and stopped, never killed, never restarted.**
//!   Its `OnFailure=` is `emergency.target remarkable-fail.service` and that
//!   unit does not exist on this image, so a *failed* Xochitl is a tablet on a
//!   serial console with nothing on the screen. [`linux::systemd::Systemd`]
//!   has no method that can do it.
//! * **Cleanup does not hang off the dying process.** Not a destructor, not a
//!   signal handler, not a shell trap. The guarantee is
//!   `paperclip-restore-stock.service`, reached by systemd's `OnFailure=`,
//!   running in a process that was not involved in the failure.
//! * **Nothing is installed on the root filesystem.** Units are written into
//!   `/run/systemd/system`, which is a tmpfs, so a reboot has never heard of
//!   Paperclip (WWW-11).
//! * **A directive that enforces nothing is not written.** See
//!   [`report::Verdict::Ineffective`].

pub mod facilities;
pub mod probe;
pub mod progress;
pub mod report;
pub mod state;
pub mod units;

#[cfg(target_os = "linux")]
pub mod linux;

pub use facilities::{CgroupLayout, Controller, Facilities, TreeTermination};
pub use progress::{MainLoopProgress, Progress, ProgressWatch};
pub use report::{IsolationReport, Mechanism, Verdict};
pub use state::{
    Action, Budget, Diagnosis, Event, ExitKind, FailurePolicy, Foreground, Machine, SessionState,
    StopReason,
};
pub use units::{SessionGrants, SessionPaths, SessionSpec, UnitFile, UnitSet};
