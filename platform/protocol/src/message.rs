//! The versioned message set: everything either side may say, and nothing else.
//!
//! Two enums, one per direction, both closed. A message that is not in
//! [`HostMessage`] or [`AppMessage`] does not exist, and the framing in
//! [`codec`](crate::codec) has no escape hatch for one.
//!
//! ## Pixels are not on this wire
//!
//! The app draws into a surface the host owns and describes in [`Hello`]; the
//! only thing it sends back is a [`FrameDone`] saying which parts changed. A
//! full 1620×2160 ARGB8888 frame is 14 MiB, and putting that through a socket
//! once per refresh would be the platform's single largest cost for no gain —
//! the host has to end up with those bytes in a mapping the waveform engine
//! can read either way.
//!
//! How the surface reaches the app process is the supervisor's business
//! (WWW-4): a file descriptor sent alongside the launch connection, mapped
//! before [`Ready`] is sent. The contract here is only its *shape*, which is
//! what both sides have to agree on.
//!
//! ## An app never says who it is
//!
//! There is no field anywhere in [`AppMessage`] carrying an app id, a version
//! or a session. All three are the host's knowledge about the connection it
//! launched, and the host attaches them itself — see
//! [`Diagnostic::tagged`], which is the only place they meet. An app that
//! wants to be a different app has to become a different process.
//!
//! ## The vocabulary grew once
//!
//! [`AppMessage::SystemQuery`] and [`HostMessage::SystemAnswer`] /
//! [`HostMessage::SystemEvent`] (WWW-50, ADR-0028) are the first addition
//! since this contract was drawn: an app may now ask the host for time,
//! battery and network facts, and the host may push a change without being
//! asked. Six messages now, not five — see [`system`](crate::system) for the
//! vocabulary and why it stops there.

use std::path::PathBuf;

use crate::capability::Capability;
use crate::geometry::{Rect, Size};
use crate::id::AppId;
use crate::input::PointerEvent;
use crate::lifecycle::{ExitReason, LaunchReason, LifecycleEvent, Request};
use crate::system::{SystemAnswer, SystemEvent, SystemQuery};
use crate::version::ProtocolVersion;

/// Identifies one run of one app, from launch to exit.
///
/// Issued by the host. Not a credential and not a secret: the connection is
/// what authenticates an app, and this is a label for correlating a crash
/// report with the frames that preceded it. It appears in diagnostics, which
/// is exactly why it must stay something safe to write to a log.
///
/// On the wire it is 32 lowercase hex characters, not a number. JSON has no
/// 128-bit integer — an encoder either refuses it or quietly rounds it through
/// a double — and a session id that survived the round trip as a *different*
/// id would misattribute every diagnostic after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(u128);

impl SessionId {
    /// Wraps a raw value.
    pub const fn new(value: u128) -> Self {
        Self(value)
    }

    /// The raw value.
    pub const fn get(self) -> u128 {
        self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

impl std::str::FromStr for SessionId {
    type Err = SessionIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 32 {
            return Err(SessionIdError);
        }
        u128::from_str_radix(s, 16)
            .map(Self)
            .map_err(|_| SessionIdError)
    }
}

/// A string that is not 32 hex characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("expected 32 lowercase hex characters")]
pub struct SessionIdError;

impl serde::Serialize for SessionId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for SessionId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// Identifies one requested frame.
///
/// Issued by the host, increasing, so a [`FrameDone`] can be matched to the
/// [`DrawRequest`] that asked for it. An app that answers a frame nobody
/// asked for, or answers the same one twice, is not confused about timing —
/// it is not following the protocol, and [`Session`](crate::Session) says so.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct FrameId(u64);

impl FrameId {
    /// The first frame of a session.
    pub const FIRST: FrameId = FrameId(0);

