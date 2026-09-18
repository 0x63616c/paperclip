//! System chrome: the status bar the compositor draws itself, outside any
//! client's surface (WWW-52, WWW-79).
//!
//! An app cannot draw over this or remove it because [`draw`] never reads a
//! client's surface at all — it paints straight onto the panel-sized canvas
//! the compositor is assembling, unconditionally and last. Its background
//! fill covers the whole reserved rectangle before anything else is drawn
//! into it, so whatever a client wrote there earlier in the same canvas does
//! not survive the call. That ordering — chrome composited after and on top
//! of client content, every frame — is the entire enforcement mechanism;
//! there is no permission check to bypass because there is nothing here for
//! a client to ask.
//!
//! ## Facts, not a second source of truth
//!
//! [`ChromeState`] holds the same [`BatteryFact`]/[`NetworkFact`]/[`TimeFact`]
//! the no-grant `SystemQuery`/`SystemEvent` vocabulary already defines
//! (WWW-50, ADR-0028), and nothing else. It follows the same shape
//! `apps/home/src/app.rs`'s `HomeApp` already uses for its own battery
//! reading: a fact arrives from outside and is filed, rather than a second
//! backend reading power or network state on its own.
//!
//! ## What this does not do yet
//!
//! Nothing populates [`ChromeState`] from a real backend: there is no socket
//! (WWW-78) and no client whose frames this composes against yet (WWW-81).
//! [`ChromeState::apply_event`] and [`ChromeState::apply_time`] are the seam
//! later work drives; this ticket builds what they drive, not the driving
//! itself — the same split ADR-0033 made for `Surface` ahead of WWW-78's
//! event loop.
//!
//! Time renders in UTC. [`TimeFact`] carries no timezone, and nothing in
//! this workspace resolves one yet (`platform/sys/src/wallclock.rs` reads
//! [`std::time::SystemTime`], which has none either) — the label says `UTC`
//! rather than implying a local time this build cannot compute.

use paper_sdk::{
    BatteryFact, BatteryState, Canvas, NetworkFact, Rect, Size, SystemEvent, TimeFact,
    chrome as sdk_chrome,
};

/// The rectangle the compositor reserves for chrome, at the top of a panel
/// shaped like `panel_size`.
///
/// Full width, [`sdk_chrome::STATUS_BAR_HEIGHT`] tall — the same height an
/// app's own [`sdk_chrome::draw_status_bar`] already uses for its title bar,
/// reused rather than a second constant invented for the same number.
pub fn reserved_rect(panel_size: Size) -> Rect {
    Rect::new(
        0.0,
        0.0,
        panel_size.width as f32,
        sdk_chrome::STATUS_BAR_HEIGHT,
    )
}

/// Where a composited client's own content may draw, below [`reserved_rect`].
pub fn content_rect(panel_size: Size) -> Rect {
    sdk_chrome::status_bar_content_area(Rect::new(
        0.0,
        0.0,
        panel_size.width as f32,
        panel_size.height as f32,
    ))
}

/// The no-grant system facts chrome renders, filed as they arrive.
///
/// Starts with nothing known: before the first answer or push, [`draw`]
/// renders each element's placeholder rather than guessing a value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChromeState {
    time: Option<TimeFact>,
    battery: Option<BatteryFact>,
    network: Option<NetworkFact>,
}

impl ChromeState {
    /// Files a wall-clock reading — the answer to a `SystemQuery::Time`,
    /// since time is not pushed (module doc on `paper_protocol::system`: "a
    /// clock ticking is not a change worth an app redrawing over").
    pub fn apply_time(&mut self, fact: TimeFact) {
        self.time = Some(fact);
    }

    /// Files a battery reading, from either a query answer or a push.
    pub fn apply_battery(&mut self, fact: BatteryFact) {
        self.battery = Some(fact);
    }

    /// Files a network reading, from either a query answer or a push.
    pub fn apply_network(&mut self, fact: NetworkFact) {
        self.network = Some(fact);
    }

