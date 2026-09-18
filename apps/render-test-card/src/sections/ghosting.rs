//! Section: ghosting under repeated partial refresh.
//!
//! A patch cycled between two fills, next to a control that is drawn once and
//! never touched again. Every tap on CYCLE claims only the patch and its
//! counter as damage — never the whole screen — so a session run through
//! `paperctl run` presents each cycle as `Waveform::INK` / `Refresh::Partial`
//! (`tools/paperctl/src/run.rs`'s interactive loop), with no full flash
//! between cycles. Residue that survives a full flash on section switch, but
//! not a control that was never touched, is ghosting rather than a rendering
//! bug.

use paper_sdk::{Canvas, Color, Point, Rect, TextStyle, palette};

/// What the last press did, and what a fresh launch starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GhostingState {
    cycles: u32,
    on: bool,
}

impl GhostingState {
    /// The fill the cycled patch currently shows.
    fn fill(self) -> Color {
        if self.on {
            palette::EMPHASIS
        } else {
            palette::PAPER
        }
    }
}

/// Where everything in this section was drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GhostingLayout {
    pub(crate) cycled: Rect,
    pub(crate) control: Rect,
    pub(crate) button: Rect,
    pub(crate) counter: Rect,
}

const PATCH_SIZE: f32 = 420.0;
const BUTTON_HEIGHT: f32 = 120.0;

pub(crate) fn render(canvas: &mut Canvas, content: Rect, state: GhostingState) -> GhostingLayout {
    let gap = 80.0;
    let cycled = Rect::new(content.x, content.y, PATCH_SIZE, PATCH_SIZE);
    let control = Rect::new(cycled.right() + gap, content.y, PATCH_SIZE, PATCH_SIZE);

    canvas.fill_rect(cycled, state.fill());
    canvas.stroke_rect(cycled, palette::HAIRLINE, 2.0);
    canvas.fill_rect(control, palette::BOARD_DARK);
    canvas.stroke_rect(control, palette::HAIRLINE, 2.0);

    canvas.draw_text(
        "CYCLED",
        Point::new(cycled.center().x, cycled.bottom() + 12.0),
        TextStyle::new(26.0, palette::INK).centered(),
    );
    canvas.draw_text(
        "CONTROL - DRAWN ONCE",
        Point::new(control.center().x, control.bottom() + 12.0),
        TextStyle::new(26.0, palette::INK).centered(),
    );

    let counter = Rect::new(content.x, cycled.bottom() + 60.0, content.width, 44.0);
    canvas.draw_text(
        &format!("CYCLES: {}", state.cycles),
        Point::new(counter.x, counter.y),
        TextStyle::new(34.0, palette::INK).with_weight(0.12),
    );

    let button = Rect::new(content.x, counter.bottom() + 24.0, 320.0, BUTTON_HEIGHT);
    canvas.fill_round_rect(button, 18.0, palette::EMPHASIS);
    canvas.draw_text(
        "CYCLE",
        Point::new(button.center().x, button.center().y - 17.0),
        TextStyle::new(34.0, palette::PAPER)
            .with_weight(0.13)
            .centered(),
    );

    canvas.draw_text(
        "EACH TAP: ONE PARTIAL-REFRESH PRESENT, NO FULL FLASH (paperctl run)",
        Point::new(content.x, button.bottom() + 32.0),
        TextStyle::new(22.0, palette::INK_FAINT),
    );

    GhostingLayout {
        cycled,
        control,
        button,
        counter,
    }
}

/// Handles a tap. `Some` carries exactly the regions the button press
/// changed; `None` means the tap landed outside the button and nothing moved.
pub(crate) fn press(
    state: &mut GhostingState,
    layout: &GhostingLayout,
    at: Point,
) -> Option<Vec<Rect>> {
    if !layout.button.contains(at) {
        return None;
    }
    state.cycles += 1;
    state.on = !state.on;
    Some(vec![layout.cycled, layout.counter])
}

#[cfg(test)]
mod tests {
    use super::{GhostingState, press, render};
    use paper_sdk::{Canvas, Rect, SCREEN};

    fn canvas() -> Canvas {
        Canvas::new(SCREEN).expect("a canvas")
    }

    #[test]
    fn a_cycle_toggles_the_patch_and_leaves_the_control_alone() {
        let mut canvas = canvas();
        let content = Rect::new(40.0, 40.0, 1500.0, 1000.0);
        let mut state = GhostingState::default();
        let layout = render(&mut canvas, content, state);
        let before_control = canvas.pixel(
            layout.control.center().x as u32,
            layout.control.center().y as u32,
        );

        let changed = press(&mut state, &layout, layout.button.center());
        assert_eq!(changed, Some(vec![layout.cycled, layout.counter]));
        assert_eq!(state.cycles, 1);

        render(&mut canvas, content, state);
        let after_control = canvas.pixel(
            layout.control.center().x as u32,
            layout.control.center().y as u32,
        );
        assert_eq!(before_control, after_control, "the control moved");
    }

    #[test]
    fn a_tap_off_the_button_changes_nothing() {
        let mut canvas = canvas();
        let content = Rect::new(40.0, 40.0, 1500.0, 1000.0);
        let mut state = GhostingState::default();
        let layout = render(&mut canvas, content, state);
        assert_eq!(press(&mut state, &layout, layout.control.center()), None);
        assert_eq!(state.cycles, 0);
    }
}
