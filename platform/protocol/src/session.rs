//! The launch-time layer: which messages are legal, in which order.
//!
//! [`Session`] is the host's side of one app connection. It is both the
//! validator for everything the app says and the *factory* for everything the
//! host says, which is deliberate: a host message that would be illegal at
//! this point in the conversation cannot be constructed, so the rules are
//! enforced in the same place for both directions and cannot drift apart.
//!
//! What this type does not do is measure time. [`READY_DEADLINE`] and the
//! prepare-to-exit budget are values it hands out and records; the thing that
//! notices they passed is the supervisor (WWW-4), because only the supervisor
//! can do anything about it. A state machine that also owned a clock would be
//! a state machine that could not be tested without one.
//!
//! An app is never trusted to run this. The SDK runs a much smaller check of
//! its own, and an app that skips the SDK entirely still meets this one,
//! because this one lives in the host process.

use std::time::Duration;

use crate::geometry::Size;
use crate::lifecycle::{ExitReason, LifecycleEvent};
use crate::limits::{EXIT_DEADLINE, MAX_DAMAGE_RECTS, MAX_DIAGNOSTIC_BYTES, READY_DEADLINE};
use crate::message::{AppMessage, DrawReason, DrawRequest, FrameId, HostMessage, SessionId};
use crate::version::ProtocolVersion;

/// Where a connection is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum State {
    /// `Hello` has been sent. Nothing but `Ready` may arrive.
    Greeting,
    /// The app is ready and foreground.
    Running,
    /// The app is ready and suspended. It may still speak; it will not be
    /// asked to draw.
    Suspended,
    /// `PrepareToExit` has been sent. Only `Saved` and `Diagnostic` are left.
    Exiting,
    /// Over.
    Closed,
}

/// A message that broke the contract.
///
/// Every variant is a launch-time or runtime kill condition, not a warning.
/// The host's response is to end the session — an app that has already
/// misused the protocol is an app whose next message means nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Violation {
    /// The app spoke before saying it was ready.
    #[error("app sent `{message}` before `ready`")]
    BeforeReady {
        /// Which message.
        message: &'static str,
    },

    /// The app said it was ready twice.
    #[error("app sent `ready` more than once")]
    AlreadyReady,

    /// The binary speaks a protocol this host cannot honour.
    ///
    /// Checked here as well as at package time, because the manifest is a text
    /// file next to the binary and nothing makes them agree. This is the check
    /// that runs against the process that actually started.
    #[error("app speaks protocol {app}, which this host ({host}) cannot run")]
    UnsupportedProtocol {
        /// What the binary said.
        app: ProtocolVersion,
        /// What the host speaks.
        host: ProtocolVersion,
    },

    /// The app answered a frame the host never asked for.
    #[error("app answered frame {frame}, which was never requested")]
    UnknownFrame {
        /// The id it claimed.
        frame: u64,
    },

    /// The app answered a frame while none was outstanding — usually the same
    /// frame answered twice.
    #[error("app sent `frame` with no draw request outstanding")]
    NoFrameOutstanding,

    /// The app claimed more damage rectangles than the limit allows.
    #[error("app claimed {count} damage rectangles, over the limit of {max}")]
    TooMuchDamage {
        /// How many it claimed.
        count: usize,
        /// The limit.
        max: usize,
    },

    /// The app sent a diagnostic over the size limit.
    ///
    /// The SDK truncates rather than sending one of these, so reaching this
    /// means the app bypassed the SDK.
    #[error("app sent a {len} byte diagnostic, over the {max} byte limit")]
    DiagnosticTooLong {
        /// Its length.
        len: usize,
        /// The limit.
        max: usize,
    },

    /// The app reported a save it was never asked for.
    #[error("app sent `saved` without a `prepare-to-exit`")]
    SavedWithoutExit,

    /// The app answered a prepare-to-exit with a different reason than it was
    /// given.
    #[error("app answered `prepare-to-exit` for `{expected}` with `{actual}`")]
    SavedWrongReason {
        /// What it was asked about.
        expected: ExitReason,
        /// What it answered about.
        actual: ExitReason,
    },

    /// The app kept talking after being asked to exit.
    #[error("app sent `{message}` after `prepare-to-exit`")]
    AfterExit {
        /// Which message.
        message: &'static str,
    },

    /// The app spoke after the session closed.
    #[error("app sent `{message}` after the session closed")]
    AfterClose {
        /// Which message.
        message: &'static str,
    },
}

