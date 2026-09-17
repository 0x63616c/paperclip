//! Pen and touch, read straight from evdev.
//!
//! Split three ways so the two halves that can be checked without a tablet
//! are checked without a tablet:
//!
//! - [`transform`] — digitizer coordinates to panel coordinates. Pure
//!   arithmetic, fully tested.
//! - [`evdev`] — kernel event bytes to [`PointerEvent`](paper_sdk::PointerEvent)s.
//!   A state machine, fully tested against synthesised frames. It emits the
//!   SDK's own event type: WWW-5 widened that type to carry everything this
//!   hardware reports, so there is no device-specific event struct left to
//!   translate out of.
//! - [`nodes`] — which device node is which. The classification is tested; the
//!   two ioctls that feed it are Linux-only and are not.
//! - [`reader`] — bytes off the node onto a channel of
//!   [`PointerEvent`](paper_sdk::PointerEvent)s. The carry-across-reads logic
//!   is tested against a pipe; opening `/dev/input/eventN` is Linux-only.

pub mod evdev;
pub mod nodes;
pub mod reader;
pub mod transform;

pub use evdev::{ContactIds, MAX_TOUCH_SLOTS, PenDecoder, RawEvent, TouchDecoder};
pub use nodes::{
    Capabilities, InputNode, InputRole, Resolution, classify, enumerate, resolve, sole,
};
pub use reader::{absorb, drive};
pub use transform::{PEN_EXTENT, PointerTransform, TOUCH_EXTENT};

#[cfg(target_os = "linux")]
pub use reader::open_node;
