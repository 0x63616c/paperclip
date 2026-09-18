//! The contract between the Paperclip host and the apps it runs (§8).
//!
//! This crate is the whole app-facing contract in one place: the vocabulary
//! both sides speak ([`AppId`], [`Capability`], [`Point`], [`PointerEvent`]),
//! the message set they exchange ([`HostMessage`], [`AppMessage`]), the
//! framing that carries it ([`codec`]), the rules about what may be said when
//! ([`Session`]), and every limit any of that is bounded by ([`limits`]).
//!
//! ## This crate is the contract; `paper_sdk` is a convenience
//!
//! An app links `paper_sdk` because writing a frame loop by hand is tedious,
//! not because the SDK is what holds it to anything. **The SDK is a statically
//! linked library inside the app's own process, so it is not a security
//! boundary and cannot be one.** Every rule that actually binds an app is
//! enforced on the far side of the socket, by a host that assumes the app is
//! hostile: the framing limits in [`codec`], the lifecycle rules in
//! [`Session`], and the OS restrictions the supervisor applies to the process
//! (WWW-4).
//!
//! That is why the enforcing code lives here rather than in the SDK, and why
//! the tests for it drive raw bytes rather than SDK calls. See
//! `docs/app-contract.md`.
//!
//! ## The four layers
//!
//! | Layer | Enforced by | Catches |
//! |---|---|---|
//! | Compile-time | `paper_sdk`'s types | An app that does not implement the interface |
//! | Package-time | `paperctl check`, `paper_packages` | A package that could not run if it were installed |
//! | Launch-time | [`Session`] and [`codec`], in the host | A binary that starts and then misbehaves |
//! | Runtime | The supervisor's OS restrictions and deadlines | An app that ignores all of the above |
//!
//! Each layer assumes the one before it was skipped.
//!
//! ## Versioning
//!
//! [`CURRENT`] is what this build speaks. An app's SemVer and the protocol
//! version are independent: Chess 3.0.0 and Chess 0.1.0 may both speak
//! protocol `1.0`, and a protocol bump is not an app release. Compatibility is
//! one-directional — [`ProtocolVersion::can_run`] — and an unsupported version
//! is refused explicitly at both package time and launch time rather than
//! being negotiated down.

pub mod capability;
pub mod codec;
pub mod geometry;
pub mod id;
pub mod input;
pub mod lifecycle;
pub mod limits;
pub mod message;
pub mod path;
pub mod session;
pub mod system;
pub mod version;

pub use capability::Capability;
pub use codec::CodecError;
pub use geometry::{Point, Rect, Size};
pub use id::{AppId, IdError};
pub use input::{
    ContactId, PEN_PRESSURE_FULL_SCALE, PEN_TILT_FULL_SCALE, Pointer, PointerEvent, PointerPhase,
    Pressure, Tilt,
};
pub use lifecycle::{Action, ExitReason, LaunchReason, LifecycleEvent, Request};
pub use limits::{
    EXIT_DEADLINE, FRAME_DEADLINE, MAX_DAMAGE_RECTS, MAX_DIAGNOSTIC_BYTES, MAX_MESSAGE_BYTES,
    MAX_SHARED_GRANTS, MAX_SYSTEM_QUERIES_PER_SECOND, MIN_EXIT_DEADLINE, READY_DEADLINE,
};
pub use message::{
    AppMessage, AppPaths, Damage, Diagnostic, DiagnosticLevel, DiagnosticRecord, DrawReason,
    DrawRequest, FrameDone, FrameId, Hello, HostMessage, PixelFormat, Ready, Saved, SessionId,
    SessionIdError, ShareAccess, SharedGrant, SurfaceDescriptor,
};
pub use path::{PathError, RelativePath};
pub use session::{HostFault, Session, State, Violation};
pub use system::{
    BatteryFact, BatteryState, NetworkFact, PlatformFact, QueryId, SystemAnswer, SystemDenial,
    SystemDenialReason, SystemEvent, SystemQuery, SystemQueryKind, SystemValue, TimeFact,
};
pub use version::{CURRENT, ParseError, ProtocolVersion};
