//! What an app is: three methods, one event type, and everything the platform
//! is willing to tell it (§8).
//!
//! ```no_run
//! use paper_sdk::{Action, App, Canvas, Context, Event, SaveError, palette};
//!
//! #[derive(Default)]
//! struct Counter {
//!     taps: u32,
//! }
//!
//! impl App for Counter {
//!     // This app does no background work, so nothing ever completes.
//!     type Completion = std::convert::Infallible;
//!
//!     fn event(&mut self, event: &Event<Self::Completion>, _: &mut Context<'_, Self::Completion>) -> Action {
//!         match event {
//!             Event::Pointer(pointer) if pointer.is_tap() => {
//!                 self.taps += 1;
//!                 Action::Redraw
//!             }
//!             _ => Action::None,
//!         }
//!     }
//!
//!     fn draw(&mut self, canvas: &mut Canvas, _: &mut Context<'_, Self::Completion>) {
//!         canvas.clear(palette::PAPER);
//!     }
//!
//!     fn save(&mut self, context: &mut Context<'_, Self::Completion>) -> Result<(), SaveError> {
//!         context.storage().write_private("taps", &self.taps.to_le_bytes())?;
//!         Ok(())
//!     }
//! }
//! ```
//!
//! That is the whole interface. Everything else an app can reach — its id, its
//! viewport, its storage, its diagnostics, a thread — is on [`Context`], and
//! everything the platform can tell it is a variant of [`Event`].

use std::fmt;

use paper_protocol::{
    Action, AppId, Capability, Diagnostic, DiagnosticLevel, ExitReason, LaunchReason, PointerEvent,
    QueryId, SessionId, Size, SurfaceDescriptor, SystemAnswer, SystemEvent, SystemQuery,
    SystemQueryKind,
};

use crate::canvas::Canvas;
use crate::storage::{Storage, StorageError};
use crate::tasks::Completer;

/// Something the platform is telling the app.
///
/// Generic over the app's own completion type: a value a worker thread sent
/// back arrives here in the same queue as input, so an app's state is only
/// ever touched on one thread.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Event<C> {
    /// Input, already in viewport space.
    Pointer(PointerEvent),

    /// The app is going into the background, or the device is suspending.
    ///
    /// **Best effort.** The tablet can suspend between any two instructions,
    /// so this may simply not arrive. Use it to stop a timer, never to save:
    /// saving happens on [`Self::PrepareToExit`], which is the one the
    /// platform guarantees a budget for.
    Suspended,

    /// Foreground again. A draw request follows when the panel is presentable;
    /// drawing on this event instead would draw into a surface nobody is
    /// scanning out yet.
    Resumed,

    /// Save now, and expect to be killed when the deadline passes.
    ///
    /// The SDK calls [`App::save`] immediately after this event returns, so an
    /// app that does its saving there does not have to handle this at all.
    /// Handle it to *stop* things: cancel a download, drop a lock, flush a
    /// log. Whatever this returns is ignored — the app is leaving.
    PrepareToExit {
        /// Why.
        reason: ExitReason,
        /// How long there is.
        deadline: std::time::Duration,
    },

    /// A worker thread finished.
    Completed(C),

    /// The answer to a [`Context::query_system`] call (WWW-50).
    ///
    /// May arrive several callbacks after the query was sent — the host
    /// answers when it can, not necessarily before the next frame — so an app
    /// that cares about the answer keeps [`SystemAnswer::id`] around to match
    /// it against the query it sent.
    System(SystemAnswer),

    /// A system fact changed without being asked about (WWW-50).
    ///
    /// Push, not poll: an app that wants to show battery or network state
    /// does not have to query on a timer to stay current.
    SystemChanged(SystemEvent),
}

/// Why an app could not save.
///
/// A string, on purpose: this value's only destination is a diagnostic and the
/// `ok: false` on a [`Saved`](paper_protocol::Saved), and a platform that
/// asked every app to model its save failures in a shared enum would get one
/// variant per app and a `Other(String)` anyway.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SaveError {
    message: String,
}