    /// Wraps a raw value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw value.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next frame id.
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// How pixels are laid out in the drawing surface.
///
/// One variant, because the panel has one format: 1620×2160 ARGB8888, `B, G,
/// R, 0xFF` in memory order, with no hardware alpha (WWW-20, ADR-0007). It is
/// an enum rather than an assumption so that a second format is a message
/// change an old app rejects, rather than an old app writing the wrong bytes
/// into a surface that looks the right size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum PixelFormat {
    /// 32 bits per pixel, `0xAARRGGBB` as a native-endian `u32`.
    Argb8888,
}

impl PixelFormat {
    /// Bytes per pixel.
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Argb8888 => 4,
        }
    }
}

/// The shape of the drawing surface an app was given.
///
/// **Viewport-relative.** `extent` is the app's own surface, not the panel:
/// an app draws from `(0, 0)` to `extent` and never learns where the host put
/// that on the glass, or what else is on it. Pointer coordinates arrive in the
/// same space, so a hit test is a comparison against the app's own layout with
/// no offset anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SurfaceDescriptor {
    /// The app's viewport, in pixels.
    pub extent: Size,
    /// Bytes between the start of one row and the start of the next.
    ///
    /// Carried rather than computed because a host is free to pad rows, and an
    /// app that assumed `width * 4` would draw a diagonal smear the first time
    /// one did.
    pub stride_bytes: u32,
    /// How pixels are laid out.
    pub format: PixelFormat,
}

impl SurfaceDescriptor {
    /// A tightly packed descriptor for `extent`.
    pub const fn packed(extent: Size, format: PixelFormat) -> Self {
        Self {
            extent,
            stride_bytes: extent.width * format.bytes_per_pixel(),
            format,
        }
    }

    /// Total bytes the surface occupies, or `None` if the numbers do not
    /// describe a usable buffer.
    ///
    /// `None` rather than a computed nonsense: a zero extent, or a stride too
    /// narrow for the width it claims, is a host bug, and an app that mapped
    /// the buffer anyway would write past the end of a row.
    pub fn bytes(self) -> Option<u64> {
        let minimum = u64::from(self.extent.width) * u64::from(self.format.bytes_per_pixel());
        if self.extent.is_empty() || u64::from(self.stride_bytes) < minimum {
            return None;
        }
        Some(u64::from(self.stride_bytes) * u64::from(self.extent.height))
    }
}

/// One of a client's two shared-memory buffers (WWW-52, WWW-77).
///
/// Two, not one and not a pool of arbitrary size: a client draws its next
/// frame into the slot the host is not currently reading, so drawing and
/// presenting never contend for the same bytes. Two is also the minimum that
/// makes that true — one buffer would put drawing and presenting back in the
/// same memory, and a third buys no further overlap once the compositor
/// serialises presents (WWW-52's tinywl precedent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BufferSlot {
    /// The first buffer.
    A,
    /// The second buffer.
    B,
}

impl BufferSlot {
    /// The other slot.
    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }
}

/// The shape of a client's shared-memory pool: two [`BufferSlot`]s, laid out
/// back to back, each the shape one [`SurfaceDescriptor`] already describes.
///
/// This is additive, not a replacement for [`SurfaceDescriptor`]: `Hello.surface`
/// keeps describing the single fd-mapped area WWW-4's supervisor hands an app
/// today (ADR-0022) — nothing here changes that message. This is the shape a
/// client's pool takes once it is a compositor client instead (WWW-81,
/// deliberately staged behind this ticket), described up front so
/// `platform/compositor`'s buffer lifecycle has something concrete to
/// validate against now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ShmPoolDescriptor {
    /// The layout shared by both slots.
    pub buffer: SurfaceDescriptor,
}

impl ShmPoolDescriptor {
    /// Bytes one slot occupies, or `None` if `buffer` does not describe a
    /// usable surface (see [`SurfaceDescriptor::bytes`]).
    pub fn slot_bytes(self) -> Option<u64> {
        self.buffer.bytes()
    }

    /// Total bytes the pool occupies: both slots, or `None` on the same terms
    /// as [`Self::slot_bytes`], or if doubling it would overflow.
    pub fn pool_bytes(self) -> Option<u64> {
        self.slot_bytes()?.checked_mul(2)
    }

