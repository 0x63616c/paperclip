//! Drawing the App Store, and handing back what was drawn where.
//!
//! Every draw function returns the rectangles it used, so a press is
//! hit-tested against exactly what is on the glass rather than against a
//! second copy of the geometry that can drift from it. Nothing here reads
//! state that is not already in the screen, and nothing here performs I/O
//! (§8: `draw` does no work).

use paper_packages::AppId;
use paper_packages::inventory::{AppEntry, AppState};
use paper_sdk::{Canvas, Point, Rect, TextStyle, chrome, palette};

use crate::screen::{AppStoreScreen, View};

/// Height of one app row.
const ROW_HEIGHT: f32 = 184.0;
/// Vertical gap between rows.
const ROW_GAP: f32 = 20.0;
/// Width of a row's action button.
const BUTTON_WIDTH: f32 = 248.0;
/// Height of a row's action button, at least one touch target.
const BUTTON_HEIGHT: f32 = 128.0;

/// Where the list view put everything interactive.
#[derive(Debug, Clone, PartialEq)]
pub struct ListLayout {
    /// One entry per drawn row, in display order.
    pub rows: Vec<RowLayout>,
    /// The refresh button.
    pub refresh: Rect,
}

impl ListLayout {
    /// Which row's body a press landed on, for opening its detail view.
    pub fn row_hit(&self, at: Point) -> Option<usize> {
        self.rows.iter().position(|row| row.body.contains(at))
    }

    /// Which row's action button a press landed on.
    pub fn action_hit(&self, at: Point) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.action.is_some_and(|rect| rect.contains(at)))
    }
}

/// Where one row's pieces landed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowLayout {
    /// The whole row, which opens the detail view.
    pub body: Rect,
    /// The action button, absent when the row has no action.
    pub action: Option<Rect>,
}

/// Where the detail view put everything interactive.
#[derive(Debug, Clone, PartialEq)]
pub struct DetailLayout {
    /// Back to the list.
    pub back: Rect,
    /// Install or update, absent when there is nothing to install.
    pub primary: Option<Rect>,
    /// Roll back, absent when there is nowhere to go.
    pub rollback: Option<Rect>,
}

/// Where whichever view is showing put everything interactive.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreLayout {
    /// The list.
    List(ListLayout),
    /// One app.
    Detail(DetailLayout),
}

/// Draws whichever view the screen is showing.
pub fn render(canvas: &mut Canvas, screen: &AppStoreScreen) -> StoreLayout {
    canvas.clear(palette::PAPER);
    // `draw_status_bar` returns the content rectangle *below* the bar, not the
    // bar itself. Treating it as the bar pushes every row off the bottom of
    // the panel, which is a thing that looks like "no apps installed".
    let content = chrome::draw_status_bar(canvas, "APP STORE", &screen.catalog_summary());
    let area = content_area(content);

    match screen.view() {
        View::List => StoreLayout::List(draw_list(canvas, area, screen)),
        View::Detail(app) => StoreLayout::Detail(draw_detail(canvas, area, screen, app)),
    }
}

/// The drawable area, inside the margins.
fn content_area(content: Rect) -> Rect {
    Rect::new(
        content.x + chrome::MARGIN,
        content.y + 32.0,
        content.width - chrome::MARGIN * 2.0,
        content.height - 32.0 - chrome::MARGIN,
    )
}

fn draw_list(canvas: &mut Canvas, area: Rect, screen: &AppStoreScreen) -> ListLayout {
    let mut cursor = area.y;

    if let Some(banner) = banner_text(screen) {
        cursor = draw_banner(canvas, area, cursor, &banner);
    }

    let refresh = Rect::new(
        area.x + area.width - BUTTON_WIDTH,
        area.y + area.height - BUTTON_HEIGHT,
        BUTTON_WIDTH,
        BUTTON_HEIGHT,
    );

    if screen.rows().is_empty() {
        canvas.draw_text(
            "NOTHING INSTALLED, AND THE CATALOG OFFERS NOTHING.",
            Point::new(area.x, cursor + 8.0),
            TextStyle::new(30.0, palette::INK_SOFT),
        );
        chrome::draw_action(canvas, refresh, "REFRESH", false);
        return ListLayout {
            rows: Vec::new(),
            refresh,
        };
    }

    let mut rows = Vec::with_capacity(screen.rows().len());
    for entry in screen.rows() {
        let body = Rect::new(area.x, cursor, area.width, ROW_HEIGHT);
        // Stop before the row would collide with the refresh button.
        if body.y + body.height > refresh.y - ROW_GAP {
            break;
        }
        rows.push(draw_row(canvas, body, screen, entry));
        cursor += ROW_HEIGHT + ROW_GAP;
    }

    chrome::draw_action(canvas, refresh, "REFRESH", false);
    ListLayout { rows, refresh }
}

