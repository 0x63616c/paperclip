//! Drawing for each Settings page.
//!
//! Apps and Grants are interactive lists and hand back the rectangles their
//! rows were drawn at, so a caller can hit-test a press against exactly what
//! is on screen. Storage, Catalog, Platform and Diagnostics are read-only
//! (WWW-22 scope) and draw without returning anything to hit-test.

use paper_sdk::chrome;
use paper_sdk::{Canvas, Point, Rect, TextStyle, palette};

use crate::host::{
    CatalogStatus, DiagnosticEntry, GrantSummary, InstalledAppSummary, PlatformInfo, StorageUsage,
};

/// Height of one row in a list page.
const ROW_HEIGHT: f32 = 168.0;
/// Vertical gap between rows.
const ROW_GAP: f32 = 20.0;
/// Width of a row's action button.
const BUTTON_WIDTH: f32 = 220.0;
/// Height of a row's action button.
const BUTTON_HEIGHT: f32 = 128.0;

fn row_rect(area: Rect, index: usize) -> Rect {
    Rect::new(
        area.x,
        area.y + index as f32 * (ROW_HEIGHT + ROW_GAP),
        area.width,
        ROW_HEIGHT,
    )
}

fn draw_row_frame(canvas: &mut Canvas, rect: Rect) {
    canvas.fill_round_rect(rect, 20.0, palette::TILE);
    canvas.stroke_round_rect(rect, 20.0, palette::HAIRLINE, 2.0);
}

fn draw_row_text(canvas: &mut Canvas, rect: Rect, title: &str, detail: &str) {
    canvas.draw_text(
        title,
        Point::new(rect.x + 32.0, rect.y + 34.0),
        TextStyle::new(38.0, palette::INK)
            .with_weight(0.12)
            .with_tracking(0.06),
    );
    canvas.draw_text(
        detail,
        Point::new(rect.x + 32.0, rect.y + 88.0),
        TextStyle::new(28.0, palette::INK_SOFT).with_tracking(0.04),
    );
}

/// Formats a byte count the way the rest of this app reports storage —
/// one decimal place, always megabytes. Nothing here holds enough data for a
/// gigabyte to matter, and a unit that changes per row is harder to compare.
pub(crate) fn format_bytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// Where an apps-page row's buttons landed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AppRowLayout {
    /// Absent when the app has no previous release to roll back to.
    pub rollback: Option<Rect>,
    /// Always present: every installed app can be uninstalled.
    pub uninstall: Rect,
}

/// The interactive layout of the apps page.
#[derive(Debug, Clone, PartialEq)]
pub struct AppsLayout {
    /// One entry per installed app, in display order.
    pub rows: Vec<AppRowLayout>,
}

impl AppsLayout {
    /// Which row's rollback button a press landed on.
    pub fn rollback_hit(&self, at: Point) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.rollback.is_some_and(|rect| rect.contains(at)))
    }

    /// Which row's uninstall button a press landed on.
    pub fn uninstall_hit(&self, at: Point) -> Option<usize> {
        self.rows.iter().position(|row| row.uninstall.contains(at))
    }
}

