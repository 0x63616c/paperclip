//! Composing the home screen.

use paper_sdk::chrome::{self, MARGIN};
use paper_sdk::{Action, Canvas, Point, PointerEvent, PointerPhase, TextStyle, palette};

use crate::shelf::{self, ShelfEntry, ShelfLayout};

/// One line in the system summary under the shelf.
///
/// Deliberately a pair of strings the caller supplies rather than something
/// this crate reads for itself: the home screen should display platform
/// facts, not go looking for them. Whoever knows the truth passes it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemFact {
    /// Left-hand label.
    pub label: String,
    /// Right-hand value.
    pub value: String,
}

impl SystemFact {
    /// Builds a fact.
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// What the home screen is showing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HomeScreen {
    /// The shelf, in display order.
    pub entries: Vec<ShelfEntry>,
    /// Facts listed under the shelf.
    pub facts: Vec<SystemFact>,
    /// Which tile is currently held down.
    pub pressed: Option<usize>,
    /// Text shown at the top right of the status bar.
    pub status: String,
}

impl HomeScreen {
    /// Sets `label`'s value, replacing it if already listed or appending it
    /// if not.
    ///
    /// Still true to the type's own doc: the screen does not go looking for
    /// `label`'s value, it is only told how to file one that arrived from
    /// somewhere else (WWW-50 — an async system-fact answer, in
    /// [`HomeApp`](crate::HomeApp)'s case).
    pub fn set_fact(&mut self, label: &str, value: impl Into<String>) {
        let value = value.into();
        if let Some(fact) = self.facts.iter_mut().find(|fact| fact.label == label) {
            fact.value = value;
        } else {
            self.facts.push(SystemFact::new(label, value));
        }
    }

    /// Handles a pointer event against the shelf `layout` a draw already
    /// produced, and says what the app should ask the platform for.
    ///
    /// The only entry with nothing to launch, in v1's shelf, is the handoff
    /// back to stock reMarkable — so a tap that landed on a tile but found no
    /// [`ShelfEntry::launch`] target is read as that (ADR-0018).
    pub fn press(&mut self, layout: &ShelfLayout, pointer: &PointerEvent) -> Action {
        let hit = layout.hit_test(pointer.at);
        match pointer.phase {
            // A hovering pen has not pressed anything; highlighting a tile
            // under it would have the shelf respond to proximity, not a tap.
            PointerPhase::Hover => Action::None,
            PointerPhase::Down | PointerPhase::Moved => {
                if self.pressed == hit {
                    Action::None
                } else {
                    self.pressed = hit;
                    Action::Redraw
                }
            }
            PointerPhase::Cancelled => {
                self.pressed = None;
                Action::Redraw
            }
            PointerPhase::Up => {
                self.pressed = None;
                match hit.and_then(|index| self.entries.get(index)) {
                    Some(entry) => match entry.launch() {
                        Some(id) => Action::Launch(id.clone()),
                        None => Action::ReturnToStock,
                    },
                    None => Action::Redraw,
                }
            }
        }
    }
}

/// Draws the home screen and returns the shelf layout used, so the caller can
/// hit-test presses against exactly what was drawn.
pub fn render(canvas: &mut Canvas, screen: &HomeScreen) -> ShelfLayout {
    let bounds = canvas.bounds();
    canvas.clear(palette::PAPER);
    let content = chrome::draw_status_bar(canvas, "PAPERCLIP", &screen.status);
    let width = bounds.width - MARGIN * 2.0;

    let apps_heading =
        chrome::draw_section_heading(canvas, "APPS", Point::new(MARGIN, content.y + 40.0), width);

    let layout = ShelfLayout::compute(
        shelf::shelf_area(bounds.width, apps_heading + 44.0, 0.0),
        screen.entries.len(),
    );
    for (index, entry) in screen.entries.iter().enumerate() {
        let Some(&tile) = layout.tiles().get(index) else {
            continue;
        };
        shelf::draw_tile(canvas, tile, entry, screen.pressed == Some(index));
    }

    if !screen.facts.is_empty() {
        // Anchored to the bottom rather than left to float under the shelf:
        // the shelf grows a row at a time, and a summary that drifts halfway
        // up the page whenever an app is installed reads as an accident.
        // Falls back to flowing under the shelf once the shelf is tall enough
        // to need the room.
        let anchored = bounds.height
            - FOOTER_TEXT_SPACE
            - screen.facts.len() as f32 * FACT_ROW_HEIGHT
            - SECTION_HEADING_SPACE;
        let heading_top = anchored.max(layout.bottom() + 72.0);
        let system_heading =
            chrome::draw_section_heading(canvas, "SYSTEM", Point::new(MARGIN, heading_top), width);
        draw_facts(canvas, &screen.facts, system_heading + 28.0, width);
    }

    canvas.draw_text(
        "PAPERCTL \u{00B7} DESKTOP PREVIEW \u{00B7} NOT DEVICE VERIFIED",
        Point::new(bounds.width / 2.0, bounds.height - 76.0),
        TextStyle::new(24.0, palette::INK_FAINT)
            .with_tracking(0.26)
            .centered(),
    );

    layout
}