fn draw_row(
    canvas: &mut Canvas,
    body: Rect,
    screen: &AppStoreScreen,
    entry: &AppEntry,
) -> RowLayout {
    canvas.fill_round_rect(body, 20.0, palette::TILE);
    canvas.stroke_round_rect(body, 20.0, palette::HAIRLINE, 2.0);

    canvas.draw_text(
        &entry.name.as_str().to_uppercase(),
        Point::new(body.x + 32.0, body.y + 32.0),
        TextStyle::new(38.0, palette::INK)
            .with_weight(0.12)
            .with_tracking(0.06),
    );
    canvas.draw_text(
        &versions_line(entry),
        Point::new(body.x + 32.0, body.y + 88.0),
        TextStyle::new(28.0, palette::INK_SOFT).with_tracking(0.04),
    );
    canvas.draw_text(
        &entry.state.label().to_uppercase(),
        Point::new(body.x + 32.0, body.y + 134.0),
        TextStyle::new(24.0, state_ink(entry.state)).with_tracking(0.08),
    );

    // The row in flight shows its step where its button would be, so progress
    // appears next to the thing it is about rather than in a corner.
    if let Some(working) = screen.working()
        && working.app == entry.app
    {
        let where_button_was = Rect::new(
            body.x + body.width - BUTTON_WIDTH - 24.0,
            body.y + (body.height - BUTTON_HEIGHT) / 2.0,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        );
        draw_progress(canvas, where_button_was, &working.step, working.percent);
        return RowLayout { body, action: None };
    }

    let action = screen.primary_label(&entry.app).map(|label| {
        let rect = Rect::new(
            body.x + body.width - BUTTON_WIDTH - 24.0,
            body.y + (body.height - BUTTON_HEIGHT) / 2.0,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        );
        chrome::draw_action(canvas, rect, label, entry.state == AppState::Failing);
        rect
    });

    RowLayout { body, action }
}