    /// Byte offset of `slot` within the pool, or `None` on the same terms as
    /// [`Self::slot_bytes`].
    ///
    /// `A` is always first. There is no client-chosen offset: unlike a raw
    /// Wayland `wl_shm_pool`, this pool has exactly two slots and the
    /// compositor lays them out, which is what makes "offset outside the
    /// pool" a case [`Self::pool_bytes`] rules out by construction rather
    /// than one a runtime check has to catch per message.
    pub fn slot_offset(self, slot: BufferSlot) -> Option<u64> {
        let slot_bytes = self.slot_bytes()?;
        match slot {
            BufferSlot::A => Some(0),
            BufferSlot::B => Some(slot_bytes),
        }
    }
}

/// What an app may do with a shared directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ShareAccess {
    /// Read only.
    Read,
    /// Read and write.
    ReadWrite,
}

/// A directory this app shares with one other app, because the user said so.
///
/// Sharing on this platform is explicit (§4): a grant exists because a person
/// initiated an exchange, never because two apps discovered each other. The
/// grant names the other app so an app can tell one share from another; it
/// does not let it talk to that app.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SharedGrant {
    /// The other app in the exchange.
    pub with: AppId,
    /// Where the shared directory is mounted for this process.
    pub path: PathBuf,
    /// What may be done in it.
    pub access: ShareAccess,
}

/// Where this app's four storage areas are, for this process.
///
/// Absolute paths, valid only inside the launched process: the host may have
/// mounted them somewhere an app cannot construct for itself, and an app that
/// builds a path out of its own id rather than reading it from here is an app
/// that breaks the first time the host changes its layout.
///
/// The paths are a convenience, not the boundary. What actually stops an app
/// reaching another app's storage is the OS restrictions the supervisor
/// applies to the process (§11, WWW-4) — see the security note on
/// [`Hello::capabilities`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AppPaths {
    /// The package's own assets, read-only. The files the manifest declared.
    pub assets: PathBuf,
    /// Private, persistent, survives a restart. Where a save goes.
    pub private: PathBuf,
    /// Scratch. May be empty on every launch, and nothing should be surprised
    /// when it is.
    pub temp: PathBuf,
    /// Directories granted for explicit sharing, at most
    /// [`MAX_SHARED_GRANTS`](crate::MAX_SHARED_GRANTS) of them.
    #[serde(default)]
    pub shared: Vec<SharedGrant>,
}

/// The host's opening message. Always first, always exactly once.
///
/// Everything an app knows about itself and its world arrives here, which is
/// what makes the launch-time identity rule enforceable: an app does not
/// announce its id, it is *told* its id by the process that launched it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Hello {
    /// The protocol version the host speaks.
    pub protocol: ProtocolVersion,
    /// This run.
    pub session: SessionId,
    /// Who this app is, according to the host that launched it.
    pub app: AppId,
    /// The app's own SemVer, as its installed manifest declares it.
    pub version: semver::Version,
    /// Why this process exists.
    pub launch: LaunchReason,
    /// The drawing surface.
    pub surface: SurfaceDescriptor,
    /// What the host's install policy granted this app.
    ///
    /// **Informational.** This list is what the host decided, so an app can
    /// grey out a feature rather than fail at it. It is not what stops the app
    /// doing something: an app that ignores this and opens a socket anyway is
    /// stopped by the OS, not by having been told no. See `docs/app-contract.md`.
    pub capabilities: Vec<Capability>,
    /// Where this app's storage is.
    pub paths: AppPaths,
}

/// Why the host wants a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DrawReason {
    /// The first frame of the session.
    First,
    /// The app asked, with [`Request::Redraw`].
    AppRequested,
    /// The host decided — it came back from a suspend, or something was drawn
    /// over this app and has gone away.
    HostRequested,
}

/// Draw a frame.
///
/// The app draws into the surface and answers with [`FrameDone`]. Requests are
/// not queued: the host issues one at a time and coalesces every
/// [`Request::Redraw`] that arrives while one is outstanding into the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DrawRequest {
    /// Which frame this is.
    pub frame: FrameId,
    /// Why it was asked for.
    pub reason: DrawReason,
    /// The surface extent, repeated so an app that is drawing does not have to
    /// have kept [`Hello`] around.
    pub viewport: Size,
}

