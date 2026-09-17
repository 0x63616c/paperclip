//! What happens to an app between launch and exit, and what it can ask for.

use std::fmt;
use std::time::Duration;

use crate::id::AppId;

/// Why an app process exists.
///
/// Carried in [`Hello`](crate::Hello) so an app knows, at its first
/// instruction, whether this is a fresh start or the platform putting it back
/// together after something went wrong. Crash recovery is prioritised over
/// isolation on this device (§4), and an app that cannot tell the two apart
/// cannot take part in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum LaunchReason {
    /// The user opened it.
    Fresh,
    /// The previous process of this app ended without saving — it crashed, or
    /// it missed its exit deadline. Whatever is in private storage is the last
    /// good save, and it may be older than what the user last saw.
    Restarted,
    /// The platform is restoring a session it took down on purpose: sleep,
    /// lock, or a platform update. A save exists and is current.
    Restored,
}

/// Why an app is being asked to stop.
///
/// Every variant is a reason the *user* would recognise, because the app's
/// only sensible response — save and be ready to disappear — is the same for
/// all of them and the reason exists for what the app writes in its save, not
/// for branching on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ExitReason {
    /// The user went Home, or opened something else.
    SwitchedAway,
    /// The user asked to go back to stock reMarkable (§4).
    ReturnToStock,
    /// The tablet is going to sleep or locking. Paperclip saves and returns to
    /// stock rather than trying to resume seamlessly (§4).
    Sleep,
    /// The platform is being updated and every app is coming down with it.
    PlatformUpdate,
    /// This app is being updated or removed.
    AppUpdate,
    /// The host is shutting down.
    Shutdown,
}

impl fmt::Display for ExitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::SwitchedAway => "switched away",
            Self::ReturnToStock => "return to reMarkable",
            Self::Sleep => "sleep",
            Self::PlatformUpdate => "platform update",
            Self::AppUpdate => "app update",
            Self::Shutdown => "shutdown",
        };
        f.write_str(text)
    }
}

/// Something that happened to the app rather than in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", tag = "event")]
#[non_exhaustive]
pub enum LifecycleEvent {
    /// The app is no longer the foreground app, or the device is suspending.
    ///
    /// **Best effort, and an app must not depend on receiving it.** The tablet
    /// can suspend between any two instructions, and a host that is itself
    /// being suspended has no opportunity to send anything. Treat it as a
    /// courtesy — a chance to stop a timer — never as the guarantee that a
    /// save happens on. That guarantee is [`Self::PrepareToExit`].
    Suspended,
    /// The app is foreground again after a suspend.
    ///
    /// The panel is not presentable the instant this arrives; the host
    /// absorbs that wait (WWW-3 measured roughly 77 ms after resume) and will
    /// ask for a frame when it is ready. An app that draws on `Resumed`
    /// instead of on the draw request is drawing into a surface nobody is
    /// scanning out.
    Resumed,
    /// Save now. The process ends when the deadline passes, saved or not.
    ///
    /// Bounded by construction: `deadline_ms` is a number in this message
    /// rather than a convention, so an app can size its save path against the
    /// actual budget it was given. See
    /// [`MIN_EXIT_DEADLINE`](crate::MIN_EXIT_DEADLINE) for the floor.
    PrepareToExit {
        /// Why.
        reason: ExitReason,
        /// How long there is, in milliseconds, from the moment this was sent.
        deadline_ms: u32,
    },
}

impl LifecycleEvent {
    /// Builds a prepare-to-exit event from a [`Duration`].
    ///
    /// Saturating: a deadline longer than a `u32` of milliseconds is a bug
    /// somewhere upstream, and 49 days is indistinguishable from "no deadline"
    /// for the app's purposes.
    pub fn prepare_to_exit(reason: ExitReason, deadline: Duration) -> Self {
        Self::PrepareToExit {
            reason,
            deadline_ms: u32::try_from(deadline.as_millis()).unwrap_or(u32::MAX),
        }
    }

    /// The deadline as a [`Duration`], for the one variant that has one.
    pub fn deadline(self) -> Option<Duration> {
        match self {
            Self::PrepareToExit { deadline_ms, .. } => {
                Some(Duration::from_millis(u64::from(deadline_ms)))
            }
            _ => None,
        }
    }
}

