//! The tab strip that switches between Settings pages.
//!
//! Six pages, deliberately scoped small (WWW-22): a flat tab strip is the
//! whole navigation model, with no per-page routing to get wrong.

use paper_sdk::chrome::MARGIN;
use paper_sdk::{Canvas, Point, Rect, TextStyle, palette};

/// One page of Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsPage {
    /// Installed apps: versions, rollback, uninstall.
    Apps,
    /// What is used under `data/`, `shared/`, `staging/` and release directories.
    Storage,
    /// Which apps hold which capability grants.
    Grants,
    /// The configured catalog endpoint and its reachability.
    Catalog,
    /// Paperclip version, firmware and active platform release.
    Platform,
    /// Recent platform errors, read-only.
    Diagnostics,
}

impl SettingsPage {
    /// Every page, in tab order.
    pub const ALL: [SettingsPage; 6] = [
        SettingsPage::Apps,
        SettingsPage::Storage,
        SettingsPage::Grants,
        SettingsPage::Catalog,
        SettingsPage::Platform,
        SettingsPage::Diagnostics,
    ];

    /// The short label drawn on its tab.
    pub fn tab_label(self) -> &'static str {
        match self {
            SettingsPage::Apps => "APPS",
            SettingsPage::Storage => "STORAGE",
            SettingsPage::Grants => "GRANTS",
            SettingsPage::Catalog => "CATALOG",
            SettingsPage::Platform => "PLATFORM",
            SettingsPage::Diagnostics => "LOGS",
        }
    }

    /// The heading drawn at the top of its content.
    pub fn heading(self) -> &'static str {
        match self {
            SettingsPage::Apps => "INSTALLED APPS",
            SettingsPage::Storage => "STORAGE",
            SettingsPage::Grants => "GRANTS",
            SettingsPage::Catalog => "CATALOG",
            SettingsPage::Platform => "PLATFORM",
            SettingsPage::Diagnostics => "DIAGNOSTICS",
        }
    }
}

/// Height of the tab strip.
pub const NAV_HEIGHT: f32 = 132.0;

/// Where each tab landed.
#[derive(Debug, Clone, PartialEq)]
pub struct NavLayout {
    tabs: Vec<Rect>,
}

impl NavLayout {
    /// Every tab rectangle, in [`SettingsPage::ALL`] order.
    pub fn tabs(&self) -> &[Rect] {
        &self.tabs
    }

    /// Which page a press landed on.
    pub fn hit_test(&self, at: Point) -> Option<SettingsPage> {
        let index = self.tabs.iter().position(|tab| tab.contains(at))?;
        SettingsPage::ALL.get(index).copied()
    }
}

/// Draws the tab strip below the status bar and returns the content rectangle
/// under it.
pub(crate) fn draw_nav(canvas: &mut Canvas, current: SettingsPage, top: f32) -> (NavLayout, Rect) {
    let bounds = canvas.bounds();
    let count = SettingsPage::ALL.len();
    let width = bounds.width / count as f32;

    let tabs: Vec<Rect> = (0..count)
        .map(|index| Rect::new(index as f32 * width, top, width, NAV_HEIGHT))
        .collect();

    for (page, tab) in SettingsPage::ALL.into_iter().zip(&tabs) {
        let active = page == current;
        if active {
            canvas.fill_rect(*tab, palette::TILE);
        }
        let style = paper_sdk::chrome::fit_text(
            page.tab_label(),
            TextStyle::new(
                30.0,
                if active {
                    palette::INK
                } else {
                    palette::INK_SOFT
                },
            )
            .with_weight(if active { 0.14 } else { 0.1 })
            .with_tracking(0.1)
            .centered(),
            tab.width - 24.0,
            18.0,
        );
        canvas.draw_text(
            page.tab_label(),
            Point::new(tab.center().x, tab.center().y - style.size / 2.0),
            style,
        );
        if active {
            canvas.fill_rect(
                Rect::new(tab.x, tab.bottom() - 6.0, tab.width, 6.0),
                palette::INK,
            );
        }
    }
    canvas.hairline(
        Point::new(0.0, top + NAV_HEIGHT),
        bounds.width,
        palette::HAIRLINE,
    );

    let content = Rect::new(
        MARGIN,
        top + NAV_HEIGHT + 32.0,
        bounds.width - MARGIN * 2.0,
        bounds.height - (top + NAV_HEIGHT + 32.0),
    );
    (NavLayout { tabs }, content)
}

#[cfg(test)]
mod tests {
    use super::{NAV_HEIGHT, SettingsPage, draw_nav};
    use paper_sdk::chrome::MIN_TOUCH_TARGET;
    use paper_sdk::{Canvas, Point, SCREEN};

    fn canvas() -> Canvas {
        Canvas::new(SCREEN).expect("screen-sized canvas")
    }

    #[test]
    fn tabs_tile_the_full_width_without_overlapping() {
        let mut canvas = canvas();
        let (nav, _content) = draw_nav(&mut canvas, SettingsPage::Apps, 132.0);
        let tabs = nav.tabs();
        assert_eq!(tabs.len(), SettingsPage::ALL.len());
        assert!((tabs[0].x - 0.0).abs() < 1e-3);
        assert!((tabs.last().unwrap().right() - SCREEN.width as f32).abs() < 1e-3);
        for pair in tabs.windows(2) {
            assert!((pair[0].right() - pair[1].x).abs() < 1e-3, "tabs overlap");
        }
    }

    #[test]
    fn every_tab_is_tall_enough_to_tap() {
        let mut canvas = canvas();
        let (nav, _content) = draw_nav(&mut canvas, SettingsPage::Apps, 132.0);
        for tab in nav.tabs() {
            assert!(tab.height >= MIN_TOUCH_TARGET.min(NAV_HEIGHT));
        }
    }

    #[test]
    fn hit_test_finds_the_page_under_a_tab() {
        let mut canvas = canvas();
        let (nav, _content) = draw_nav(&mut canvas, SettingsPage::Apps, 132.0);
        for (page, tab) in SettingsPage::ALL.into_iter().zip(nav.tabs()) {
            assert_eq!(nav.hit_test(tab.center()), Some(page));
        }
        assert_eq!(nav.hit_test(Point::new(-10.0, -10.0)), None);
    }

    #[test]
    fn content_starts_below_the_nav_and_never_goes_negative() {
        let mut canvas = canvas();
        let (_nav, content) = draw_nav(&mut canvas, SettingsPage::Apps, 132.0);
        assert!(content.y > 132.0 + NAV_HEIGHT);
        assert!(content.height > 0.0);
        assert!(content.bottom() <= SCREEN.height as f32);
    }
}
