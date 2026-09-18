//! The tab strip that switches between the card's sections.
//!
//! Same shape as `paper_settings::nav`: a flat strip, no per-section routing.
//! Seven sections, one per thing WWW-47 asks this card to settle.

use paper_sdk::chrome::MARGIN;
use paper_sdk::{Canvas, Point, Rect, TextStyle, palette};

/// One section of the render test card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Section {
    /// Every palette step, labelled, plus a continuous ramp for banding.
    ///
    /// The section a fresh launch opens on: it needs no interpretation and
    /// nothing on it depends on another section having run first.
    #[default]
    Greyscale,
    /// Primaries, secondaries and a set of UI tints, at full and reduced
    /// saturation.
    Colour,
    /// The waveform x content-type matrix: identical content, four labelled
    /// pairings.
    Waveform,
    /// A patch cycled under partial refresh next to an untouched control.
    Ghosting,
    /// Hairlines at each orientation and the stroke font at every chrome size.
    Text,
    /// A grid that claims damage one cell at a time.
    Damage,
    /// The surface geometry this session actually received.
    Geometry,
}

impl Section {
    /// Every section, in tab order.
    pub(crate) const ALL: [Section; 7] = [
        Section::Greyscale,
        Section::Colour,
        Section::Waveform,
        Section::Ghosting,
        Section::Text,
        Section::Damage,
        Section::Geometry,
    ];

    /// The short label drawn on its tab.
    pub(crate) fn tab_label(self) -> &'static str {
        match self {
            Section::Greyscale => "GREY",
            Section::Colour => "COLOUR",
            Section::Waveform => "WAVEFORM",
            Section::Ghosting => "GHOSTING",
            Section::Text => "TEXT",
            Section::Damage => "DAMAGE",
            Section::Geometry => "GEOMETRY",
        }
    }

    /// The heading drawn at the top of its content.
    pub(crate) fn heading(self) -> &'static str {
        match self {
            Section::Greyscale => "GREYSCALE RAMP",
            Section::Colour => "COLOUR RAMPS",
            Section::Waveform => "WAVEFORM X CONTENT-TYPE MATRIX",
            Section::Ghosting => "GHOSTING",
            Section::Text => "HAIRLINES AND TEXT",
            Section::Damage => "DAMAGE REGIONS",
            Section::Geometry => "GEOMETRY READOUT",
        }
    }
}

/// Height of the tab strip.
pub(crate) const NAV_HEIGHT: f32 = 120.0;

/// Where each tab landed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NavLayout {
    tabs: Vec<Rect>,
}

impl NavLayout {
    /// Every tab rectangle, in [`Section::ALL`] order.
    ///
    /// Test-only: production code only ever needs [`Self::hit_test`], and the
    /// tab rectangles themselves are exactly what a test needs to compute a
    /// tap.
    #[cfg(test)]
    pub(crate) fn tabs(&self) -> &[Rect] {
        &self.tabs
    }

    /// Which section a press landed on.
    pub(crate) fn hit_test(&self, at: Point) -> Option<Section> {
        let index = self.tabs.iter().position(|tab| tab.contains(at))?;
        Section::ALL.get(index).copied()
    }
}

/// Where the tab strip and the content rectangle under it land, for a
/// viewport shaped like `bounds` with the strip starting at `top`.
///
/// Pure geometry: which tab is `current` does not change where any tab is,
/// only how it is drawn, so [`draw_nav`] is the only thing that needs it.
pub(crate) fn layout_nav(bounds: Rect, top: f32) -> (NavLayout, Rect) {
    let count = Section::ALL.len();
    let width = bounds.width / count as f32;

    let tabs: Vec<Rect> = (0..count)
        .map(|index| Rect::new(index as f32 * width, top, width, NAV_HEIGHT))
        .collect();

    let content = Rect::new(
        MARGIN,
        top + NAV_HEIGHT + 28.0,
        bounds.width - MARGIN * 2.0,
        bounds.height - (top + NAV_HEIGHT + 28.0),
    );
    (NavLayout { tabs }, content)
}

/// Draws the tab strip against a layout [`layout_nav`] already computed.
pub(crate) fn draw(canvas: &mut Canvas, current: Section, nav: &NavLayout) {
    let bounds = canvas.bounds();
    let tabs = &nav.tabs;

    for (section, tab) in Section::ALL.into_iter().zip(tabs) {
        let active = section == current;
        if active {
            canvas.fill_rect(*tab, palette::TILE);
        }
        let style = paper_sdk::chrome::fit_text(
            section.tab_label(),
            TextStyle::new(
                26.0,
                if active {
                    palette::INK
                } else {
                    palette::INK_SOFT
                },
            )
            .with_weight(if active { 0.14 } else { 0.1 })
            .with_tracking(0.08)
            .centered(),
            tab.width - 16.0,
            15.0,
        );
        canvas.draw_text(
            section.tab_label(),
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
    if let Some(top) = tabs.first().map(|tab| tab.y) {
        canvas.hairline(
            Point::new(0.0, top + NAV_HEIGHT),
            bounds.width,
            palette::HAIRLINE,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{NAV_HEIGHT, Section, layout_nav};
    use paper_sdk::chrome::MIN_TOUCH_TARGET;
    use paper_sdk::{Canvas, Rect, SCREEN};

    fn bounds() -> Rect {
        Canvas::new(SCREEN).expect("screen-sized canvas").bounds()
    }

    #[test]
    fn tabs_tile_the_full_width_without_overlapping() {
        let (nav, _content) = layout_nav(bounds(), 132.0);
        let tabs = nav.tabs();
        assert_eq!(tabs.len(), Section::ALL.len());
        assert!((tabs[0].x - 0.0).abs() < 1e-3);
        assert!((tabs.last().unwrap().right() - SCREEN.width as f32).abs() < 1e-3);
        for pair in tabs.windows(2) {
            assert!((pair[0].right() - pair[1].x).abs() < 1e-3, "tabs overlap");
        }
    }

    #[test]
    fn every_tab_is_tall_enough_to_tap() {
        let (nav, _content) = layout_nav(bounds(), 132.0);
        for tab in nav.tabs() {
            assert!(tab.height >= MIN_TOUCH_TARGET.min(NAV_HEIGHT));
        }
    }

    #[test]
    fn hit_test_finds_the_section_under_a_tab() {
        let (nav, _content) = layout_nav(bounds(), 132.0);
        for (section, tab) in Section::ALL.into_iter().zip(nav.tabs()) {
            assert_eq!(nav.hit_test(tab.center()), Some(section));
        }
        assert_eq!(nav.hit_test(paper_sdk::Point::new(-10.0, -10.0)), None);
    }

    #[test]
    fn content_starts_below_the_nav_and_never_goes_negative() {
        let (_nav, content) = layout_nav(bounds(), 132.0);
        assert!(content.y > 132.0 + NAV_HEIGHT);
        assert!(content.height > 0.0);
        assert!(content.bottom() <= SCREEN.height as f32);
    }
}
