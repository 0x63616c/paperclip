//! Bridging an app's [`DiagnosticRecord`] into the same pipeline as
//! everything else (WWW-46).
//!
//! `platform/protocol`'s `Diagnostic`/`DiagnosticRecord` types (and
//! `Diagnostic::tagged`, and the `SessionId` that tags it) already existed —
//! nothing here changes their shape. What was missing was anywhere that
//! turned a received one into a log line; [`record`] is that.
//!
//! There is deliberately no host-side receive loop wired to this yet:
//! `platform/host` does not currently read `AppMessage` off a connection at
//! all (there is no such loop in the crate to hook), so an app's `Diagnostic`
//! has nowhere to travel from today. This function is the pipeline's far end,
//! ready for whichever issue builds that loop to call it.

use paper_protocol::{DiagnosticLevel, DiagnosticRecord};

/// Emits `record` as a `tracing` event, tagged with the app, its version and
/// the session that sent it — exactly the identity `Diagnostic::tagged`
/// attaches, and exactly what makes a journal entry attributable to one
/// running app rather than "something wrote to the log".
pub fn record(record: &DiagnosticRecord) {
    match record.level {
        DiagnosticLevel::Info => tracing::info!(
            target: "paper_telemetry::diagnostic",
            app = %record.app,
            version = %record.version,
            session = %record.session,
            "{}",
            record.message
        ),
        DiagnosticLevel::Warn => tracing::warn!(
            target: "paper_telemetry::diagnostic",
            app = %record.app,
            version = %record.version,
            session = %record.session,
            "{}",
            record.message
        ),
        DiagnosticLevel::Error => tracing::error!(
            target: "paper_telemetry::diagnostic",
            app = %record.app,
            version = %record.version,
            session = %record.session,
            "{}",
            record.message
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use paper_protocol::{AppId, Diagnostic, SessionId};
    use std::str::FromStr as _;

    #[test]
    fn every_level_emits_without_panicking() {
        // A trace event with no subscriber installed goes nowhere, which is
        // exactly what this test wants to prove: `record` never panics or
        // requires telemetry to be initialised first, so a caller cannot
        // crash a session by logging before `init_pretty`/`init_journald` ran.
        let app = AppId::from_str("dev.calum.chess").expect("valid id");
        let session = SessionId::new(1);
        for level in [
            DiagnosticLevel::Info,
            DiagnosticLevel::Warn,
            DiagnosticLevel::Error,
        ] {
            let tagged = Diagnostic::new(level, "hello").tagged(
                app.clone(),
                semver::Version::new(1, 0, 0),
                session,
            );
            record(&tagged);
        }
    }
}