fn draw_detail(
    canvas: &mut Canvas,
    area: Rect,
    screen: &AppStoreScreen,
    app: &AppId,
) -> DetailLayout {
    let back = Rect::new(area.x, area.y, BUTTON_WIDTH, BUTTON_HEIGHT);
    chrome::draw_action(canvas, back, "BACK", false);

    let Some(entry) = screen.detail() else {
        canvas.draw_text(
            "THAT APP IS NO LONGER HERE.",
            Point::new(area.x, back.y + BUTTON_HEIGHT + 48.0),
            TextStyle::new(30.0, palette::INK_SOFT),
        );
        return DetailLayout {
            back,
            primary: None,
            rollback: None,
        };
    };

    let mut cursor = back.y + BUTTON_HEIGHT + 56.0;
    canvas.draw_text(
        &entry.name.as_str().to_uppercase(),
        Point::new(area.x, cursor),
        TextStyle::new(52.0, palette::INK)
            .with_weight(0.14)
            .with_tracking(0.05),
    );
    cursor += 76.0;

    canvas.draw_text(
        entry.app.as_str(),
        Point::new(area.x, cursor),
        TextStyle::new(26.0, palette::INK_FAINT).with_tracking(0.04),
    );
    cursor += 56.0;

    canvas.draw_text(
        &versions_line(entry),
        Point::new(area.x, cursor),
        TextStyle::new(30.0, palette::INK_SOFT).with_tracking(0.04),
    );
    cursor += 44.0;

    if let Some(prerelease) = &entry.available_prerelease {
        canvas.draw_text(
            &format!("PRERELEASE {prerelease} \u{00B7} NOT INSTALLED AUTOMATICALLY"),
            Point::new(area.x, cursor),
            TextStyle::new(24.0, palette::INK_FAINT).with_tracking(0.04),
        );
        cursor += 44.0;
    }

    if let Some(health) = &entry.health
        && !health.started
        && health.attempts > 0
    {
        canvas.draw_text(
            &format!(
                "{} ATTEMPT(S) WITHOUT A START{}",
                health.attempts,
                health
                    .last_failure
                    .as_deref()
                    .map_or_else(String::new, |note| format!(": {}", note.to_uppercase()))
            ),
            Point::new(area.x, cursor),
            TextStyle::new(24.0, palette::INK).with_tracking(0.04),
        );
        cursor += 52.0;
    }

    cursor += 16.0;
    cursor = chrome::draw_section_heading(
        canvas,
        "RELEASE NOTES",
        Point::new(area.x, cursor),
        area.width,
    );
    match screen.notes() {
        Some(notes) if !notes.trim().is_empty() => {
            draw_wrapped(canvas, area.x, cursor, area.width, notes);
        }
        Some(_) => {
            canvas.draw_text(
                "THIS RELEASE SHIPPED WITHOUT NOTES.",
                Point::new(area.x, cursor),
                TextStyle::new(26.0, palette::INK_FAINT),
            );
        }
        None => {
            canvas.draw_text(
                if entry.available.is_some() {
                    "FETCHING NOTES\u{2026}"
                } else {
                    "THE CATALOG DOES NOT OFFER THIS APP."
                },
                Point::new(area.x, cursor),
                TextStyle::new(26.0, palette::INK_FAINT),
            );
        }
    }

    let row = area.y + area.height - BUTTON_HEIGHT;
    if let Some(failure) = screen.failure() {
        draw_failure(canvas, area, row, failure);
    }

    if let Some(working) = screen.working()
        && &working.app == app
    {
        let rect = Rect::new(area.x, row, BUTTON_WIDTH * 2.0, BUTTON_HEIGHT);
        draw_progress(canvas, rect, &working.step, working.percent);
        return DetailLayout {
            back,
            primary: None,
            rollback: None,
        };
    }

    let primary = screen.primary_label(app).map(|label| {
        let rect = Rect::new(area.x, row, BUTTON_WIDTH, BUTTON_HEIGHT);
        chrome::draw_action(canvas, rect, label, true);
        rect
    });
    let rollback = screen.can_roll_back(app).then(|| {
        let rect = Rect::new(
            area.x + BUTTON_WIDTH + 24.0,
            row,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        );
        chrome::draw_action(canvas, rect, "ROLL BACK", false);
        rect
    });

    DetailLayout {
        back,
        primary,
        rollback,
    }
}

/// Draws a progress box where a button would be.
fn draw_progress(canvas: &mut Canvas, rect: Rect, step: &str, percent: Option<u8>) {
    canvas.fill_round_rect(rect, 16.0, palette::PAPER);
    canvas.stroke_round_rect(rect, 16.0, palette::INK_FAINT, 2.0);
    let style = chrome::fit_text(
        step,
        TextStyle::new(22.0, palette::INK).with_tracking(0.06),
        rect.width - 24.0,
        14.0,
    );
    canvas.draw_text(step, Point::new(rect.x + 16.0, rect.y + 24.0), style);

    // Only a real fraction draws a bar. A download has one; committing does
    // not, and a bar that moves for a step with no measure is a lie.
    if let Some(percent) = percent {
        let track = Rect::new(
            rect.x + 16.0,
            rect.y + rect.height - 40.0,
            rect.width - 32.0,
            16.0,
        );
        canvas.fill_round_rect(track, 8.0, palette::TILE);
        let filled = track.width * f32::from(percent.min(100)) / 100.0;
        if filled > 0.0 {
            canvas.fill_round_rect(
                Rect::new(track.x, track.y, filled, track.height),
                8.0,
                palette::EMPHASIS,
            );
        }
    }
}

