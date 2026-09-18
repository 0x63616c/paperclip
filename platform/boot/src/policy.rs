//! The boot decision (§10): pure, so every combination of counter and
//! marker state can be asserted on a Mac (WWW-53).
//!
//! What actually reads the counter and the marker off disk is
//! [`crate::counter`] and [`crate::autostart`]; what acts on the result —
//! writing units, starting the supervisor, grading it against `state=home`
//! — is `paperclip-launcher`, compiled only for Linux and proven in the VM
//! harness. Keeping the decision itself free of I/O is what lets this file
//! be the one place "should this boot start Paperclip" is answered, instead
//! of that answer being implicit in whatever order the launcher happens to
//! check things.

use crate::counter::BootCounter;

/// What the launcher should do this boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Start Paperclip: write the runtime units, bring the supervisor up,
    /// and grade it against `state=home`.
    Launch,
    /// Do nothing. The device boots to stock, exactly as it would if
    /// Paperclip were not installed at all.
    Skip(SkipReason),
}

/// Why the launcher is not starting Paperclip this boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `paperctl autostart disable` was run, or safe mode set the same
    /// marker after the launcher unit exceeded systemd's own start limit.
    Disabled,
    /// [`MAX_BOOT_ATTEMPTS`](crate::counter::MAX_BOOT_ATTEMPTS) consecutive
    /// boots failed to reach Home.
    AttemptsExhausted {
        /// How many consecutive failures were recorded.
        attempts: u32,
    },
}

impl SkipReason {
    /// One line worth recording as the reason: what the safe-mode marker
    /// carries, and what `paperctl autostart status` prints.
    pub fn describe(self) -> String {
        match self {
            SkipReason::Disabled => "autostart is disabled".to_owned(),
            SkipReason::AttemptsExhausted { attempts } => format!(
                "{attempts} consecutive boot attempt(s) failed to reach Home; \
                 autostart refuses to try again until it is reset"
            ),
        }
    }
}

/// Decides what this boot should do.
///
/// `disabled` is checked first and unconditionally: a human who disabled
/// autostart gets silence, not "tried once more anyway". The counter is
/// checked second: crossing [`MAX_BOOT_ATTEMPTS`](crate::counter::MAX_BOOT_ATTEMPTS)
/// on the *previous* boot is what put the device here, and — deliberately,
/// matching §10's "a spent failure budget refuses requests" — nothing this
/// function observes clears it again. Only an explicit `paperctl autostart
/// enable`/`reset`, or a fresh install, does.
pub fn decide(counter: BootCounter, disabled: bool) -> Decision {
    if disabled {
        return Decision::Skip(SkipReason::Disabled);
    }
    if counter.exhausted() {
        return Decision::Skip(SkipReason::AttemptsExhausted {
            attempts: counter.attempts,
        });
    }
    Decision::Launch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disabled_device_skips_regardless_of_the_counter() {
        let counter = BootCounter::default();
        assert_eq!(decide(counter, true), Decision::Skip(SkipReason::Disabled));
    }

    #[test]
    fn disabled_wins_even_over_an_exhausted_counter() {
        let counter = BootCounter { attempts: 9 };
        assert_eq!(decide(counter, true), Decision::Skip(SkipReason::Disabled));
    }

    #[test]
    fn a_fresh_counter_launches() {
        assert_eq!(decide(BootCounter::default(), false), Decision::Launch);
    }

    #[test]
    fn two_prior_failures_still_launches() {
        let counter = BootCounter { attempts: 2 };
        assert_eq!(decide(counter, false), Decision::Launch);
    }

    #[test]
    fn three_prior_failures_skips_with_the_count_in_the_reason() {
        let counter = BootCounter { attempts: 3 };
        assert_eq!(
            decide(counter, false),
            Decision::Skip(SkipReason::AttemptsExhausted { attempts: 3 })
        );
    }

    #[test]
    fn the_skip_reasons_describe_themselves_for_a_human_reading_them_over_ssh() {
        assert!(SkipReason::Disabled.describe().contains("disabled"));
        assert!(
            SkipReason::AttemptsExhausted { attempts: 3 }
                .describe()
                .contains('3')
        );
    }
}