    /// Files whichever pushed fact `event` carries.
    pub fn apply_event(&mut self, event: SystemEvent) {
        match event {
            SystemEvent::Battery(fact) => self.apply_battery(fact),
            SystemEvent::Network(fact) => self.apply_network(fact),
            // `SystemEvent` is `#[non_exhaustive]`: a future minor protocol
            // bump can push a kind this build predates. Nothing to file yet.
            _ => {}
        }
    }
}

/// `HH:MM UTC`, or a placeholder before the first reading arrives.
fn format_time(fact: Option<TimeFact>) -> String {
    let Some(fact) = fact else {
        return "--:--".to_owned();
    };
    let seconds_of_day = (fact.unix_millis / 1000) % 86_400;
    format!(
        "{:02}:{:02} UTC",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60
    )
}

/// `NN%`, with a suffix for charging or full, or a placeholder.
fn format_battery(fact: Option<BatteryFact>) -> String {
    let Some(fact) = fact else {
        return "--%".to_owned();
    };
    let suffix = match fact.state {
        BatteryState::Charging => " CHG",
        BatteryState::Full => " FULL",
        // `BatteryState` is `#[non_exhaustive]`; `Discharging` and any
        // future state both render as a bare percentage.
        _ => "",
    };
    format!("{}%{suffix}", fact.percent)
}

/// The network's name when connected and named, otherwise a plain state
/// word, or a placeholder before any reading arrives.
fn format_network(fact: Option<NetworkFact>) -> String {
    match fact {
        None => "NO NETWORK".to_owned(),
        Some(fact) if !fact.connected => "OFFLINE".to_owned(),
        Some(NetworkFact {
            ssid: Some(ssid), ..
        }) => ssid,
        Some(_) => "CONNECTED".to_owned(),
    }
}

/// Draws chrome onto `canvas`, which must be sized like the panel it belongs
/// to, and returns the content rectangle below it.
///
/// Unconditional and last: [`sdk_chrome::draw_status_bar`] fills the whole
/// reserved rectangle with an opaque background before drawing the clock and
/// the battery/network line into it, so whatever `canvas` held inside that
/// rectangle beforehand does not survive the call. A caller assembling a
/// frame from a client's committed surface must call this after blitting
/// that surface, not before, for the guarantee to hold — see the module doc.
pub fn draw(canvas: &mut Canvas, state: &ChromeState) -> Rect {
    let clock = format_time(state.time);
    let trailing = format!(
        "{}  {}",
        format_network(state.network.clone()),
        format_battery(state.battery)
    );
    sdk_chrome::draw_status_bar(canvas, &clock, &trailing)
}

#[cfg(test)]
mod tests {
    use super::{
        ChromeState, content_rect, draw, format_battery, format_network, format_time, reserved_rect,
    };
    use paper_sdk::{
        BatteryFact, BatteryState, Canvas, Color, NetworkFact, Size, SystemEvent, TimeFact,
    };

    fn canvas() -> Canvas {
        Canvas::new(Size::new(400, 300)).expect("canvas allocates")
    }

    #[test]
    fn reserved_and_content_rects_tile_the_panel_without_a_gap() {
        let size = Size::new(400, 300);
        let reserved = reserved_rect(size);
        let content = content_rect(size);
        assert_eq!(reserved.y, 0.0);
        assert_eq!(content.y, reserved.height);
        assert_eq!(content.bottom(), size.height as f32);
    }

    #[test]
    fn drawing_returns_the_same_content_area_the_pure_function_computes() {
        let mut canvas = canvas();
        let drawn = draw(&mut canvas, &ChromeState::default());
        assert_eq!(content_rect(canvas.size()), drawn);
    }

    /// Pins down the acceptance criterion: whatever a client already put in
    /// the reserved rectangle is gone once chrome draws, regardless of what
    /// it was.
    #[test]
    fn chrome_overwrites_whatever_a_client_already_drew_in_its_rect() {
        let mut canvas = canvas();
        let sentinel = Color::rgb(0x00, 0xFF, 0x00);
        canvas.fill_rect(reserved_rect(canvas.size()), sentinel);
        assert_eq!(canvas.pixel(10, 10), Some(sentinel));

        draw(&mut canvas, &ChromeState::default());

        let reserved_height = reserved_rect(canvas.size()).height as u32;
        for y in 0..reserved_height {
            for x in (0..canvas.size().width).step_by(7) {
                assert_ne!(canvas.pixel(x, y), Some(sentinel), "at ({x}, {y})");
            }
        }
    }