/// Which part of the surface an app changed.
///
/// **Advisory.** The host may present more than this claims — it keeps its own
/// copy of the previous frame and is free to diff it, because centralised
/// damage tracking is the only version of this that is right when an app is
/// wrong. Claiming [`Self::Full`] is always correct and is the initial path;
/// claiming regions is an optimisation the host may take or ignore.
///
/// What an app must never do is claim *less* than it changed. That is the one
/// way to get a stale rectangle on an e-ink panel that nothing will repaint.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", tag = "damage")]
#[non_exhaustive]
pub enum Damage {
    /// The whole surface changed. Always correct.
    Full,
    /// These rectangles changed, in viewport space. At most
    /// [`MAX_DAMAGE_RECTS`](crate::MAX_DAMAGE_RECTS).
    Regions {
        /// The changed rectangles.
        regions: Vec<Rect>,
    },
}

impl Damage {
    /// The rectangles claimed, or `None` for [`Self::Full`].
    pub fn regions(&self) -> Option<&[Rect]> {
        match self {
            Self::Full => None,
            Self::Regions { regions } => Some(regions),
        }
    }
}

/// The app has mapped its surface and is ready to be drawn.
///
/// Carries the protocol the *binary* speaks, which is not necessarily what its
/// manifest claimed: the manifest is checked at package time, but a manifest
/// is a text file next to the binary and nothing stops the two disagreeing.
/// This is the version that gets enforced, on the connection, against a binary
/// that has already started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Ready {
    /// The protocol the running binary was built against.
    pub protocol: ProtocolVersion,
}

/// The app finished drawing the frame it was asked for.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FrameDone {
    /// Which frame — the id from the [`DrawRequest`].
    pub frame: FrameId,
    /// What changed.
    pub damage: Damage,
}

/// The app finished saving after a
/// [`PrepareToExit`](LifecycleEvent::PrepareToExit).
///
/// Sending this early is the difference between the host closing the app
/// cleanly and the host waiting out the deadline for every exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Saved {
    /// The reason from the event being answered.
    pub reason: ExitReason,
    /// Whether the save succeeded.
    ///
    /// A failed save is still worth reporting: the host learns not to treat
    /// the next launch as [`LaunchReason::Restored`], and the user finds out
    /// from something other than missing work.
    pub ok: bool,
}

/// How much a diagnostic matters.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum DiagnosticLevel {
    /// Something happened that a developer would want in a trace.
    Info,
    /// Something went wrong that the app handled.
    Warn,
    /// Something went wrong that the app did not handle.
    Error,
}

/// A line an app wants in the platform log.
///
/// One string and a level. There is no structured payload, no key-value map
/// and no attachment, and that is a secrets decision rather than a
/// minimalism one: every field a diagnostic can carry is a field something
/// eventually dumps a token into, and a platform that offers only a short
/// line makes "log the whole config" awkward enough that nobody does it by
/// accident.
///
/// Bounded at [`MAX_DIAGNOSTIC_BYTES`](crate::MAX_DIAGNOSTIC_BYTES).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Diagnostic {
    /// How much it matters.
    pub level: DiagnosticLevel,
    /// What happened, in one line.
    pub message: String,
}

impl Diagnostic {
    /// Builds a diagnostic, truncating an over-long message at a character
    /// boundary.
    ///
    /// Truncating rather than refusing: a diagnostic is what an app reaches
    /// for when something has already gone wrong, and an error path that can
    /// itself fail is an error path that gets skipped.
    pub fn new(level: DiagnosticLevel, message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > crate::limits::MAX_DIAGNOSTIC_BYTES {
            let mut cut = crate::limits::MAX_DIAGNOSTIC_BYTES;
            while cut > 0 && !message.is_char_boundary(cut) {
                cut -= 1;
            }
            message.truncate(cut);
        }
        Self { level, message }
    }