/// Why a host action was not available.
///
/// Distinct from [`Violation`]: a violation is the app's fault and ends the
/// session, this is the host's own bug and is caught before a message exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HostFault {
    /// Asked for a frame before the app was ready, or after it was told to exit.
    #[error("cannot request a frame while the session is {state:?}")]
    WrongState {
        /// Where the session actually is.
        state: State,
    },
    /// Asked for a frame while one was already outstanding.
    #[error("frame {frame} is already outstanding")]
    FrameOutstanding {
        /// The frame still being drawn.
        frame: u64,
    },
}

/// One app connection, from the host's side.
#[derive(Debug)]
pub struct Session {
    id: SessionId,
    host_protocol: ProtocolVersion,
    app_protocol: Option<ProtocolVersion>,
    viewport: Size,
    state: State,
    next_frame: FrameId,
    outstanding: Option<FrameId>,
    exiting: Option<ExitReason>,
}

impl Session {
    /// Opens a session that has just had its `Hello` sent.
    ///
    /// There is no state before this one. A connection with no `Hello` on it
    /// is not a session, it is a socket.
    pub fn greeting(id: SessionId, host_protocol: ProtocolVersion, viewport: Size) -> Self {
        Self {
            id,
            host_protocol,
            app_protocol: None,
            viewport,
            state: State::Greeting,
            next_frame: FrameId::FIRST,
            outstanding: None,
            exiting: None,
        }
    }

    /// Which session this is.
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// Where the conversation is.
    pub fn state(&self) -> State {
        self.state
    }

    /// The protocol the app's binary reported, once it has.
    pub fn app_protocol(&self) -> Option<ProtocolVersion> {
        self.app_protocol
    }

    /// How long the app has to answer `Hello` with `Ready`.
    pub fn ready_deadline(&self) -> Duration {
        READY_DEADLINE
    }

    /// The frame the app is currently drawing, if any.
    pub fn outstanding_frame(&self) -> Option<FrameId> {
        self.outstanding
    }

    /// Validates one message from the app and folds it into the session.
    ///
    /// An `Err` ends the session: the state moves to [`State::Closed`], so a
    /// host that keeps reading gets [`Violation::AfterClose`] rather than a
    /// second, differently-worded version of the same problem.
    pub fn on_app_message(&mut self, message: &AppMessage) -> Result<(), Violation> {
        let result = self.check_app_message(message);
        if result.is_err() {
            self.state = State::Closed;
        }
        result
    }

    fn check_app_message(&mut self, message: &AppMessage) -> Result<(), Violation> {
        let name = app_message_name(message);

        if self.state == State::Closed {
            return Err(Violation::AfterClose { message: name });
        }

        // Diagnostics are legal in every state an app can still speak in, so
        // the size check comes first and applies everywhere.
        if let AppMessage::Diagnostic(diagnostic) = message {
            if diagnostic.message.len() > MAX_DIAGNOSTIC_BYTES {
                return Err(Violation::DiagnosticTooLong {
                    len: diagnostic.message.len(),
                    max: MAX_DIAGNOSTIC_BYTES,
                });
            }
            // Legal in every state an app can still speak in, including
            // before readiness: an app that cannot start should be able to say
            // why, and the alternative is a silent launch failure.
            return Ok(());
        }

        if self.state == State::Greeting {
            let AppMessage::Ready(ready) = message else {
                return Err(Violation::BeforeReady { message: name });
            };
            if !self.host_protocol.can_run(ready.protocol) {
                return Err(Violation::UnsupportedProtocol {
                    app: ready.protocol,
                    host: self.host_protocol,
                });
            }
            self.app_protocol = Some(ready.protocol);
            self.state = State::Running;
            return Ok(());
        }

        if self.state == State::Exiting {
            return match message {
                AppMessage::Saved(saved) => {
                    let expected = self.exiting.expect("Exiting implies a reason");
                    if saved.reason != expected {
                        return Err(Violation::SavedWrongReason {
                            expected,
                            actual: saved.reason,
                        });
                    }
                    self.state = State::Closed;
                    Ok(())
                }
                _ => Err(Violation::AfterExit { message: name }),
            };
        }

        match message {
            AppMessage::Ready(_) => Err(Violation::AlreadyReady),
            AppMessage::Frame(done) => {
                let Some(outstanding) = self.outstanding else {
                    return Err(Violation::NoFrameOutstanding);
                };
                if done.frame != outstanding {
                    return Err(Violation::UnknownFrame {
                        frame: done.frame.get(),
                    });
                }
                if let Some(regions) = done.damage.regions()
                    && regions.len() > MAX_DAMAGE_RECTS
                {
                    return Err(Violation::TooMuchDamage {
                        count: regions.len(),
                        max: MAX_DAMAGE_RECTS,
                    });
                }
                self.outstanding = None;
                Ok(())
            }
            AppMessage::Request(_) => Ok(()),
            AppMessage::Saved(_) => Err(Violation::SavedWithoutExit),
            AppMessage::Diagnostic(_) => unreachable!("handled above"),
        }
    }

