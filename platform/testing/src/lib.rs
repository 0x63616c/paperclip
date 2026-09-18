//! Fakes, one copy each, for every crate's tests (WWW-46).
//!
//! A dev-dependency-only crate — see the crate's `Cargo.toml` for why that
//! matters. Two modules:
//!
//! - [`sys`]: fakes for `paper_sys`'s four effects (`Clock`, `Process`,
//!   `UnitControl`, `Storage`).
//! - [`service_control`]: a fake for `paper_device::stock::ServiceControl`,
//!   which predates `paper_sys` and is domain-specific enough (it is bound to
//!   one implicit unit, `xochitl.service`, plus a wakelock and a display
//!   probe) that it was not folded into `UnitControl`.

pub mod service_control;
pub mod sys;

pub use service_control::FakeServiceControl;
pub use sys::{FakeClock, FakeProcess, FakeStorage, FakeUnitControl};
