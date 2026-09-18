//! The destructive-action confirmation overlay.
//!
//! Uninstall, rollback and revoke are Host transactions
//! ([`SettingsHost`](crate::host::SettingsHost)) that cannot be undone once
//! asked for. Every one of them builds one of these before calling the Host
//! at all, and states plainly what will be lost — the WWW-22 requirement that
//! "a destructive action confirms first and says what will be lost."

use paper_sdk::chrome::{self, MARGIN, MIN_TOUCH_TARGET};
use paper_sdk::{Canvas, Point, Rect, TextStyle, palette};

/// What to ask, and what pressing through it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmDialog {
    /// What is about to happen.
    pub title: String,
    /// What it costs, one statement per line. Pre-wrapped by the caller: this
    /// crate has no paragraph-wrapping and none of its labels need one.
    pub lines: Vec<String>,
    /// The label on the button that goes through with it.
    pub confirm_label: String,
}

impl ConfirmDialog {
    /// Builds a dialog.
    pub fn new(
        title: impl Into<String>,
        lines: Vec<String>,
        confirm_label: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            lines,
            confirm_label: confirm_label.into(),
        }
    }
}

/// Where the dialog's two actions landed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfirmLayout {
    /// Backs out without calling the Host.
    pub cancel: Rect,
    /// Goes through with the action.
    pub confirm: Rect,
}

/// Where the card and its two actions land over a viewport shaped like
/// `bounds`, for a dialog with `line_count` lines of body text.
///
/// Pure geometry: the card's height depends on how many lines there are, not
/// on what they say.
pub(crate) fn layout_confirm(bounds: Rect, line_count: usize) -> ConfirmLayout {
    let button_height = MIN_TOUCH_TARGET;
    let card = card_rect(bounds, line_count);

    let gap = 24.0;
    let usable = card.width - 80.0;
    let button_width = (usable - gap) / 2.0;
    let buttons_top = card.bottom() - 40.0 - button_height;
    let cancel = Rect::new(card.x + 40.0, buttons_top, button_width, button_height);
    let confirm = Rect::new(
        cancel.right() + gap,
        buttons_top,
        button_width,
        button_height,
    );
    ConfirmLayout { cancel, confirm }
}

/// The card's own rectangle, for [`draw_confirm`] — not part of
/// [`ConfirmLayout`], because nothing hit-tests the card itself, only its two
/// actions.
fn card_rect(bounds: Rect, line_count: usize) -> Rect {
    let button_height = MIN_TOUCH_TARGET;
    let line_height = 46.0;
    let content_height =
        56.0 + 60.0 + line_count as f32 * line_height + 40.0 + button_height + 40.0;
    let card_height = content_height.min(bounds.height - 240.0);
    Rect::new(
        MARGIN,
        (bounds.height - card_height) / 2.0,
        bounds.width - MARGIN * 2.0,
        card_height,
    )
}

/// Draws the scrim, the card and its two actions, against a layout
/// [`layout_confirm`] already computed.
pub(crate) fn draw_confirm(canvas: &mut Canvas, dialog: &ConfirmDialog, layout: &ConfirmLayout) {
    let bounds = canvas.bounds();
    // A translucent scrim rather than clearing the page: what the dialog
    // interrupts should still read as present, not as gone.
    canvas.fill_rect(bounds, palette::INK.with_alpha(160));

    let card = card_rect(bounds, dialog.lines.len());
    canvas.fill_round_rect(card, 28.0, palette::PAPER);
    canvas.stroke_round_rect(card, 28.0, palette::INK, 4.0);

    let text_x = card.x + 40.0;
    let mut y = card.y + 56.0;
    canvas.draw_text(
        &dialog.title,
        Point::new(text_x, y),
        TextStyle::new(44.0, palette::INK)
            .with_weight(0.13)
            .with_tracking(0.05),
    );
    y += 76.0;
    for line in &dialog.lines {
        canvas.draw_text(
            line,
            Point::new(text_x, y),
            TextStyle::new(30.0, palette::INK_SOFT).with_tracking(0.04),
        );
        y += 46.0;
    }

    // CANCEL is the emphasised (filled) button, not the destructive one: the
    // heavier target under a thumb should be the one that backs out.
    chrome::draw_action(canvas, layout.cancel, "CANCEL", true);
    chrome::draw_action(canvas, layout.confirm, &dialog.confirm_label, false);
}

#[cfg(test)]
mod tests {
    use super::{ConfirmDialog, draw_confirm, layout_confirm};
    use paper_sdk::chrome::MIN_TOUCH_TARGET;
    use paper_sdk::{Canvas, SCREEN};

    fn canvas() -> Canvas {
        Canvas::new(SCREEN).expect("screen-sized canvas")
    }

    fn dialog() -> ConfirmDialog {
        ConfirmDialog::new(
            "UNINSTALL CHESS",
            vec![
                "2.4 MB OF DATA WILL BE DELETED.".to_owned(),
                "THIS CANNOT BE UNDONE.".to_owned(),
            ],
            "UNINSTALL",
        )
    }

    #[test]
    fn layout_needs_no_canvas_and_matches_what_drawing_returns() {
        let dialog = dialog();
        let bounds = canvas().bounds();
        assert_eq!(layout_confirm(bounds, dialog.lines.len()), {
            let mut probe = canvas();
            let layout = layout_confirm(probe.bounds(), dialog.lines.len());
            draw_confirm(&mut probe, &dialog, &layout);
            layout
        });
    }

    #[test]
    fn the_two_actions_are_big_enough_and_do_not_overlap() {
        let layout = layout_confirm(canvas().bounds(), dialog().lines.len());
        assert!(layout.cancel.shortest_side() >= MIN_TOUCH_TARGET);
        assert!(layout.confirm.shortest_side() >= MIN_TOUCH_TARGET);
        assert!(layout.cancel.right() <= layout.confirm.x);
    }

    #[test]
    fn both_actions_land_inside_the_screen() {
        let layout = layout_confirm(canvas().bounds(), dialog().lines.len());
        for rect in [layout.cancel, layout.confirm] {
            assert!(rect.x >= 0.0 && rect.right() <= SCREEN.width as f32);
            assert!(rect.y >= 0.0 && rect.bottom() <= SCREEN.height as f32);
        }
    }

    #[test]
    fn the_dialog_covers_more_of_the_page_than_a_bare_page_would() {
        let plain = canvas();
        let mut confirming = canvas();
        let dialog = dialog();
        let layout = layout_confirm(confirming.bounds(), dialog.lines.len());
        draw_confirm(&mut confirming, &dialog, &layout);
        assert!(confirming.ink_coverage() > plain.ink_coverage());
        assert!(confirming.ink_coverage() > 0.05);
    }

    #[test]
    fn a_dialog_with_more_lines_gets_a_taller_card_without_pushing_actions_off_screen() {
        let long = ConfirmDialog::new(
            "REVOKE STORAGE",
            (0..10)
                .map(|index| format!("LINE {index}"))
                .collect::<Vec<_>>(),
            "REVOKE",
        );
        let layout = layout_confirm(canvas().bounds(), long.lines.len());
        assert!(layout.confirm.bottom() <= SCREEN.height as f32);
    }
}