/// Height of one fact row.
const FACT_ROW_HEIGHT: f32 = 78.0;

/// Room a section heading takes, including the gap under its rule.
const SECTION_HEADING_SPACE: f32 = 80.0;

/// Room kept clear at the bottom of the screen for the footer line.
const FOOTER_TEXT_SPACE: f32 = 140.0;

fn draw_facts(canvas: &mut Canvas, facts: &[SystemFact], top: f32, width: f32) {
    const ROW_HEIGHT: f32 = FACT_ROW_HEIGHT;
    let label_style = TextStyle::new(30.0, palette::INK_SOFT).with_tracking(0.12);
    let value_style = TextStyle::new(30.0, palette::INK)
        .with_weight(0.11)
        .with_tracking(0.08)
        .right_aligned();

    for (index, fact) in facts.iter().enumerate() {
        let y = top + index as f32 * ROW_HEIGHT;
        canvas.draw_text(&fact.label, Point::new(MARGIN, y), label_style);
        canvas.draw_text(&fact.value, Point::new(MARGIN + width, y), value_style);
        canvas.hairline(
            Point::new(MARGIN, y + ROW_HEIGHT - 26.0),
            width,
            palette::HAIRLINE,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{HomeScreen, SystemFact, render};
    use crate::shelf::{ShelfEntry, ShelfGlyph};
    use paper_sdk::{Canvas, Point, SCREEN};

    fn screen() -> HomeScreen {
        HomeScreen {
            entries: vec![
                ShelfEntry::action("Chess", "V0.1.0", ShelfGlyph::Board),
                ShelfEntry::action("App Store", "V0.1.0", ShelfGlyph::Store),
                ShelfEntry::action("Return to stock", "reMarkable", ShelfGlyph::Stock),
            ],
            facts: vec![
                SystemFact::new("Display", "1620 x 2160"),
                SystemFact::new("Renderer", "Software"),
            ],
            pressed: None,
            status: "0.1.0".to_owned(),
        }
    }

    fn canvas() -> Canvas {
        Canvas::new(SCREEN).expect("screen-sized canvas")
    }

    #[test]
    fn set_fact_replaces_an_existing_label_rather_than_duplicating_it() {
        let mut screen = screen();
        screen.set_fact("Renderer", "GPU");
        assert_eq!(
            screen.facts,
            vec![
                SystemFact::new("Display", "1620 x 2160"),
                SystemFact::new("Renderer", "GPU"),
            ]
        );
    }

    #[test]
    fn set_fact_appends_a_label_that_was_not_already_listed() {
        let mut screen = screen();
        screen.set_fact("Battery", "87%");
        assert_eq!(
            screen.facts.last(),
            Some(&SystemFact::new("Battery", "87%"))
        );
    }

    #[test]
    fn the_shelf_fits_on_the_screen() {
        let mut canvas = canvas();
        let layout = render(&mut canvas, &screen());
        assert_eq!(layout.tiles().len(), 3);
        for tile in layout.tiles() {
            assert!(tile.x >= 0.0);
            assert!(tile.right() <= SCREEN.width as f32);
            assert!(tile.y >= paper_sdk::chrome::STATUS_BAR_HEIGHT);
            assert!(tile.bottom() <= SCREEN.height as f32);
        }
    }

    #[test]
    fn the_system_summary_stays_above_the_bottom_of_the_screen() {
        let mut canvas = canvas();
        let mut home = screen();
        home.facts = (0..6)
            .map(|index| SystemFact::new(format!("Fact {index}"), "value"))
            .collect();
        let layout = render(&mut canvas, &home);
        // The summary starts below the shelf, never on top of it, and still
        // clears the footer line at the bottom of the screen.
        let heading_top =
            (SCREEN.height as f32 - 140.0 - 6.0 * 78.0 - 80.0).max(layout.bottom() + 72.0);
        assert!(
            heading_top > layout.bottom(),
            "the summary overlaps the shelf"
        );
        let last_row = heading_top + 80.0 + 6.0 * 78.0;
        assert!(
            last_row < SCREEN.height as f32 - 60.0,
            "the summary ran to {last_row}"
        );
    }

    #[test]
    fn the_screen_draws_something_without_going_solid() {
        let mut canvas = canvas();
        render(&mut canvas, &screen());
        let coverage = canvas.ink_coverage();
        assert!(coverage > 0.05, "coverage was {coverage}");
        assert!(coverage < 0.80, "coverage was {coverage}");
    }

    #[test]
    fn a_pressed_tile_looks_different() {
        let mut idle = canvas();
        let mut held = canvas();
        render(&mut idle, &screen());
        render(
            &mut held,
            &HomeScreen {
                pressed: Some(1),
                ..screen()
            },
        );
        assert_ne!(
            idle.to_png().expect("encodes"),
            held.to_png().expect("encodes")
        );
    }

    #[test]
    fn a_press_maps_back_to_the_tile_it_landed_on() {
        let mut canvas = canvas();
        let layout = render(&mut canvas, &screen());
        for (index, tile) in layout.tiles().iter().enumerate() {
            assert_eq!(layout.hit_test(tile.center()), Some(index));
        }
        assert_eq!(layout.hit_test(Point::new(10.0, 10.0)), None);
    }

    #[test]
    fn an_empty_shelf_still_renders_a_screen() {
        let mut canvas = canvas();
        let layout = render(
            &mut canvas,
            &HomeScreen {
                entries: Vec::new(),
                facts: Vec::new(),
                pressed: None,
                status: String::new(),
            },
        );
        assert!(layout.tiles().is_empty());
        assert!(canvas.ink_coverage() > 0.0, "the chrome still draws");
    }

    fn press_screen() -> HomeScreen {
        let manifest = paper_packages::Manifest::parse(
            "[app]\nid = \"dev.calum.chess\"\nname = \"Chess\"\nversion = \"0.1.0\"\n\
             protocol = \"1.0\"\nentrypoint = \"bin/chess\"\n",
        )
        .expect("a valid manifest");
        HomeScreen {
            entries: vec![
                ShelfEntry::from_manifest(&manifest, ShelfGlyph::Board),
                ShelfEntry::action("Return to stock", "reMarkable", ShelfGlyph::Stock),
            ],
            facts: Vec::new(),
            pressed: None,
            status: String::new(),
        }
    }

    fn tap(at: Point) -> paper_sdk::PointerEvent {
        paper_sdk::PointerEvent::new(
            at,
            paper_sdk::PointerPhase::Up,
            paper_sdk::Pointer::Touch,
            paper_sdk::ContactId::FIRST,
        )
    }

    #[test]
    fn tapping_an_app_tile_asks_to_launch_it() {
        let mut canvas = canvas();
        let mut screen = press_screen();
        let layout = render(&mut canvas, &screen);
        let tile = layout.tiles()[0].center();
        assert_eq!(
            screen.press(&layout, &tap(tile)),
            paper_sdk::Action::Launch("dev.calum.chess".parse().unwrap())
        );
    }

    #[test]
    fn tapping_the_stock_tile_asks_to_return_to_stock() {
        let mut canvas = canvas();
        let mut screen = press_screen();
        let layout = render(&mut canvas, &screen);
        let tile = layout.tiles()[1].center();
        assert_eq!(
            screen.press(&layout, &tap(tile)),
            paper_sdk::Action::ReturnToStock
        );
    }

    #[test]
    fn tapping_off_every_tile_just_redraws() {
        let mut canvas = canvas();
        let mut screen = press_screen();
        let layout = render(&mut canvas, &screen);
        assert_eq!(
            screen.press(&layout, &tap(Point::new(0.0, 0.0))),
            paper_sdk::Action::Redraw
        );
    }

    #[test]
    fn a_press_highlights_the_tile_it_landed_on_until_release() {
        let mut canvas = canvas();
        let mut screen = press_screen();
        let layout = render(&mut canvas, &screen);
        let tile = layout.tiles()[0].center();
        let down = paper_sdk::PointerEvent::new(
            tile,
            paper_sdk::PointerPhase::Down,
            paper_sdk::Pointer::Touch,
            paper_sdk::ContactId::FIRST,
        );
        assert_eq!(screen.press(&layout, &down), paper_sdk::Action::Redraw);
        assert_eq!(screen.pressed, Some(0));
    }

    #[test]
    fn a_cancelled_contact_clears_the_highlight_without_launching_anything() {
        let mut canvas = canvas();
        let mut screen = press_screen();
        let layout = render(&mut canvas, &screen);
        screen.pressed = Some(0);
        let cancelled = paper_sdk::PointerEvent::new(
            layout.tiles()[0].center(),
            paper_sdk::PointerPhase::Cancelled,
            paper_sdk::Pointer::Touch,
            paper_sdk::ContactId::FIRST,
        );
        assert_eq!(screen.press(&layout, &cancelled), paper_sdk::Action::Redraw);
        assert_eq!(screen.pressed, None);
    }
}
