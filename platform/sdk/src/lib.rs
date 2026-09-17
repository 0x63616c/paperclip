//! What every Paperclip screen is drawn with.
//!
//! One canvas, one palette, one coordinate space. Apps draw into a
//! [`Canvas`] sized [`SCREEN`] and never learn what is presenting it — on the
//! Mac that is a letterboxed window ([`desktop`]), on the tablet it will be
//! whatever WWW-3 establishes.
//!
//! Stage 1 scope, stated so nobody mistakes this for more than it is: the
//! desktop backend here proves coordinate handling and legibility on a Mac.
//! It proves nothing about e-ink refresh, pen pressure, palm rejection,
//! latency or colour. Those are device facts and belong to WWW-1 and WWW-3.

mod canvas;
mod color;
mod display;
mod geometry;
mod input;
mod text;

#[cfg(feature = "desktop")]
pub mod desktop;

pub mod chrome;

pub use canvas::Canvas;
pub use color::{Color, palette};
pub use display::{DisplayMapping, SCREEN};
pub use geometry::{Point, Rect, Size};
pub use input::{Pointer, PointerEvent, PointerPhase};
pub use text::{TextAlign, TextStyle, measure_text};
