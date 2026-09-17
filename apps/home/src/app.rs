//! Home as a real [`App`] (§5).
//!
//! Everything with a decision behind it lives on [`HomeScreen::press`], which
//! takes no [`Context`] and is tested there directly. What is here is only
//! the wire adapter: feed pointer events in, draw on request, save nothing.
//! Home has no state worth persisting — the shelf is rebuilt from the catalog
//! on every launch — so [`App::save`] is a no-op.

use std::convert::Infallible;

use paper_sdk::{Action, App, Canvas, Context, Event, SaveError};

use crate::screen::HomeScreen;
use crate::shelf::ShelfLayout;

/// The Home app.
///
/// The shelf's contents are supplied at construction rather than discovered
/// here: enumerating installed apps is `paper_packages::Inventory`'s job, and
/// a host-side concern (which storage layout, which catalog) Home itself has
/// no business knowing. Whoever launches Home builds the entry list and hands
/// it over.
#[derive(Debug)]
pub struct HomeApp {
    screen: HomeScreen,
    layout: Option<ShelfLayout>,
}

impl HomeApp {
    /// Builds Home showing `screen` as its starting state.
    pub fn new(screen: HomeScreen) -> Self {
        Self {
            screen,
            layout: None,
        }
    }
}

impl App for HomeApp {
    // Home launches other apps; it does not do anything itself that outlives
    // one callback.
    type Completion = Infallible;

    fn event(
        &mut self,
        event: &Event<Self::Completion>,
        _context: &mut Context<'_, Self::Completion>,
    ) -> Action {
        let (Event::Pointer(pointer), Some(layout)) = (event, &self.layout) else {
            return Action::None;
        };
        self.screen.press(layout, pointer)
    }

    fn draw(&mut self, canvas: &mut Canvas, _context: &mut Context<'_, Self::Completion>) {
        self.layout = Some(crate::screen::render(canvas, &self.screen));
    }

    fn save(&mut self, _context: &mut Context<'_, Self::Completion>) -> Result<(), SaveError> {
        Ok(())
    }
}