/// Draws the installed-apps page.
pub(crate) fn draw_apps(
    canvas: &mut Canvas,
    area: Rect,
    apps: &[InstalledAppSummary],
) -> AppsLayout {
    if apps.is_empty() {
        canvas.draw_text(
            "NOTHING INSTALLED FROM THE CATALOG YET.",
            Point::new(area.x, area.y + 8.0),
            TextStyle::new(30.0, palette::INK_SOFT),
        );
        return AppsLayout { rows: Vec::new() };
    }

    let mut rows = Vec::with_capacity(apps.len());
    for (index, app) in apps.iter().enumerate() {
        let rect = row_rect(area, index);
        draw_row_frame(canvas, rect);
        let detail = match &app.previous_version {
            Some(previous) => format!(
                "ACTIVE V{} \u{00B7} PREVIOUS V{}",
                app.active_version, previous
            ),
            None => format!(
                "ACTIVE V{} \u{00B7} NO PREVIOUS RELEASE",
                app.active_version
            ),
        };
        draw_row_text(canvas, rect, &app.name.to_uppercase(), &detail);

        let uninstall = Rect::new(
            rect.right() - 32.0 - BUTTON_WIDTH,
            rect.center().y - BUTTON_HEIGHT / 2.0,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        );
        let rollback = if app.has_previous_release() {
            let rect = Rect::new(
                uninstall.x - 20.0 - BUTTON_WIDTH,
                uninstall.y,
                BUTTON_WIDTH,
                BUTTON_HEIGHT,
            );
            chrome::draw_action(canvas, rect, "ROLLBACK", false);
            Some(rect)
        } else {
            None
        };
        chrome::draw_action(canvas, uninstall, "UNINSTALL", false);
        rows.push(AppRowLayout {
            rollback,
            uninstall,
        });
    }
    AppsLayout { rows }
}

/// The interactive layout of the grants page.
#[derive(Debug, Clone, PartialEq)]
pub struct GrantsLayout {
    /// One revoke button per grant, in display order.
    pub revoke: Vec<Rect>,
}

impl GrantsLayout {
    /// Which grant's revoke button a press landed on.
    pub fn revoke_hit(&self, at: Point) -> Option<usize> {
        self.revoke.iter().position(|rect| rect.contains(at))
    }
}

/// Draws the grants page.
pub(crate) fn draw_grants(
    canvas: &mut Canvas,
    area: Rect,
    grants: &[GrantSummary],
) -> GrantsLayout {
    if grants.is_empty() {
        canvas.draw_text(
            "NO CAPABILITIES ARE GRANTED.",
            Point::new(area.x, area.y + 8.0),
            TextStyle::new(30.0, palette::INK_SOFT),
        );
        return GrantsLayout { revoke: Vec::new() };
    }

    let mut revoke = Vec::with_capacity(grants.len());
    for (index, grant) in grants.iter().enumerate() {
        let rect = row_rect(area, index);
        draw_row_frame(canvas, rect);
        let title = format!(
            "{} \u{00B7} {}",
            grant.app_name.to_uppercase(),
            grant.capability.name().to_uppercase()
        );
        let detail = if grant.in_use {
            "IN USE NOW"
        } else {
            "GRANTED, NOT CURRENTLY IN USE"
        };
        draw_row_text(canvas, rect, &title, detail);

        let button = Rect::new(
            rect.right() - 32.0 - BUTTON_WIDTH,
            rect.center().y - BUTTON_HEIGHT / 2.0,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        );
        chrome::draw_action(canvas, button, "REVOKE", false);
        revoke.push(button);
    }
    GrantsLayout { revoke }
}

/// A read-only label/value row, the shape every non-interactive page uses.
fn draw_facts(canvas: &mut Canvas, area: Rect, facts: &[(&str, String)]) {
    const ROW: f32 = 72.0;
    for (index, (label, value)) in facts.iter().enumerate() {
        let y = area.y + index as f32 * ROW;
        canvas.draw_text(
            label,
            Point::new(area.x, y),
            TextStyle::new(30.0, palette::INK_SOFT).with_tracking(0.1),
        );
        canvas.draw_text(
            value,
            Point::new(area.right(), y),
            TextStyle::new(30.0, palette::INK)
                .with_weight(0.11)
                .with_tracking(0.05)
                .right_aligned(),
        );
        canvas.hairline(
            Point::new(area.x, y + ROW - 20.0),
            area.width,
            palette::HAIRLINE,
        );
    }
}

