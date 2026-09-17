//! The parts that only mean anything on Linux.
//!
//! Compiled only for `target_os = "linux"`, and that is a deliberate refusal
//! rather than a build convenience. A macOS build of this module would compile
//! and run and prove nothing, and the project's standing rule is that passing
//! local tests is not qualification. If it is not here, it cannot be
//! accidentally cited as evidence.

pub mod process;
pub mod recovery;
pub mod runtime;
pub mod systemd;
pub mod unit;