    #[test]
    fn chrome_leaves_the_content_area_untouched() {
        let mut canvas = canvas();
        let sentinel = Color::rgb(0x00, 0xFF, 0x00);
        canvas.fill_rect(content_rect(canvas.size()), sentinel);

        draw(&mut canvas, &ChromeState::default());

        assert_eq!(canvas.pixel(10, canvas.size().height - 10), Some(sentinel));
    }

    #[test]
    fn nothing_known_yet_renders_placeholders_rather_than_a_guess() {
        assert_eq!(format_time(None), "--:--");
        assert_eq!(format_battery(None), "--%");
        assert_eq!(format_network(None), "NO NETWORK");
    }

    #[test]
    fn midnight_utc_is_00_00() {
        assert_eq!(format_time(Some(TimeFact { unix_millis: 0 })), "00:00 UTC");
    }

    #[test]
    fn a_time_mid_day_formats_hours_and_minutes() {
        let millis = ((13 * 3600) + (7 * 60)) * 1000;
        assert_eq!(
            format_time(Some(TimeFact {
                unix_millis: millis
            })),
            "13:07 UTC"
        );
    }

    #[test]
    fn a_charging_battery_is_labelled() {
        assert_eq!(
            format_battery(Some(BatteryFact {
                percent: 40,
                state: BatteryState::Charging
            })),
            "40% CHG"
        );
    }

    #[test]
    fn a_full_battery_is_labelled() {
        assert_eq!(
            format_battery(Some(BatteryFact {
                percent: 100,
                state: BatteryState::Full
            })),
            "100% FULL"
        );
    }

    #[test]
    fn a_discharging_battery_shows_only_the_percentage() {
        assert_eq!(
            format_battery(Some(BatteryFact {
                percent: 62,
                state: BatteryState::Discharging
            })),
            "62%"
        );
    }

    #[test]
    fn a_named_network_shows_its_ssid() {
        assert_eq!(
            format_network(Some(NetworkFact {
                connected: true,
                ssid: Some("home".to_owned()),
                signal_percent: Some(80),
            })),
            "home"
        );
    }

    #[test]
    fn a_connected_but_unnamed_network_says_so() {
        assert_eq!(
            format_network(Some(NetworkFact {
                connected: true,
                ssid: None,
                signal_percent: Some(40),
            })),
            "CONNECTED"
        );
    }

    #[test]
    fn a_disconnected_network_says_so_even_if_it_remembers_an_ssid() {
        assert_eq!(
            format_network(Some(NetworkFact {
                connected: false,
                ssid: Some("home".to_owned()),
                signal_percent: None,
            })),
            "OFFLINE"
        );
    }

    #[test]
    fn apply_event_files_the_matching_fact() {
        let mut state = ChromeState::default();
        let battery = BatteryFact {
            percent: 55,
            state: BatteryState::Discharging,
        };
        state.apply_event(SystemEvent::Battery(battery));
        assert_eq!(state.battery, Some(battery));

        let network = NetworkFact {
            connected: true,
            ssid: Some("office".to_owned()),
            signal_percent: Some(90),
        };
        state.apply_event(SystemEvent::Network(network.clone()));
        assert_eq!(state.network, Some(network));
    }

    #[test]
    fn drawing_with_facts_filed_produces_more_ink_than_placeholders() {
        let mut blank = canvas();
        draw(&mut blank, &ChromeState::default());

        let mut filled = canvas();
        let mut state = ChromeState::default();
        state.apply_time(TimeFact {
            unix_millis: 1_700_000_000_000,
        });
        state.apply_battery(BatteryFact {
            percent: 87,
            state: BatteryState::Discharging,
        });
        state.apply_network(NetworkFact {
            connected: true,
            ssid: Some("paperclip-lan".to_owned()),
            signal_percent: Some(75),
        });
        draw(&mut filled, &state);

        assert!(filled.ink_coverage() > blank.ink_coverage());
    }
}