    /// Builds the next draw request, or explains why there is not one.
    ///
    /// One frame at a time: a host that issued a second request while the
    /// first was outstanding would be asking an app to draw two versions of
    /// itself into one buffer. Redraw requests that arrive meanwhile are
    /// coalesced by the caller into the next call of this.
    pub fn draw(&mut self, reason: DrawReason) -> Result<HostMessage, HostFault> {
        if self.state != State::Running {
            return Err(HostFault::WrongState { state: self.state });
        }
        if let Some(frame) = self.outstanding {
            return Err(HostFault::FrameOutstanding { frame: frame.get() });
        }
        let frame = self.next_frame;
        self.next_frame = frame.next();
        self.outstanding = Some(frame);
        Ok(HostMessage::Draw(DrawRequest {
            frame,
            reason,
            viewport: self.viewport,
        }))
    }

    /// Marks the app suspended and builds the courtesy notification.
    ///
    /// `None` when the app is not running — including when it is already
    /// suspended, which happens whenever the device suspends twice without an
    /// intervening resume that the host managed to observe.
    ///
    /// An outstanding frame is abandoned rather than waited for: the app may
    /// not get another instruction before the device goes down, and a host
    /// holding a frame slot open across a suspend would never ask for another.
    pub fn suspend(&mut self) -> Option<HostMessage> {
        if self.state != State::Running {
            return None;
        }
        self.outstanding = None;
        self.state = State::Suspended;
        Some(HostMessage::Lifecycle(LifecycleEvent::Suspended))
    }

    /// Marks the app running again and builds the notification.
    pub fn resume(&mut self) -> Option<HostMessage> {
        if self.state != State::Suspended {
            return None;
        }
        self.state = State::Running;
        Some(HostMessage::Lifecycle(LifecycleEvent::Resumed))
    }

    /// Asks the app to save, with a bounded deadline.
    ///
    /// Legal from `Running` and from `Suspended` — a suspended app still has
    /// to be told the device is about to sleep for good. Not legal twice: the
    /// second call returns `None` rather than restarting the clock, because a
    /// deadline that can be extended is not a deadline.
    pub fn prepare_to_exit(
        &mut self,
        reason: ExitReason,
        deadline: Duration,
    ) -> Option<HostMessage> {
        if !matches!(self.state, State::Running | State::Suspended) {
            return None;
        }
        self.outstanding = None;
        self.state = State::Exiting;
        self.exiting = Some(reason);
        Some(HostMessage::Lifecycle(LifecycleEvent::prepare_to_exit(
            reason, deadline,
        )))
    }

    /// [`Self::prepare_to_exit`] with the default budget.
    pub fn prepare_to_exit_default(&mut self, reason: ExitReason) -> Option<HostMessage> {
        self.prepare_to_exit(reason, EXIT_DEADLINE)
    }

    /// Closes the session and builds the goodbye, if one is still owed.
    pub fn close(&mut self) -> Option<HostMessage> {
        if self.state == State::Closed {
            return None;
        }
        self.state = State::Closed;
        Some(HostMessage::Goodbye)
    }
}

/// The wire name of an app message, for error text.
const fn app_message_name(message: &AppMessage) -> &'static str {
    match message {
        AppMessage::Ready(_) => "ready",
        AppMessage::Frame(_) => "frame",
        AppMessage::Request(_) => "request",
        AppMessage::Saved(_) => "saved",
        AppMessage::Diagnostic(_) => "diagnostic",
    }
}

#[cfg(test)]
mod tests {
    use super::{HostFault, Session, State, Violation};
    use crate::geometry::{Rect, Size};
    use crate::lifecycle::{ExitReason, Request};
    use crate::limits::{MAX_DAMAGE_RECTS, MAX_DIAGNOSTIC_BYTES};
    use crate::message::{
        AppMessage, Damage, Diagnostic, DiagnosticLevel, DrawReason, FrameDone, FrameId,
        HostMessage, Ready, Saved, SessionId,
    };
    use crate::version::{CURRENT, ProtocolVersion};

    fn session() -> Session {
        Session::greeting(SessionId::new(1), CURRENT, Size::new(1620, 2160))
    }

    fn ready() -> AppMessage {
        AppMessage::Ready(Ready { protocol: CURRENT })
    }

