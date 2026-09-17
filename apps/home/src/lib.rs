//! The home screen.
//!
//! A shelf of what is installed, and a short summary of what the platform
//! currently is. Shelf entries are built from real
//! [`Manifest`](paper_packages::Manifest) values rather than hard-coded
//! strings, so the name and version on a tile are the ones the app actually
//! declares — if a manifest is wrong, the shelf shows it being wrong.

mod screen;
mod shelf;

pub use screen::{HomeScreen, SystemFact, render};
pub use shelf::{ShelfEntry, ShelfGlyph, ShelfLayout};