/// Draws the storage page: a used/free meter, then a bucket-by-bucket
/// breakdown.
pub(crate) fn draw_storage(canvas: &mut Canvas, area: Rect, storage: &StorageUsage) {
    let used = storage.used_bytes();
    let total = used + storage.free_bytes;
    let meter = Rect::new(area.x, area.y, area.width, 56.0);
    canvas.fill_round_rect(meter, 12.0, palette::TILE);
    if total > 0 {
        let filled = meter.width * (used as f32 / total as f32).clamp(0.0, 1.0);
        canvas.fill_round_rect(
            Rect::new(meter.x, meter.y, filled, meter.height),
            12.0,
            palette::INK,
        );
    }
    canvas.stroke_round_rect(meter, 12.0, palette::HAIRLINE, 2.0);
    canvas.draw_text(
        &format!("{} USED OF {}", format_bytes(used), format_bytes(total)),
        Point::new(area.x, meter.bottom() + 40.0),
        TextStyle::new(30.0, palette::INK_SOFT),
    );

    let facts: Vec<(&str, String)> = storage
        .buckets
        .iter()
        .map(|bucket| (bucket.label.as_str(), format_bytes(bucket.bytes)))
        .chain(std::iter::once(("FREE", format_bytes(storage.free_bytes))))
        .collect();
    draw_facts(
        canvas,
        Rect::new(area.x, meter.bottom() + 108.0, area.width, area.height),
        &facts,
    );
}

/// Draws the catalog page.
pub(crate) fn draw_catalog(canvas: &mut Canvas, area: Rect, catalog: &CatalogStatus) {
    let facts = [
        ("ENDPOINT", catalog.endpoint.clone()),
        (
            "LAST SUCCESSFUL FETCH",
            catalog
                .last_fetch
                .clone()
                .unwrap_or_else(|| "NEVER".to_owned()),
        ),
        (
            "REACHABLE NOW",
            if catalog.reachable {
                "YES".to_owned()
            } else {
                "NO".to_owned()
            },
        ),
    ];
    draw_facts(canvas, area, &facts);
}

/// Draws the platform page.
pub(crate) fn draw_platform(canvas: &mut Canvas, area: Rect, platform: &PlatformInfo) {
    let facts = [
        (
            "PAPERCLIP VERSION",
            format!("V{}", platform.paperclip_version),
        ),
        ("FIRMWARE", platform.firmware.clone()),
        ("ACTIVE PLATFORM RELEASE", platform.active_release.clone()),
    ];
    draw_facts(canvas, area, &facts);
}

