//! Answers the no-grant `SystemQuery`s any app may send (WWW-50, ADR-0028):
//! time, battery, network and platform facts. No capability check gates any
//! of them — see the ADR for why this tier is unconditional.
//!
//! Lives here, not in `platform/host`, because [`Session`](crate::session::Session)
//! is the one real process this workspace runs apps under today — `platform/host`
//! is the systemd/cgroup supervisor crate and never touches the wire protocol.
//! The Settings admin surface (`SettingsHost`'s nine methods) is deliberately
//! not part of this responder; see the ADR for why that stays out of scope
//! here.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use paper_protocol::{
    BatteryFact, BatteryState, MAX_SYSTEM_QUERIES_PER_SECOND, NetworkFact, PlatformFact,
    SystemAnswer, SystemDenial, SystemDenialReason, SystemEvent, SystemQuery, SystemQueryKind,
    SystemValue, TimeFact,
};
use paper_sys::{
    BatteryReading, ChargeDirection, Network, NmcliNetwork, PowerSource, SystemPowerSource,
    SystemProcess, SystemWallClock, WallClock,
};

/// A rolling one-second budget of queries.
///
/// Per session rather than global: one misbehaving app must not cost every
/// other running app its own battery reading.
#[derive(Debug, Default)]
struct RateWindow {
    sent: VecDeque<Instant>,
}

impl RateWindow {
    /// Whether one more query is allowed right now, recording it if so.
    fn allow(&mut self, now: Instant) -> bool {
        while let Some(&oldest) = self.sent.front() {
            if now.duration_since(oldest) >= Duration::from_secs(1) {
                self.sent.pop_front();
            } else {
                break;
            }
        }
        if self.sent.len() >= MAX_SYSTEM_QUERIES_PER_SECOND as usize {
            return false;
        }
        self.sent.push_back(now);
        true
    }
}

/// Answers no-grant [`SystemQuery`]s and notices when a pushable fact
/// changed, over injected backends — real ones in [`Self::new`], scripted
/// ones in a test.
#[derive(Debug)]
pub(crate) struct SystemResponder<P: PowerSource, N: Network, W: WallClock> {
    power: P,
    network: N,
    clock: W,
    window: RateWindow,
    last_battery: Option<BatteryFact>,
    last_network: Option<NetworkFact>,
}

impl SystemResponder<SystemPowerSource, NmcliNetwork<SystemProcess>, SystemWallClock> {
    /// A responder over the real backends: `/sys/class/power_supply`,
    /// `nmcli` and the wall clock.
    pub(crate) fn new() -> Self {
        Self::over(
            SystemPowerSource::default(),
            NmcliNetwork::new(SystemProcess),
            SystemWallClock,
        )
    }
}

impl<P: PowerSource, N: Network, W: WallClock> SystemResponder<P, N, W> {
    /// A responder over specific backends — the real ones from [`Self::new`],
    /// or scripted fakes in a test.
    pub(crate) fn over(power: P, network: N, clock: W) -> Self {
        Self {
            power,
            network,
            clock,
            window: RateWindow::default(),
            last_battery: None,
            last_network: None,
        }
    }

    /// Answers one query, denying it if this session has asked too often.
    pub(crate) fn answer(&mut self, query: SystemQuery) -> SystemAnswer {
        if !self.window.allow(Instant::now()) {
            return SystemAnswer {
                id: query.id,
                result: Err(SystemDenial::new(SystemDenialReason::RateLimited)),
            };
        }
        SystemAnswer {
            id: query.id,
            result: self.value(query.kind),
        }
    }

    fn value(&mut self, kind: SystemQueryKind) -> Result<SystemValue, SystemDenial> {
        match kind {
            SystemQueryKind::Time => Ok(SystemValue::Time(TimeFact {
                unix_millis: self.clock.now_unix_millis(),
            })),
            SystemQueryKind::Battery => {
                let fact = battery_fact(self.power.read())
                    .ok_or_else(|| SystemDenial::new(SystemDenialReason::BackendUnavailable))?;
                self.last_battery = Some(fact);
                Ok(SystemValue::Battery(fact))
            }
            SystemQueryKind::Network => {
                let fact = network_fact(self.network.read())
                    .ok_or_else(|| SystemDenial::new(SystemDenialReason::BackendUnavailable))?;
                self.last_network = Some(fact.clone());
                Ok(SystemValue::Network(fact))
            }
            SystemQueryKind::Platform => Ok(SystemValue::Platform(PlatformFact {
                paperclip_version: env!("CARGO_PKG_VERSION").to_owned(),
                // Neither is verified against real device evidence yet — see
                // `paper_settings::host::LiveHost::platform_info`, which
                // reports the same honest placeholder rather than a guess.
                firmware: "NOT VERIFIED".to_owned(),
                active_release: "NOT VERIFIED".to_owned(),
            })),
            // `SystemQueryKind` is `#[non_exhaustive]`: a future minor
            // protocol bump can add a kind this build predates.
            _ => Err(SystemDenial::new(SystemDenialReason::Unsupported)),
        }
    }

