//! Composing the Counter screen, and the interaction on top of it.

use paper_sdk::chrome;
use paper_sdk::{Action, Canvas, Point, PointerEvent, Rect, Size, TextStyle, palette};

/// What the Counter screen is showing. The whole of this app's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CounterScreen {
    /// How many times COUNT has been tapped.
    pub count: u32,
}

impl CounterScreen {
    /// Handles a tap against the layout a draw already produced, and says
    /// what the app should ask the platform for.
    ///
    /// Takes no [`paper_sdk::Context`]: nothing here needs one, which is
    /// what lets this module's own tests call it directly.
    pub fn press(&mut self, layout: &CounterLayout, pointer: &PointerEvent) -> Action {
        if !pointer.is_tap() {
            return Action::None;
        }
        if layout.home.contains(pointer.at) {
            return Action::Home;
        }
        if layout.count.contains(pointer.at) {
            self.count += 1;
            return Action::Redraw;
        }
        Action::None
    }
}

/// Where everything on the Counter screen was drawn, so a caller can hit-test
/// a press against exactly what was on the glass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CounterLayout {
    /// The COUNT button — tapping it increments [`CounterScreen::count`].
    pub count: Rect,
    /// The HOME footer action.
    pub home: Rect,
}

/// Where everything on the Counter screen would land for a viewport shaped
/// like `bounds` — no [`Canvas`], because nothing about this screen's
/// geometry depends on [`CounterScreen::count`] or on having drawn a frame.
///
/// [`crate::CounterApp`] calls this directly from
/// [`event`](paper_sdk::App::event), so a tap is hit-testable before the
/// first [`draw`] ever runs, rather than only after a frame has been drawn
/// to cache it from — the bug WWW-51 removed from five other apps that each
/// discovered it independently.
pub fn layout(bounds: Size) -> CounterLayout {
    let bounds = Rect::new(0.0, 0.0, bounds.width as f32, bounds.height as f32);
    let content = chrome::status_bar_content_area(bounds);
    let count = Rect::new(
        bounds.width / 2.0 - 220.0,
        content.y + (content.height - chrome::FOOTER_HEIGHT - 240.0) / 2.0,
        440.0,
        240.0,
    );
    let actions = chrome::footer_action_rects(bounds, 1);
    CounterLayout {
        count,
        home: actions[0],
    }
}

/// Draws the Counter screen against a layout [`layout`] already computed.
pub fn draw(canvas: &mut Canvas, screen: &CounterScreen, layout: &CounterLayout) {
    canvas.clear(palette::PAPER);
    chrome::draw_status_bar(canvas, "COUNTER", "PAPERCLIP");

    canvas.fill_round_rect(layout.count, 24.0, palette::TILE);
    canvas.stroke_round_rect(layout.count, 24.0, palette::INK, 4.0);
    canvas.draw_text(
        &screen.count.to_string(),
        Point::new(layout.count.center().x, layout.count.center().y - 70.0),
        TextStyle::new(140.0, palette::INK)
            .with_weight(0.16)
            .centered(),
    );
    canvas.draw_text(
        "TAP TO COUNT",
        Point::new(layout.count.center().x, layout.count.bottom() + 40.0),
        TextStyle::new(28.0, palette::INK_SOFT)
            .with_tracking(0.16)
            .centered(),
    );

    chrome::draw_footer_actions(canvas, &["HOME"]);
}

/// Computes the layout and draws it, for callers that want both — every
/// caller in this crate, and [`examples/preview.rs`](../examples/preview.rs)
/// once a frame actually needs to reach the glass.
pub fn render(canvas: &mut Canvas, screen: &CounterScreen) -> CounterLayout {
    let bounds = canvas.bounds();
    let layout = self::layout(Size::new(bounds.width as u32, bounds.height as u32));
    draw(canvas, screen, &layout);
    layout
}

#[cfg(test)]
mod tests {
    use super::{CounterScreen, layout, render};
    use paper_sdk::{Action, Canvas, ContactId, Pointer, PointerEvent, PointerPhase, SCREEN};

    fn canvas() -> Canvas {
        Canvas::new(SCREEN).expect("screen-sized canvas")
    }

    fn tap(at: paper_sdk::Point) -> PointerEvent {
        PointerEvent::new(at, PointerPhase::Up, Pointer::Touch, ContactId::FIRST)
    }

    #[test]
    fn layout_needs_no_canvas_and_matches_what_render_draws() {
        let screen = CounterScreen::default();
        let drawn = render(&mut canvas(), &screen);
        assert_eq!(layout(SCREEN), drawn);
    }

    #[test]
    fn tapping_count_increments_it_and_asks_to_redraw() {
        let mut screen = CounterScreen::default();
        let layout = layout(SCREEN);
        assert_eq!(
            screen.press(&layout, &tap(layout.count.center())),
            Action::Redraw
        );
        assert_eq!(screen.count, 1);
    }

    #[test]
    fn tapping_home_asks_to_go_home_without_counting() {
        let mut screen = CounterScreen::default();
        let layout = layout(SCREEN);
        assert_eq!(
            screen.press(&layout, &tap(layout.home.center())),
            Action::Home
        );
        assert_eq!(screen.count, 0);
    }

    #[test]
    fn tapping_neither_button_does_nothing() {
        let mut screen = CounterScreen::default();
        let layout = layout(SCREEN);
        assert_eq!(
            screen.press(&layout, &tap(paper_sdk::Point::new(2.0, 2.0))),
            Action::None
        );
        assert_eq!(screen.count, 0);
    }

    #[test]
    fn a_hover_is_never_treated_as_a_tap() {
        let mut screen = CounterScreen::default();
        let layout = layout(SCREEN);
        let hover = PointerEvent::new(
            layout.count.center(),
            PointerPhase::Hover,
            Pointer::Pen,
            ContactId::FIRST,
        );
        assert_eq!(screen.press(&layout, &hover), Action::None);
        assert_eq!(screen.count, 0);
    }

    #[test]
    fn the_screen_draws_something_without_going_solid() {
        let mut canvas = canvas();
        render(&mut canvas, &CounterScreen::default());
        let coverage = canvas.ink_coverage();
        assert!(coverage > 0.0, "coverage was {coverage}");
        assert!(coverage < 0.5, "coverage was {coverage}");
    }
}
