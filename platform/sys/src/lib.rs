//! The effects spine (WWW-46).
//!
//! Four small traits, each with exactly one real adapter:
//!
//! | Trait          | Real adapter | Built on       |
//! |----------------|--------------|----------------|
//! | [`Clock`]      | [`SystemClock`]   | —          |
//! | [`Process`]    | [`SystemProcess`] | —          |
//! | [`UnitControl`]| [`Systemctl`]     | [`Process`]|
//! | [`Storage`]    | [`Filesystem`]    | —          |
//!
//! The fakes that make these testable without a tablet — `FakeClock`,
//! `FakeProcess`, `FakeUnitControl`, `FakeStorage` — live in `platform/testing`,
//! a dev-dependency-only crate. This crate never depends on it in a normal
//! (non-dev) way, so nothing that links `paper-sys` for production carries a
//! test double.
//!
//! `platform/updater` is the first adopter: its `Clock` and `SystemClock`
//! moved here unchanged (`health.rs` now re-exports them), and its 34 tests
//! kept passing without modification. `SessionControl` — the domain-specific
//! composition of a unit, a wakelock and a readiness observation — stays in
//! `platform/updater`, where it belongs; what moved is the *pattern* it
//! proved: a small trait, a real adapter, and a fake that never touches
//! systemd.

mod clock;
mod process;
mod storage;
mod unit;

pub use clock::{Clock, SystemClock};
pub use process::{Process, ProcessCommand, ProcessError, ProcessOutput, SystemProcess};
pub use storage::{Filesystem, Storage, StorageError};
pub use unit::{Systemctl, UnitControl, UnitError, wait_active};