fn draw_failure(canvas: &mut Canvas, area: Rect, row: f32, failure: &crate::screen::Failure) {
    let top = row - 140.0;
    canvas.draw_text(
        &failure.message.to_uppercase(),
        Point::new(area.x, top),
        TextStyle::new(26.0, palette::INK).with_tracking(0.04),
    );
    canvas.draw_text(
        &failure.advice.to_uppercase(),
        Point::new(area.x, top + 44.0),
        TextStyle::new(24.0, palette::INK_SOFT).with_tracking(0.04),
    );
}

/// A banner above the list for the one thing that is true of the whole screen.
fn banner_text(screen: &AppStoreScreen) -> Option<String> {
    if let Some(failure) = screen.failure() {
        return Some(format!("{} \u{2014} {}", failure.message, failure.advice));
    }
    screen
        .is_stale()
        .then(|| "THE CATALOG IS NOT REACHABLE. INSTALLED APPS STILL WORK.".to_owned())
}

fn draw_banner(canvas: &mut Canvas, area: Rect, cursor: f32, text: &str) -> f32 {
    let rect = Rect::new(area.x, cursor, area.width, 96.0);
    canvas.fill_round_rect(rect, 16.0, palette::TILE);
    canvas.stroke_round_rect(rect, 16.0, palette::INK_FAINT, 2.0);
    let style = chrome::fit_text(
        text,
        TextStyle::new(24.0, palette::INK).with_tracking(0.04),
        rect.width - 48.0,
        14.0,
    );
    canvas.draw_text(
        &text.to_uppercase(),
        Point::new(rect.x + 24.0, rect.y + 34.0),
        style,
    );
    cursor + rect.height + ROW_GAP
}

/// Draws text across several lines, breaking on words.
fn draw_wrapped(canvas: &mut Canvas, x: f32, top: f32, width: f32, text: &str) {
    const LINE: f32 = 38.0;
    const MAX_LINES: usize = 8;

    let style = TextStyle::new(26.0, palette::INK_SOFT).with_tracking(0.03);
    let mut line = String::new();
    let mut drawn = 0usize;
    let mut y = top;

    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_owned()
        } else {
            format!("{line} {word}")
        };
        if paper_sdk::measure_text(&candidate, style) > width && !line.is_empty() {
            canvas.draw_text(&line, Point::new(x, y), style);
            drawn += 1;
            y += LINE;
            line = word.to_owned();
            if drawn == MAX_LINES {
                // Notes are bounded at 4 KiB, which is far more than fits. The
                // detail view shows the beginning rather than silently drawing
                // off the bottom of the panel.
                canvas.draw_text("\u{2026}", Point::new(x, y), style);
                return;
            }
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() {
        canvas.draw_text(&line, Point::new(x, y), style);
    }
}

fn versions_line(entry: &AppEntry) -> String {
    let installed = entry
        .installed
        .as_ref()
        .map_or_else(|| "NOT INSTALLED".to_owned(), |v| format!("V{v}"));
    match &entry.available {
        Some(available) if entry.installed.as_ref() != Some(available) => {
            format!("{installed} \u{00B7} OFFERED V{available}")
        }
        Some(available) => format!("{installed} \u{00B7} OFFERED V{available}"),
        None => installed,
    }
}

fn state_ink(state: AppState) -> paper_sdk::Color {
    match state {
        AppState::Failing | AppState::NothingSelected => palette::INK,
        _ => palette::INK_FAINT,
    }
}

#[cfg(test)]
mod tests {
    use paper_packages::inventory::Inventory;
    use paper_packages::launch::Ledger;
    use paper_packages::store::Layout;
    use paper_sdk::{Canvas, SCREEN};

    use super::{StoreLayout, render};
    use crate::screen::AppStoreScreen;

    fn screen() -> AppStoreScreen {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let layout = Layout::new(dir.path());
        layout.ensure().expect("an empty store");
        AppStoreScreen::new(
            Inventory::survey(&layout, None, &Ledger::new(layout.clone())).expect("a survey"),
        )
    }

