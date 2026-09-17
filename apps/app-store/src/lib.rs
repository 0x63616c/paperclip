//! The App Store (§5, §6).
//!
//! Shows what is installed, what the catalog offers, both versions, release
//! notes, install progress and errors that say what to do about themselves.
//! Installing and updating are explicit; nothing here installs or launches
//! anything on its own (§12).
//!
//! Four pieces, split so that the interesting one needs no device:
//!
//! - [`screen`] is the state, and it is pure. Every press is a transition over
//!   a snapshot, so what the App Store *does* is testable without a catalog, a
//!   store or a panel.
//! - [`render`] draws that state and hands back the rectangles it used, so a
//!   press is hit-tested against what is actually on the glass.
//! - [`source`] is the seam to the platform. The App Store owns no package
//!   machinery: it holds a [`StoreSource`], and the only way to build the real
//!   one is to prove host policy granted this app `packages`.
//! - [`app`] is the wire adapter: [`AppStoreApp`] drives `screen` and `render`
//!   over a `Context`, and hands every blocking `StoreSource` call to a
//!   worker thread rather than the event loop (§8).

mod app;
mod render;
mod screen;
mod source;

pub use app::{AppStoreApp, Completion};
pub use render::{DetailLayout, ListLayout, RowLayout, StoreLayout, render};
pub use screen::{AppStoreScreen, Failure, Outcome, Request, View, Working};
pub use source::{PackagesSource, SourceError, StoreSource};