impl SaveError {
    /// Builds one from anything printable.
    pub fn new(message: impl fmt::Display) -> Self {
        Self {
            message: message.to_string(),
        }
    }

    /// What went wrong.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<StorageError> for SaveError {
    fn from(error: StorageError) -> Self {
        Self::new(error)
    }
}

impl From<std::io::Error> for SaveError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error)
    }
}

/// Everything an app knows about itself and its world.
///
/// All of it arrives from the host in [`Hello`](paper_protocol::Hello). None
/// of it is asserted by the app, which is the launch-time identity rule made
/// concrete: there is no setter here and no message that carries one.
#[derive(Debug)]
pub struct Context<'a, C> {
    session: SessionId,
    app: &'a AppId,
    version: &'a semver::Version,
    launch: LaunchReason,
    surface: SurfaceDescriptor,
    capabilities: &'a [Capability],
    storage: &'a Storage,
    completer: &'a Completer<C>,
    outbox: &'a mut Vec<Diagnostic>,
    next_query: &'a mut QueryId,
    queries: &'a mut Vec<SystemQuery>,
}

impl<'a, C: Send + 'static> Context<'a, C> {
    /// Builds a context for one callback.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        session: SessionId,
        app: &'a AppId,
        version: &'a semver::Version,
        launch: LaunchReason,
        surface: SurfaceDescriptor,
        capabilities: &'a [Capability],
        storage: &'a Storage,
        completer: &'a Completer<C>,
        outbox: &'a mut Vec<Diagnostic>,
        next_query: &'a mut QueryId,
        queries: &'a mut Vec<SystemQuery>,
    ) -> Self {
        Self {
            session,
            app,
            version,
            launch,
            surface,
            capabilities,
            storage,
            completer,
            outbox,
            next_query,
            queries,
        }
    }

    /// Which run this is. Appears in every diagnostic this app emits.
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Who this app is, according to the host that launched it.
    pub fn app(&self) -> &AppId {
        self.app
    }

    /// This app's own SemVer, from its installed manifest.
    ///
    /// Independent of the protocol version: a 3.0.0 app and a 0.1.0 app can
    /// both speak protocol `1.0`, and a protocol bump is not an app release.
    pub fn version(&self) -> &semver::Version {
        self.version
    }

    /// Why this process exists — in particular, whether the last one crashed.
    pub fn launch(&self) -> LaunchReason {
        self.launch
    }

    /// The drawing surface's extent. The app's own space, not the panel's.
    pub fn viewport(&self) -> Size {
        self.surface.extent
    }

    /// The drawing surface this session actually granted: extent, stride and
    /// pixel format, exactly as [`Hello`](paper_protocol::Hello) carried them.
    ///
    /// Not a compiled constant: whichever host built this session's `Hello`
    /// decided these numbers, and a host that queried the real panel (rather
    /// than assuming a tightly packed one) is the only way this differs from
    /// [`SurfaceDescriptor::packed`].
    pub fn surface(&self) -> SurfaceDescriptor {
        self.surface
    }

    /// What the host granted.
    ///
    /// For deciding what to *offer*, not for deciding what is allowed: the OS
    /// decides that. An App Store without `network` should grey out its
    /// catalog rather than discover the answer by failing to connect.
    pub fn capabilities(&self) -> &[Capability] {
        self.capabilities
    }

    /// Whether the host granted `capability`.
    pub fn holds(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// Assets, private storage, scratch and shares.
    pub fn storage(&self) -> &Storage {
        self.storage
    }

    /// A handle for reporting from a worker thread.
    pub fn completer(&self) -> Completer<C> {
        self.completer.clone()
    }

    /// Runs `work` on a thread; its result arrives as
    /// [`Event::Completed`].
    pub fn spawn<F>(&self, work: F)
    where
        F: FnOnce() -> C + Send + 'static,
    {
        self.completer.spawn(work);
    }

    /// Adds a line to the platform log.
    ///
    /// The app's id, version and session are attached by the *host*, from its
    /// own record of this connection — there is no field here to put them in.
    /// A message longer than
    /// [`MAX_DIAGNOSTIC_BYTES`](paper_protocol::MAX_DIAGNOSTIC_BYTES) is
    /// truncated rather than dropped.
    ///
    /// Nothing scans the text for secrets, because nothing can. What the
    /// platform does instead is offer one short line and no structured
    /// payload, so "log the whole config" is awkward enough that it does not
    /// happen by accident.
    pub fn log(&mut self, level: DiagnosticLevel, message: impl Into<String>) {
        self.outbox.push(Diagnostic::new(level, message));
    }

    /// [`Self::log`] at info.
    pub fn info(&mut self, message: impl Into<String>) {
        self.log(DiagnosticLevel::Info, message);
    }

    /// [`Self::log`] at warn.
    pub fn warn(&mut self, message: impl Into<String>) {
        self.log(DiagnosticLevel::Warn, message);
    }

    /// [`Self::log`] at error.
    pub fn error(&mut self, message: impl Into<String>) {
        self.log(DiagnosticLevel::Error, message);
    }

    /// Asks the host for a no-grant system fact — time, battery, network or
    /// platform (§ project description, WWW-50, ADR-0028).
    ///
    /// Returns the id the answer will carry on [`Event::System`], which may
    /// arrive on a later callback: this call only sends the question. No
    /// grant is required or checked, and the host may still refuse with a
    /// [`SystemDenial`](paper_protocol::SystemDenial) — a backend can be
    /// unavailable, or this app can simply be asking too often.
    pub fn query_system(&mut self, kind: SystemQueryKind) -> QueryId {
        let id = *self.next_query;
        *self.next_query = id.next();
        self.queries.push(SystemQuery { id, kind });
        id
    }
}