/// Draws the diagnostics page.
pub(crate) fn draw_diagnostics(canvas: &mut Canvas, area: Rect, entries: &[DiagnosticEntry]) {
    if entries.is_empty() {
        canvas.draw_text(
            "NO ERRORS RECORDED.",
            Point::new(area.x, area.y + 8.0),
            TextStyle::new(30.0, palette::INK_SOFT),
        );
        return;
    }
    const ROW: f32 = 88.0;
    for (index, entry) in entries.iter().enumerate() {
        let y = area.y + index as f32 * ROW;
        canvas.draw_text(
            &entry.when,
            Point::new(area.x, y),
            TextStyle::new(26.0, palette::INK_FAINT).with_tracking(0.05),
        );
        canvas.draw_text(
            &entry.message,
            Point::new(area.x, y + 34.0),
            TextStyle::new(28.0, palette::INK),
        );
    }
    let logs_y = area.y + entries.len() as f32 * ROW + 16.0;
    canvas.draw_text(
        "FULL LOGS: /HOME/ROOT/PAPERCLIP/LOGS",
        Point::new(area.x, logs_y),
        TextStyle::new(26.0, palette::INK_FAINT).with_tracking(0.05),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use paper_packages::Capability;
    use paper_sdk::SCREEN;
    use paper_sdk::chrome::MIN_TOUCH_TARGET;

    fn canvas() -> Canvas {
        Canvas::new(SCREEN).expect("screen-sized canvas")
    }

    fn area() -> Rect {
        Rect::new(56.0, 300.0, 1508.0, 1600.0)
    }

    fn app(previous: Option<&str>) -> InstalledAppSummary {
        InstalledAppSummary {
            id: "dev.calum.chess".parse().unwrap(),
            name: "Chess".to_owned(),
            active_version: "0.2.0".to_owned(),
            previous_version: previous.map(str::to_owned),
            data_bytes: 2_400_000,
        }
    }

    #[test]
    fn a_row_with_a_previous_release_gets_a_rollback_button() {
        let mut canvas = canvas();
        let layout = draw_apps(&mut canvas, area(), &[app(Some("0.1.0"))]);
        assert!(layout.rows[0].rollback.is_some());
    }

    #[test]
    fn a_row_with_no_previous_release_has_no_rollback_button() {
        let mut canvas = canvas();
        let layout = draw_apps(&mut canvas, area(), &[app(None)]);
        assert!(layout.rows[0].rollback.is_none());
        assert_eq!(layout.rollback_hit(Point::new(0.0, 0.0)), None);
    }

    #[test]
    fn every_row_button_is_big_enough_to_tap() {
        let mut canvas = canvas();
        let layout = draw_apps(&mut canvas, area(), &[app(Some("0.1.0")), app(None)]);
        for row in &layout.rows {
            assert!(row.uninstall.shortest_side() >= MIN_TOUCH_TARGET);
            if let Some(rollback) = row.rollback {
                assert!(rollback.shortest_side() >= MIN_TOUCH_TARGET);
            }
        }
    }

    #[test]
    fn an_empty_apps_list_still_draws_something() {
        let mut canvas = canvas();
        let layout = draw_apps(&mut canvas, area(), &[]);
        assert!(layout.rows.is_empty());
        assert!(canvas.ink_coverage() > 0.0);
    }

    #[test]
    fn grants_hand_back_one_revoke_button_per_row() {
        let mut canvas = canvas();
        let grants = vec![
            crate::host::GrantSummary {
                app_id: "dev.calum.chess".parse().unwrap(),
                app_name: "Chess".to_owned(),
                capability: Capability::Storage,
                in_use: true,
            },
            crate::host::GrantSummary {
                app_id: "dev.calum.chess".parse().unwrap(),
                app_name: "Chess".to_owned(),
                capability: Capability::Sharing,
                in_use: false,
            },
        ];
        let layout = draw_grants(&mut canvas, area(), &grants);
        assert_eq!(layout.revoke.len(), 2);
        for rect in &layout.revoke {
            assert!(rect.shortest_side() >= MIN_TOUCH_TARGET);
        }
        assert_eq!(layout.revoke_hit(layout.revoke[1].center()), Some(1));
    }

    #[test]
    fn format_bytes_reports_one_decimal_megabyte() {
        assert_eq!(format_bytes(41_300_000), "41.3 MB");
        assert_eq!(format_bytes(0), "0.0 MB");
    }

    #[test]
    fn the_storage_meter_fills_by_the_used_fraction() {
        let mut full = canvas();
        let mut empty = canvas();
        let used = StorageUsage {
            free_bytes: 0,
            buckets: vec![crate::host::StorageBucket {
                label: "DATA".to_owned(),
                bytes: 1_000_000,
            }],
        };
        let none_used = StorageUsage {
            free_bytes: 1_000_000,
            buckets: vec![crate::host::StorageBucket {
                label: "DATA".to_owned(),
                bytes: 0,
            }],
        };
        draw_storage(&mut full, area(), &used);
        draw_storage(&mut empty, area(), &none_used);

        // A point near the right edge of the meter bar: filled when the
        // fraction is 1.0, still just the empty track's colour when it is 0.
        // `ink_coverage` cannot tell these apart — the track itself is
        // already non-background — so this checks the actual pixel.
        let sample = area();
        let (x, y) = (
            (sample.x + sample.width - 20.0) as u32,
            (sample.y + 28.0) as u32,
        );
        assert_eq!(full.pixel(x, y), Some(palette::INK));
        assert_ne!(empty.pixel(x, y), Some(palette::INK));
    }
}
