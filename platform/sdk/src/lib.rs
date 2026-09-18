//! What every Paperclip app is written against (§8).
//!
//! Two halves. The drawing half — one [`Canvas`], one palette, one coordinate
//! space — and the app half: the [`App`] interface, the [`Event`]s it is given
//! and the [`run`] loop that connects the two to a host.
//!
//! An app implements [`App`], calls [`run`], and never sees a message, a
//! socket or a frame id. It draws into a canvas sized to its own viewport and
//! never learns what is presenting it — on the Mac a letterboxed window
//! ([`desktop`]), on the tablet the vendor waveform engine behind
//! `paper_device`.
//!
//! ## This crate is not a security boundary
//!
//! **The SDK is a statically linked convenience layer.** It is compiled into
//! the app's own process, so every check in it is a check the app could delete
//! by not linking it. [`Storage`] refusing an ungranted directory, [`Context`]
//! having no way to set an app id, a [`Diagnostic`](paper_protocol::Diagnostic)
//! truncating itself — all of that exists so an honest app fails at the point
//! of the mistake, with a typed error naming the problem.
//!
//! What actually holds an app to the contract is on the far side of the
//! socket: the framing limits, the lifecycle rules and the OS restrictions the
//! supervisor applies, all of which assume the app is hostile and none of
//! which are in this crate. The process protocol in `paper_protocol` is the
//! runtime contract; this is the ergonomics. `docs/app-contract.md` says the
//! same thing at more length, and it is the document to read before relying on
//! anything here to stop anything.
//!
//! ## Where the types live
//!
//! Geometry, input, capabilities and the app id are defined in
//! `paper_protocol`, because they are on the wire, and re-exported here so app
//! code only needs one import. A [`PointerEvent`] an app handles is
//! byte-for-byte the one the host sent; there is no conversion layer to get
//! wrong.

mod app;
mod canvas;
mod color;
mod damage;
mod display;
mod runtime;
mod storage;
mod surface;
mod tasks;
mod text;

#[cfg(feature = "desktop")]
pub mod desktop;

pub mod chrome;

pub use app::{App, Context, Event, SaveError};
pub use canvas::Canvas;
pub use color::{Color, palette};
pub use damage::DamageAccumulator;
pub use display::{DisplayMapping, SCREEN};
pub use runtime::{Outcome, RuntimeError, run};
pub use storage::{Storage, StorageError};
pub use surface::{
    LocalSurface, LocalSurfaces, Surface, SurfaceError, SurfaceLog, SurfaceProvider,
};
pub use tasks::{Completer, Disconnected};
pub use text::{TextAlign, TextStyle, measure_text};

// The contract's own vocabulary, so an app needs one dependency rather than
// two. These are re-exports, not aliases: `paper_sdk::PointerEvent` and
// `paper_protocol::PointerEvent` are the same type.
// `MAX_DAMAGE_RECTS` is here for the same reason: an app that overrides
// [`App::damage`] has to know the cap it is claiming against, and finding it
// out would otherwise mean depending on `paper_protocol` directly.
pub use paper_protocol::{
    Action, AdminError, AdminQuery, AdminValue, AppId, BatteryFact, BatteryState, Capability,
    CatalogStatus, ContactId, Damage, DiagnosticEntry, DiagnosticLevel, ExitReason, GrantSummary,
    InstalledAppSummary, LaunchReason, MAX_DAMAGE_RECTS, NetworkFact, PixelFormat, PlatformFact,
    Point, Pointer, PointerEvent, PointerPhase, Pressure, QueryId, Rect, Size, StorageBucket,
    StorageUsage, SurfaceDescriptor, SystemAnswer, SystemDenial, SystemDenialReason, SystemEvent,
    SystemQueryKind, SystemValue, Tilt, TimeFact,
};