    /// Attaches the identity the host knows and the app never sent.
    ///
    /// This function is the whole reason [`Diagnostic`] has no `app`,
    /// `version` or `session` field. Those three arrive here from the host's
    /// record of the connection, so a log line saying `dev.calum.chess` is
    /// evidence that the process the host launched as Chess wrote it, rather
    /// than evidence that something typed `dev.calum.chess` into a message.
    pub fn tagged(
        self,
        app: AppId,
        version: semver::Version,
        session: SessionId,
    ) -> DiagnosticRecord {
        DiagnosticRecord {
            app,
            version,
            session,
            level: self.level,
            message: self.message,
        }
    }
}

/// A diagnostic after the host has attached who said it.
///
/// Not a message: nothing sends one of these. It is what the host writes down,
/// and it exists as a type so that the tagging rule is code rather than a
/// paragraph somebody has to remember.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiagnosticRecord {
    /// Which app, according to the host.
    pub app: AppId,
    /// Which version of it, from the installed manifest.
    pub version: semver::Version,
    /// Which run.
    pub session: SessionId,
    /// How much it matters.
    pub level: DiagnosticLevel,
    /// What the app said.
    pub message: String,
}

/// Everything the host may say to an app.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", tag = "type")]
#[non_exhaustive]
pub enum HostMessage {
    /// Opening message, always first.
    Hello(Hello),
    /// Something happened to the app.
    Lifecycle(LifecycleEvent),
    /// Input.
    Pointer(PointerEvent),
    /// Draw a frame.
    Draw(DrawRequest),
    /// The answer to a [`SystemQuery`] the app sent.
    SystemAnswer(SystemAnswer),
    /// A system fact changed; no query prompted this (WWW-50).
    SystemEvent(SystemEvent),
    /// The connection is closing. Nothing follows.
    Goodbye,
}

/// Everything an app may say to the host.
///
/// Note what is still absent: no identity, no capability request, no path, no
/// "launch that". [`Self::SystemQuery`] (WWW-50) is the one addition to §8's
/// original sketch that lets an app *ask* the host something rather than only
/// answering what the host asked — see [`system`](crate::system) for why it
/// is scoped to time, battery, network and platform facts and nothing wider.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", tag = "type")]
#[non_exhaustive]
pub enum AppMessage {
    /// Surface mapped, ready to draw. Always first, always exactly once.
    Ready(Ready),
    /// A frame is drawn.
    Frame(FrameDone),
    /// One of the four things an app may ask for.
    Request(Request),
    /// Asks the host for a no-grant system fact.
    SystemQuery(SystemQuery),
    /// Saving is finished.
    Saved(Saved),
    /// A line for the log.
    Diagnostic(Diagnostic),
}

#[cfg(test)]
mod tests {
    use super::{
        AppMessage, BufferSlot, Damage, Diagnostic, DiagnosticLevel, FrameDone, FrameId,
        HostMessage, PixelFormat, SessionId, ShmPoolDescriptor, SurfaceDescriptor,
    };
    use crate::geometry::Size;
    use crate::id::AppId;
    use crate::limits::MAX_DIAGNOSTIC_BYTES;

    #[test]
    fn a_packed_surface_reports_the_bytes_it_needs() {
        let surface = SurfaceDescriptor::packed(Size::new(1620, 2160), PixelFormat::Argb8888);
        assert_eq!(surface.stride_bytes, 1620 * 4);
        assert_eq!(surface.bytes(), Some(1620 * 4 * 2160));
    }

    /// A stride narrower than the width it claims describes a buffer that
    /// cannot hold the image. Answering with a plausible number would have the
    /// app write past the end of every row.
    #[test]
    fn an_impossible_surface_has_no_size() {
        let narrow = SurfaceDescriptor {
            extent: Size::new(100, 100),
            stride_bytes: 399,
            format: PixelFormat::Argb8888,
        };
        assert_eq!(narrow.bytes(), None);
        let empty = SurfaceDescriptor::packed(Size::new(0, 100), PixelFormat::Argb8888);
        assert_eq!(empty.bytes(), None);
    }

