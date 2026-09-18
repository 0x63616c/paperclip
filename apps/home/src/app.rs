//! Home as a real [`App`] (§5).
//!
//! Everything with a decision behind it lives on [`HomeScreen::press`], which
//! takes no [`Context`] and is tested there directly. What is here is only
//! the wire adapter: feed pointer events in, draw on request, save nothing.
//! Home has no state worth persisting — the shelf is rebuilt from the catalog
//! on every launch — so [`App::save`] is a no-op.

use std::convert::Infallible;

use paper_sdk::{
    Action, App, BatteryFact, BatteryState, Canvas, Context, Event, SaveError, SystemEvent,
    SystemQueryKind, SystemValue,
};

use crate::screen::HomeScreen;

/// The label [`HomeScreen::set_fact`] files the battery reading under.
const BATTERY_FACT: &str = "Battery";

/// The Home app.
///
/// The shelf's contents are supplied at construction rather than discovered
/// here: enumerating installed apps is `paper_packages::Inventory`'s job, and
/// a host-side concern (which storage layout, which catalog) Home itself has
/// no business knowing. Whoever launches Home builds the entry list and hands
/// it over.
///
/// The battery fact is different: it needs no grant (WWW-50, ADR-0028), so
/// Home asks for it itself, once, on its first frame, and updates the fact in
/// place whenever the host pushes a change.
#[derive(Debug)]
pub struct HomeApp {
    screen: HomeScreen,
    asked_battery: bool,
}

impl HomeApp {
    /// Builds Home showing `screen` as its starting state.
    pub fn new(screen: HomeScreen) -> Self {
        Self {
            screen,
            asked_battery: false,
        }
    }

    fn apply_battery(&mut self, fact: BatteryFact) {
        let charging = matches!(fact.state, BatteryState::Charging | BatteryState::Full);
        self.screen.set_fact(
            BATTERY_FACT,
            format!(
                "{}%{}",
                fact.percent,
                if charging { " (charging)" } else { "" }
            ),
        );
    }
}

impl App for HomeApp {
    // Home launches other apps; it does not do anything itself that outlives
    // one callback.
    type Completion = Infallible;

    fn event(
        &mut self,
        event: &Event<Self::Completion>,
        context: &mut Context<'_, Self::Completion>,
    ) -> Action {
        match event {
            Event::Pointer(pointer) => {
                // Pure and cheap: no canvas, no drawn frame to wait for. A
                // tap is hit-testable the instant Home exists, not only
                // after the first `draw` — see `crate::screen::layout`'s own
                // doc.
                let layout = crate::screen::layout(context.viewport(), &self.screen);
                self.screen.press(&layout, pointer)
            }
            Event::System(answer) => match &answer.result {
                Ok(SystemValue::Battery(fact)) => {
                    self.apply_battery(*fact);
                    Action::Redraw
                }
                _ => Action::None,
            },
            Event::SystemChanged(SystemEvent::Battery(fact)) => {
                self.apply_battery(*fact);
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    fn draw(&mut self, canvas: &mut Canvas, context: &mut Context<'_, Self::Completion>) {
        if !self.asked_battery {
            self.asked_battery = true;
            context.query_system(SystemQueryKind::Battery);
        }
        crate::screen::render(canvas, &self.screen);
    }

    fn save(&mut self, _context: &mut Context<'_, Self::Completion>) -> Result<(), SaveError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use paper_sdk::{BatteryFact, BatteryState};

    use super::{BATTERY_FACT, HomeApp};
    use crate::screen::HomeScreen;

    fn app() -> HomeApp {
        HomeApp::new(HomeScreen::default())
    }

    #[test]
    fn a_discharging_battery_shows_only_the_percentage() {
        let mut app = app();
        app.apply_battery(BatteryFact {
            percent: 62,
            state: BatteryState::Discharging,
        });
        let fact = app
            .screen
            .facts
            .iter()
            .find(|fact| fact.label == BATTERY_FACT)
            .expect("the fact was filed");
        assert_eq!(fact.value, "62%");
    }

    #[test]
    fn a_charging_battery_says_so() {
        let mut app = app();
        app.apply_battery(BatteryFact {
            percent: 40,
            state: BatteryState::Charging,
        });
        let fact = app
            .screen
            .facts
            .iter()
            .find(|fact| fact.label == BATTERY_FACT)
            .expect("the fact was filed");
        assert_eq!(fact.value, "40% (charging)");
    }
}