/// Something an app asks the platform to do.
///
/// Four things an app may ask for, and nothing else. There is no "install
/// this" and no "put me full screen": an app influences the platform only
/// through these, and everything else is the host's decision or the user's.
///
/// [`Self::Launch`] is the one addition since §8's original sketch (ADR-0016)
/// — Home cannot do its one job, launching the app a tile names, without it.
/// It is still not "an app may run anything": the host decides whether the
/// named app is installed and runnable, exactly as it already decides for
/// [`Self::Home`] and [`Self::ReturnToStock`]. See ADR-0018.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Request {
    /// Draw me again. The host decides when, and may coalesce several of these
    /// into one frame.
    Redraw,
    /// Take the user back to the home screen.
    Home,
    /// Leave Paperclip and hand the tablet back to stock reMarkable (§4).
    ReturnToStock,
    /// Launch a specific installed app. Home's request, and — for now —
    /// only Home's: nothing else on the shelf has another app to name.
    Launch(AppId),
}

/// What an app's event handler decided.
///
/// The `None`/`Redraw`/`Home`/`ReturnToStock`/`Launch` shape [`Request`]
/// carries on the wire. `None` has no wire representation — it is the
/// *absence* of a request — so the message set carries [`Request`], which has
/// no way to spell "nothing". An app returning [`Action::None`] sends nothing
/// at all, which is what an app does for the overwhelming majority of events
/// it sees.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Action {
    /// Nothing to do. Not a message.
    #[default]
    None,
    /// [`Request::Redraw`].
    Redraw,
    /// [`Request::Home`].
    Home,
    /// [`Request::ReturnToStock`].
    ReturnToStock,
    /// [`Request::Launch`].
    Launch(AppId),
}

impl Action {
    /// The request this action sends, if it sends one.
    pub fn request(self) -> Option<Request> {
        match self {
            Self::None => None,
            Self::Redraw => Some(Request::Redraw),
            Self::Home => Some(Request::Home),
            Self::ReturnToStock => Some(Request::ReturnToStock),
            Self::Launch(id) => Some(Request::Launch(id)),
        }
    }
}

impl From<Request> for Action {
    fn from(request: Request) -> Self {
        match request {
            Request::Redraw => Self::Redraw,
            Request::Home => Self::Home,
            Request::ReturnToStock => Self::ReturnToStock,
            Request::Launch(id) => Self::Launch(id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, ExitReason, LifecycleEvent, Request};
    use crate::id::AppId;
    use std::time::Duration;

    fn chess_id() -> AppId {
        "dev.calum.chess".parse().expect("a valid app id")
    }

    /// `Action::None` must not be spellable on the wire, or a host would have
    /// to decide what an app meant by asking for nothing.
    #[test]
    fn doing_nothing_is_not_a_message() {
        assert_eq!(Action::None.request(), None);
        for action in [
            Action::Redraw,
            Action::Home,
            Action::ReturnToStock,
            Action::Launch(chess_id()),
        ] {
            let request = action.clone().request().expect("sends something");
            assert_eq!(Action::from(request), action);
        }
    }

    #[test]
    fn a_prepare_to_exit_deadline_survives_the_round_trip() {
        let event = LifecycleEvent::prepare_to_exit(ExitReason::Sleep, Duration::from_millis(1500));
        assert_eq!(event.deadline(), Some(Duration::from_millis(1500)));
        assert_eq!(LifecycleEvent::Suspended.deadline(), None);
    }

    /// An absurd deadline saturates rather than wrapping to a short one: an
    /// app given 49 days behaves the same as one given forever, but an app
    /// given 6 ms because a `u32` wrapped would lose the save.
    #[test]
    fn an_absurd_deadline_saturates_rather_than_wrapping() {
        let event = LifecycleEvent::prepare_to_exit(
            ExitReason::Shutdown,
            Duration::from_secs(60 * 60 * 24 * 365),
        );
        assert_eq!(
            event.deadline(),
            Some(Duration::from_millis(u64::from(u32::MAX)))
        );
    }

    #[test]
    fn requests_round_trip_through_their_wire_form() {
        for request in [
            Request::Redraw,
            Request::Home,
            Request::ReturnToStock,
            Request::Launch(chess_id()),
        ] {
            let json = serde_json::to_string(&request).unwrap();
            assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
        }
        assert_eq!(
            serde_json::to_string(&Request::ReturnToStock).unwrap(),
            "\"return-to-stock\""
        );
    }

    #[test]
    fn a_launch_request_names_the_app_on_the_wire() {
        let json = serde_json::to_string(&Request::Launch(chess_id())).unwrap();
        assert_eq!(json, "{\"launch\":\"dev.calum.chess\"}");
    }
}