    /// A 128-bit id has to survive the round trip exactly, or every
    /// diagnostic after it is attributed to a session that never existed.
    #[test]
    fn a_session_id_round_trips_as_hex_rather_than_as_a_number() {
        let id = SessionId::new(u128::MAX - 1);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"fffffffffffffffffffffffffffffffe\"");
        assert_eq!(serde_json::from_str::<SessionId>(&json).unwrap(), id);
        assert!(serde_json::from_str::<SessionId>("\"nothex\"").is_err());
        assert!(serde_json::from_str::<SessionId>("123").is_err());
    }

    #[test]
    fn messages_round_trip_through_their_wire_form() {
        let frame = AppMessage::Frame(FrameDone {
            frame: FrameId::new(3),
            damage: Damage::Full,
        });
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(serde_json::from_str::<AppMessage>(&json).unwrap(), frame);

        let goodbye = HostMessage::Goodbye;
        let json = serde_json::to_string(&goodbye).unwrap();
        assert_eq!(json, "{\"type\":\"goodbye\"}");
        assert_eq!(serde_json::from_str::<HostMessage>(&json).unwrap(), goodbye);
    }

    /// An error path that can fail is an error path that gets skipped, so an
    /// over-long diagnostic is cut rather than refused — and cut on a
    /// character boundary, because a log line is UTF-8.
    #[test]
    fn an_overlong_diagnostic_is_truncated_not_refused() {
        let long = "\u{e9}".repeat(MAX_DIAGNOSTIC_BYTES);
        let diagnostic = Diagnostic::new(DiagnosticLevel::Error, long);
        assert!(diagnostic.message.len() <= MAX_DIAGNOSTIC_BYTES);
        assert!(!diagnostic.message.is_empty());
        // Still valid UTF-8, i.e. it did not cut a two-byte character in half.
        assert!(diagnostic.message.chars().all(|c| c == '\u{e9}'));
    }

    /// `A` is always first, and doubling one slot's size is the whole pool —
    /// the layout `platform/compositor`'s buffer lifecycle validates against.
    #[test]
    fn a_pool_lays_out_two_slots_back_to_back() {
        let pool = ShmPoolDescriptor {
            buffer: SurfaceDescriptor::packed(Size::new(1620, 2160), PixelFormat::Argb8888),
        };
        let slot_bytes = pool.slot_bytes().unwrap();
        assert_eq!(slot_bytes, 1620 * 4 * 2160);
        assert_eq!(pool.pool_bytes(), Some(slot_bytes * 2));
        assert_eq!(pool.slot_offset(BufferSlot::A), Some(0));
        assert_eq!(pool.slot_offset(BufferSlot::B), Some(slot_bytes));
        assert_eq!(BufferSlot::A.other(), BufferSlot::B);
        assert_eq!(BufferSlot::B.other(), BufferSlot::A);
    }

    /// An impossible per-slot surface (see
    /// `an_impossible_surface_has_no_size`) has no pool shape either, rather
    /// than one computed from nonsense.
    #[test]
    fn a_pool_over_an_impossible_surface_has_no_size() {
        let pool = ShmPoolDescriptor {
            buffer: SurfaceDescriptor {
                extent: Size::new(100, 100),
                stride_bytes: 399,
                format: PixelFormat::Argb8888,
            },
        };
        assert_eq!(pool.slot_bytes(), None);
        assert_eq!(pool.pool_bytes(), None);
        assert_eq!(pool.slot_offset(BufferSlot::A), None);
    }

    /// The identity on a log line comes from the host's own record, not from
    /// anything the app sent. There is no field on `Diagnostic` to forge.
    #[test]
    fn the_host_attaches_identity_the_app_never_sent() {
        let diagnostic = Diagnostic::new(DiagnosticLevel::Warn, "board would not load");
        let json = serde_json::to_string(&diagnostic).unwrap();
        assert!(!json.contains("app"), "{json}");
        assert!(!json.contains("session"), "{json}");

        let record = diagnostic.tagged(
            "dev.calum.chess".parse::<AppId>().unwrap(),
            semver::Version::parse("0.1.0").unwrap(),
            SessionId::new(0x2b),
        );
        assert_eq!(record.app.as_str(), "dev.calum.chess");
        assert_eq!(
            record.session.to_string(),
            "0000000000000000000000000000002b"
        );
    }
}
