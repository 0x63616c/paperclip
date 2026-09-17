//! Pen and touch, read straight from evdev.
//!
//! Split three ways so the two halves that can be checked without a tablet
//! are checked without a tablet:
//!
//! - [`transform`] — digitizer coordinates to panel coordinates. Pure
//!   arithmetic, fully tested.
//! - [`evdev`] — kernel event bytes to contact events. A state machine, fully
//!   tested against synthesised frames.
//! - [`nodes`] — which device node is which. The classification is tested; the
//!   two ioctls that feed it are Linux-only and are not.

pub mod evdev;
pub mod nodes;
pub mod transform;

pub use evdev::{ContactEvent, MAX_TOUCH_SLOTS, PenDecoder, RawEvent, Tool, TouchDecoder};
pub use nodes::{
    Capabilities, InputNode, InputRole, Resolution, classify, enumerate, resolve, sole,
};
pub use transform::{PEN_EXTENT, PointerTransform, TOUCH_EXTENT};