    /// Facts that changed since the last time this or [`Self::answer`] looked
    /// — for a host to push as [`SystemEvent`]s without being asked.
    ///
    /// Only battery and network are pushable (module doc on
    /// [`paper_protocol::system`]): a clock ticking is not a change worth an
    /// app redrawing over.
    pub(crate) fn changes(&mut self) -> Vec<SystemEvent> {
        let mut events = Vec::new();
        if let Some(fact) = battery_fact(self.power.read())
            && self.last_battery != Some(fact)
        {
            self.last_battery = Some(fact);
            events.push(SystemEvent::Battery(fact));
        }
        if let Some(fact) = network_fact(self.network.read())
            && self.last_network.as_ref() != Some(&fact)
        {
            self.last_network = Some(fact.clone());
            events.push(SystemEvent::Network(fact));
        }
        events
    }
}

fn battery_fact(reading: Option<BatteryReading>) -> Option<BatteryFact> {
    reading.map(|reading| BatteryFact {
        percent: reading.percent,
        state: match reading.direction {
            ChargeDirection::Charging => BatteryState::Charging,
            ChargeDirection::Discharging => BatteryState::Discharging,
            ChargeDirection::Full => BatteryState::Full,
            ChargeDirection::Unknown => BatteryState::Unknown,
        },
    })
}

fn network_fact(reading: Option<paper_sys::NetworkReading>) -> Option<NetworkFact> {
    reading.map(|reading| NetworkFact {
        connected: reading.connected,
        ssid: reading.ssid,
        signal_percent: reading.signal_percent,
    })
}

#[cfg(test)]
mod tests {
    use paper_protocol::QueryId;
    use paper_testing::{FakeNetwork, FakePowerSource, FakeWallClock};

    use super::*;

    fn responder() -> SystemResponder<FakePowerSource, FakeNetwork, FakeWallClock> {
        SystemResponder::over(
            FakePowerSource::new(),
            FakeNetwork::new(),
            FakeWallClock::at(1_700_000_000_000),
        )
    }

    #[test]
    fn a_backend_with_nothing_to_report_is_a_named_denial_not_a_guess() {
        let mut responder = responder();
        let answer = responder.answer(SystemQuery {
            id: QueryId::new(1),
            kind: SystemQueryKind::Battery,
        });
        assert_eq!(
            answer.result,
            Err(SystemDenial::new(SystemDenialReason::BackendUnavailable))
        );
    }

    #[test]
    fn a_working_backend_answers_with_the_value() {
        let mut responder = responder();
        responder.power.set(BatteryReading {
            percent: 71,
            direction: ChargeDirection::Discharging,
        });
        let answer = responder.answer(SystemQuery {
            id: QueryId::new(1),
            kind: SystemQueryKind::Battery,
        });
        assert_eq!(
            answer.result,
            Ok(SystemValue::Battery(BatteryFact {
                percent: 71,
                state: BatteryState::Discharging,
            }))
        );
    }

    #[test]
    fn time_reads_the_injected_clock() {
        let mut responder = responder();
        let answer = responder.answer(SystemQuery {
            id: QueryId::new(1),
            kind: SystemQueryKind::Time,
        });
        assert_eq!(
            answer.result,
            Ok(SystemValue::Time(TimeFact {
                unix_millis: 1_700_000_000_000
            }))
        );
    }

    #[test]
    fn asking_past_the_budget_is_rate_limited_not_answered() {
        let mut responder = responder();
        responder.power.set(BatteryReading {
            percent: 50,
            direction: ChargeDirection::Full,
        });
        for i in 0..MAX_SYSTEM_QUERIES_PER_SECOND {
            let answer = responder.answer(SystemQuery {
                id: QueryId::new(u64::from(i)),
                kind: SystemQueryKind::Battery,
            });
            assert!(answer.result.is_ok(), "query {i} should be within budget");
        }
        let denied = responder.answer(SystemQuery {
            id: QueryId::new(999),
            kind: SystemQueryKind::Battery,
        });
        assert_eq!(
            denied.result,
            Err(SystemDenial::new(SystemDenialReason::RateLimited))
        );
    }

    #[test]
    fn changes_reports_only_what_actually_changed() {
        let mut responder = responder();
        responder.power.set(BatteryReading {
            percent: 90,
            direction: ChargeDirection::Charging,
        });
        let first = responder.changes();
        assert_eq!(
            first,
            vec![SystemEvent::Battery(BatteryFact {
                percent: 90,
                state: BatteryState::Charging,
            })]
        );

        // Nothing moved, so the second poll is silent.
        assert!(responder.changes().is_empty());

        responder.power.set(BatteryReading {
            percent: 89,
            direction: ChargeDirection::Discharging,
        });
        let second = responder.changes();
        assert_eq!(
            second,
            vec![SystemEvent::Battery(BatteryFact {
                percent: 89,
                state: BatteryState::Discharging,
            })]
        );
    }
}
