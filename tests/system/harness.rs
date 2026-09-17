//! Marker crate for the system test suite.
//!
//! The tests live in `cases/`, registered as an explicit `[[test]]` target.
//! This file exists because a Cargo package needs a target of its own; there
//! is deliberately nothing in it, and helpers belong next to the cases that
//! use them rather than here where every case would have to link them.