    fn running() -> Session {
        let mut session = session();
        session.on_app_message(&ready()).expect("ready is legal");
        session
    }

    fn frame_id(message: &HostMessage) -> FrameId {
        match message {
            HostMessage::Draw(request) => request.frame,
            other => panic!("expected a draw request, got {other:?}"),
        }
    }

    #[test]
    fn a_session_starts_by_waiting_for_readiness() {
        let mut session = session();
        assert_eq!(session.state(), State::Greeting);
        assert!(session.draw(DrawReason::First).is_err());
        session.on_app_message(&ready()).unwrap();
        assert_eq!(session.state(), State::Running);
        assert_eq!(session.app_protocol(), Some(CURRENT));
    }

    /// Missing readiness: everything an app might want to do first is refused
    /// until it has said it is ready.
    #[test]
    fn nothing_but_readiness_is_legal_before_readiness() {
        for message in [
            AppMessage::Request(Request::Redraw),
            AppMessage::Frame(FrameDone {
                frame: FrameId::FIRST,
                damage: Damage::Full,
            }),
            AppMessage::Saved(Saved {
                reason: ExitReason::Sleep,
                ok: true,
            }),
        ] {
            let mut session = session();
            assert!(matches!(
                session.on_app_message(&message),
                Err(Violation::BeforeReady { .. })
            ));
            assert_eq!(session.state(), State::Closed);
        }
    }

    /// An app that cannot start is allowed to say why before it is ready. This
    /// is the one exception, and it exists because the alternative is a silent
    /// launch failure.
    #[test]
    fn an_app_may_complain_before_it_is_ready() {
        let mut session = session();
        session
            .on_app_message(&AppMessage::Diagnostic(Diagnostic::new(
                DiagnosticLevel::Error,
                "could not map the surface",
            )))
            .expect("a diagnostic is legal while greeting");
        assert_eq!(session.state(), State::Greeting);
    }

    #[test]
    fn readiness_twice_is_a_violation() {
        let mut session = running();
        assert_eq!(
            session.on_app_message(&ready()),
            Err(Violation::AlreadyReady)
        );
    }

    /// Unsupported protocol, caught on the connection rather than in a
    /// manifest. A binary claiming a newer minor than the host is refused; an
    /// older one is fine, which is the one-directional rule.
    #[test]
    fn a_binary_from_the_future_does_not_get_to_run() {
        let mut session = session();
        let newer = AppMessage::Ready(Ready {
            protocol: ProtocolVersion::new(CURRENT.major(), CURRENT.minor() + 1),
        });
        assert!(matches!(
            session.on_app_message(&newer),
            Err(Violation::UnsupportedProtocol { .. })
        ));
        assert_eq!(session.state(), State::Closed);

        let mut host = Session::greeting(
            SessionId::new(2),
            ProtocolVersion::new(1, 4),
            Size::new(10, 10),
        );
        assert!(
            host.on_app_message(&AppMessage::Ready(Ready {
                protocol: ProtocolVersion::new(1, 2),
            }))
            .is_ok()
        );
    }

    #[test]
    fn a_different_major_is_refused_in_both_directions() {
        for app_major in [0, 2] {
            let mut session = session();
            assert!(matches!(
                session.on_app_message(&AppMessage::Ready(Ready {
                    protocol: ProtocolVersion::new(app_major, 0),
                })),
                Err(Violation::UnsupportedProtocol { .. })
            ));
        }
    }

    #[test]
    fn frames_are_answered_once_and_only_when_asked_for() {
        let mut session = running();
        let request = session.draw(DrawReason::First).unwrap();
        let frame = frame_id(&request);
        assert_eq!(session.outstanding_frame(), Some(frame));

        // A second request while one is outstanding is the host's bug.
        assert!(matches!(
            session.draw(DrawReason::AppRequested),
            Err(HostFault::FrameOutstanding { .. })
        ));

        let done = AppMessage::Frame(FrameDone {
            frame,
            damage: Damage::Full,
        });
        session.on_app_message(&done).unwrap();
        assert_eq!(session.outstanding_frame(), None);

        // Answering it again is the app's.
        assert_eq!(
            session.on_app_message(&done),
            Err(Violation::NoFrameOutstanding)
        );
    }

    #[test]
    fn a_frame_nobody_asked_for_is_a_violation() {
        let mut session = running();
        let request = session.draw(DrawReason::First).unwrap();
        let frame = frame_id(&request);
        assert!(matches!(
            session.on_app_message(&AppMessage::Frame(FrameDone {
                frame: frame.next(),
                damage: Damage::Full,
            })),
            Err(Violation::UnknownFrame { .. })
        ));
    }