/// A Paperclip app.
///
/// Three methods, and the split between them is the platform's one hard rule
/// about time:
///
/// - [`event`](Self::event) decides. It runs on the UI loop, so it must not
///   block — hand anything slow to [`Context::spawn`].
/// - [`draw`](Self::draw) renders state that already exists. **No network, no
///   filesystem, no installation work, no waiting.** By the time `draw` is
///   called, everything it needs must already be in `self`.
/// - [`save`](Self::save) persists, once, against a deadline the app was told.
///
/// State drives rendering. An app never presents a frame; it changes its own
/// state and returns [`Action::Redraw`], and the host decides when to ask.
pub trait App {
    /// What this app's background work produces.
    ///
    /// `std::convert::Infallible` for an app that has none — it makes
    /// [`Event::Completed`] unconstructible, so the match arm is provably
    /// dead rather than merely unused.
    type Completion: Send + 'static;

    /// Handles one event and says what should happen next.
    fn event(
        &mut self,
        event: &Event<Self::Completion>,
        context: &mut Context<'_, Self::Completion>,
    ) -> Action;

    /// Draws the current state into the viewport.
    ///
    /// Called only when the host asks. The canvas is the app's own viewport,
    /// origin top-left, already the right size.
    fn draw(&mut self, canvas: &mut Canvas, context: &mut Context<'_, Self::Completion>);

    /// Writes whatever must survive this process.
    ///
    /// Called after a [`Event::PrepareToExit`], inside the deadline that event
    /// carried. An `Err` is reported to the host and logged; it does not stop
    /// the app exiting, because nothing can.
    fn save(&mut self, context: &mut Context<'_, Self::Completion>) -> Result<(), SaveError>;

    /// What the app claims changed since the last frame.
    ///
    /// Defaults to the whole viewport, which is always correct and is the
    /// initial path (§8). Overriding it is an optimisation the host may take
    /// or ignore — it keeps the previous frame and is free to work the damage
    /// out itself, because centralised damage tracking is the only version of
    /// this that stays right when an app gets it wrong.
    ///
    /// Claiming *less* than changed is the one genuinely harmful answer: on
    /// e-ink a stale rectangle stays on the glass until something else
    /// repaints it.
    fn damage(&self) -> paper_protocol::Damage {
        paper_protocol::Damage::Full
    }
}