    #[test]
    fn an_empty_store_still_draws_a_usable_screen() {
        let mut canvas = Canvas::new(SCREEN).expect("a canvas");
        let layout = render(&mut canvas, &screen());

        let StoreLayout::List(list) = layout else {
            panic!("an empty store shows the list");
        };
        assert!(list.rows.is_empty());
        // Refresh is always reachable — an empty screen with no way to try
        // again is a dead end.
        assert!(list.refresh.width >= paper_sdk::chrome::MIN_TOUCH_TARGET);
        assert!(list.refresh.height >= paper_sdk::chrome::MIN_TOUCH_TARGET);
        assert!(
            canvas
                .bounds()
                .contains(paper_sdk::Point::new(list.refresh.x, list.refresh.y))
        );
    }

    /// A screen with two apps in it: one to update, one already current.
    fn populated() -> AppStoreScreen {
        use paper_packages::inventory::{AppEntry, CatalogStatus};

        AppStoreScreen::new(Inventory::fixture(
            vec![
                AppEntry::fixture(
                    "dev.calum.chess".parse().expect("a valid id"),
                    "Chess".parse().expect("a valid name"),
                    Some("0.1.0".parse().expect("a valid version")),
                    Some("0.2.0".parse().expect("a valid version")),
                ),
                AppEntry::fixture(
                    "dev.calum.settings".parse().expect("a valid id"),
                    "Settings".parse().expect("a valid name"),
                    Some("0.1.0".parse().expect("a valid version")),
                    Some("0.1.0".parse().expect("a valid version")),
                ),
            ],
            Some(CatalogStatus {
                name: "calum-home".to_owned(),
                serial: 7,
                stale: false,
            }),
        ))
    }

    #[test]
    fn every_row_is_drawn_inside_the_panel() {
        let mut canvas = Canvas::new(SCREEN).expect("a canvas");
        let screen = populated();
        let StoreLayout::List(list) = render(&mut canvas, &screen) else {
            panic!("the list is showing");
        };

        // The bug this test exists for: a content area computed from the wrong
        // rectangle put every row below the bottom of the panel, and the only
        // visible symptom was a screen that looked like it had no apps.
        assert_eq!(
            list.rows.len(),
            screen.rows().len(),
            "every row must fit on the panel"
        );
        let bounds = canvas.bounds();
        for row in &list.rows {
            assert!(
                row.body.y >= bounds.y && row.body.y + row.body.height <= bounds.y + bounds.height,
                "row at {} is off the panel",
                row.body.y
            );
        }
        // The one with an update available has a button; the current one does not.
        assert!(list.rows[0].action.is_some(), "an update needs a button");
        assert!(
            list.rows[1].action.is_none(),
            "nothing to do needs no button"
        );
    }

    #[test]
    fn a_row_and_its_button_are_hit_testable_where_they_were_drawn() {
        let mut canvas = Canvas::new(SCREEN).expect("a canvas");
        let screen = populated();
        let StoreLayout::List(list) = render(&mut canvas, &screen) else {
            panic!("the list is showing");
        };

        let action = list.rows[0].action.expect("an update button");
        assert_eq!(list.action_hit(action.center()), Some(0));
        assert_eq!(list.row_hit(list.rows[1].body.center()), Some(1));
        // A press on the button is on the row too; the caller checks the
        // button first, and this records that the overlap is expected.
        assert_eq!(list.row_hit(action.center()), Some(0));
        assert!(action.width >= paper_sdk::chrome::MIN_TOUCH_TARGET);
        assert!(action.height >= paper_sdk::chrome::MIN_TOUCH_TARGET);
    }

    #[test]
    fn the_detail_view_can_always_get_back() {
        let mut canvas = Canvas::new(SCREEN).expect("a canvas");
        let mut screen = populated();
        screen.open(&"dev.calum.chess".parse().expect("a valid id"));

        let StoreLayout::Detail(detail) = render(&mut canvas, &screen) else {
            panic!("the detail view is showing");
        };
        assert!(canvas.bounds().contains(detail.back.center()));
        assert!(detail.primary.is_some(), "an update is offered");
    }

    #[test]
    fn something_is_actually_drawn() {
        let mut canvas = Canvas::new(SCREEN).expect("a canvas");
        render(&mut canvas, &screen());
        assert!(
            canvas.ink_coverage() > 0.0,
            "an empty store must still draw its chrome"
        );
    }
}