    #[test]
    fn an_unbounded_damage_list_is_refused() {
        let mut session = running();
        let request = session.draw(DrawReason::First).unwrap();
        let frame = frame_id(&request);
        let regions = vec![Rect::new(0.0, 0.0, 1.0, 1.0); MAX_DAMAGE_RECTS + 1];
        assert!(matches!(
            session.on_app_message(&AppMessage::Frame(FrameDone {
                frame,
                damage: Damage::Regions { regions },
            })),
            Err(Violation::TooMuchDamage { .. })
        ));
    }

    #[test]
    fn a_suspended_app_is_not_asked_to_draw() {
        let mut session = running();
        session.draw(DrawReason::First).unwrap();
        assert!(session.suspend().is_some());
        assert_eq!(session.state(), State::Suspended);
        // The frame in flight was abandoned, not held open across the suspend.
        assert_eq!(session.outstanding_frame(), None);
        assert!(matches!(
            session.draw(DrawReason::HostRequested),
            Err(HostFault::WrongState { .. })
        ));

        assert!(session.suspend().is_none(), "suspending twice says nothing");
        assert!(session.resume().is_some());
        assert_eq!(session.state(), State::Running);
        assert!(session.draw(DrawReason::HostRequested).is_ok());
    }

    #[test]
    fn after_prepare_to_exit_only_saving_is_left() {
        let mut session = running();
        session
            .prepare_to_exit_default(ExitReason::Sleep)
            .expect("legal while running");
        assert_eq!(session.state(), State::Exiting);

        assert!(matches!(
            session.on_app_message(&AppMessage::Request(Request::Redraw)),
            Err(Violation::AfterExit { message: "request" })
        ));
    }

    #[test]
    fn saving_closes_the_session_and_must_answer_the_right_reason() {
        let mut session = running();
        session
            .prepare_to_exit_default(ExitReason::ReturnToStock)
            .unwrap();
        assert!(matches!(
            session.on_app_message(&AppMessage::Saved(Saved {
                reason: ExitReason::Shutdown,
                ok: true,
            })),
            Err(Violation::SavedWrongReason { .. })
        ));

        let mut session = running();
        session
            .prepare_to_exit_default(ExitReason::ReturnToStock)
            .unwrap();
        session
            .on_app_message(&AppMessage::Saved(Saved {
                reason: ExitReason::ReturnToStock,
                ok: false,
            }))
            .expect("a failed save is still a save report");
        assert_eq!(session.state(), State::Closed);
    }

    #[test]
    fn saving_without_being_asked_is_a_violation() {
        let mut session = running();
        assert_eq!(
            session.on_app_message(&AppMessage::Saved(Saved {
                reason: ExitReason::Sleep,
                ok: true,
            })),
            Err(Violation::SavedWithoutExit)
        );
    }

    /// A deadline that can be restarted is not a deadline.
    #[test]
    fn the_exit_deadline_is_not_extendable() {
        let mut session = running();
        assert!(session.prepare_to_exit_default(ExitReason::Sleep).is_some());
        assert!(session.prepare_to_exit_default(ExitReason::Sleep).is_none());
    }

    /// A suspended app still gets told the device is going down for good.
    #[test]
    fn a_suspended_app_can_still_be_asked_to_save() {
        let mut session = running();
        session.suspend().unwrap();
        assert!(session.prepare_to_exit_default(ExitReason::Sleep).is_some());
    }

    #[test]
    fn a_violation_closes_the_session_for_good() {
        let mut session = session();
        assert!(
            session
                .on_app_message(&AppMessage::Request(Request::Redraw))
                .is_err()
        );
        assert!(matches!(
            session.on_app_message(&ready()),
            Err(Violation::AfterClose { message: "ready" })
        ));
    }

    /// The SDK truncates, so an over-long diagnostic means the app bypassed
    /// the SDK — which is exactly the case this layer exists for.
    #[test]
    fn an_oversized_diagnostic_is_a_violation_not_a_truncation() {
        let mut session = running();
        let over = AppMessage::Diagnostic(crate::message::Diagnostic {
            level: DiagnosticLevel::Info,
            message: "x".repeat(MAX_DIAGNOSTIC_BYTES + 1),
        });
        assert!(matches!(
            session.on_app_message(&over),
            Err(Violation::DiagnosticTooLong { .. })
        ));
    }

    #[test]
    fn closing_says_goodbye_exactly_once() {
        let mut session = running();
        assert!(matches!(session.close(), Some(HostMessage::Goodbye)));
        assert!(session.close().is_none());
    }
}
