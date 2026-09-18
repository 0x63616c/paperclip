//! Composing the render test card's screen: the tab strip, whichever section
//! is showing, the HOME footer, and the damage a press claims.
//!
//! Every section but [`Section::Ghosting`] and [`Section::Damage`] is static
//! once drawn — there is nothing in them a tap can change — so only those two
//! carry interactive state and a hit test. Switching sections is always
//! [`Damage::Full`]: the sections draw wildly different content, and working
//! out a minimal diff between them would cost more than the extra flash it
//! saves.

use paper_sdk::{
    Action, Canvas, Damage, MAX_DAMAGE_RECTS, Point, PointerEvent, Rect, Size, SurfaceDescriptor,
    TextStyle, chrome, palette,
};

use crate::nav::{self, NavLayout, Section};
use crate::sections::{
    colour,
    damage::{self, DamageGridLayout, DamageGridState},
    geometry,
    ghosting::{self, GhostingLayout, GhostingState},
    greyscale, lines_and_text, waveform,
};

/// The render test card's own state: which section is showing, and the
/// scratch state the two interactive sections carry.
///
/// Nothing here is persisted (see [`crate::app::RenderTestCardApp`]'s module
/// doc) — a fresh launch always starts on [`Section::Greyscale`] with a clean
/// ghosting counter and an empty damage grid, which is the right lifetime for
/// numbers that exist to be looked at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct RenderTestCardScreen {
    /// The section currently showing.
    pub(crate) section: Section,
    ghosting: GhostingState,
    damage_grid: DamageGridState,
}

/// What a press did.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Press {
    /// What the app should ask the platform for.
    pub(crate) action: Action,
    /// What changed on the glass.
    pub(crate) damage: Damage,
}

impl Press {
    fn nothing() -> Self {
        Self {
            action: Action::None,
            damage: Damage::Regions {
                regions: Vec::new(),
            },
        }
    }

    fn leaving(action: Action) -> Self {
        Self {
            action,
            damage: Damage::Regions {
                regions: Vec::new(),
            },
        }
    }

    fn regions(regions: Vec<Rect>) -> Self {
        if regions.is_empty() {
            return Self::nothing();
        }
        if regions.len() > MAX_DAMAGE_RECTS {
            return Self::everything();
        }
        Self {
            action: Action::Redraw,
            damage: Damage::Regions { regions },
        }
    }

    fn everything() -> Self {
        Self {
            action: Action::Redraw,
            damage: Damage::Full,
        }
    }
}

/// Where everything on the card was drawn, so a press can hit-test against
/// exactly what a draw put on the glass.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RenderTestCardLayout {
    nav: NavLayout,
    home: Rect,
    ghosting: Option<GhostingLayout>,
    damage_grid: Option<DamageGridLayout>,
}

/// The content area below the tab strip and the section heading, for a
/// viewport shaped like `bounds` with the strip starting at `below_status`.
///
/// Pure geometry, factored out because both [`layout`] and [`draw`] need the
/// same two-step shrink (footer clearance, then heading clearance) and only
/// one of them has a [`Canvas`] to draw the heading into.
fn section_content_area(bounds: Rect, below_status: f32) -> (NavLayout, Rect) {
    let (nav_layout, content) = nav::layout_nav(bounds, below_status);
    let footer_top = bounds.height - chrome::FOOTER_HEIGHT;
    let content = Rect::new(
        content.x,
        content.y,
        content.width,
        (footer_top - 24.0 - content.y).max(0.0),
    );
    let content = Rect::new(
        content.x,
        content.y + 48.0,
        content.width,
        content.height - 48.0,
    );
    (nav_layout, content)
}

/// Where everything on the card would land for a viewport shaped like
/// `bounds`, computed from `screen` alone — no [`Canvas`].
///
/// [`crate::app::RenderTestCardApp`] calls this directly from
/// [`event`](paper_sdk::App::event), so a tap is hit-testable before the
/// first [`draw`](paper_sdk::App::draw) ever runs rather than only after a
/// frame has been drawn to cache it from. The [`SurfaceDescriptor`] the
/// [`Section::Geometry`] section reports is a [`draw`]-only concern: that
/// section has no interactive layout of its own to compute.
pub(crate) fn layout(bounds: Size, screen: &RenderTestCardScreen) -> RenderTestCardLayout {
    let bounds = Rect::new(0.0, 0.0, bounds.width as f32, bounds.height as f32);
    let below_status = chrome::status_bar_content_area(bounds).y;
    let (nav_layout, content) = section_content_area(bounds, below_status);

    let ghosting_layout = (screen.section == Section::Ghosting).then(|| ghosting::layout(content));
    let damage_layout = (screen.section == Section::Damage).then(|| damage::layout(content));

    let actions = chrome::footer_action_rects(bounds, 1);

    RenderTestCardLayout {
        nav: nav_layout,
        home: actions[0],
        ghosting: ghosting_layout,
        damage_grid: damage_layout,
    }
}

/// Draws the card against a layout [`layout`] already computed.
///
/// `surface` is this session's own [`SurfaceDescriptor`] — see
/// [`crate::sections::geometry`] for why it has to arrive from the caller
/// rather than be assumed.
pub(crate) fn draw(
    canvas: &mut Canvas,
    screen: &RenderTestCardScreen,
    surface: SurfaceDescriptor,
    layout: &RenderTestCardLayout,
) {
    let bounds = canvas.bounds();
    canvas.clear(palette::PAPER);

    let below_status =
        chrome::draw_status_bar(canvas, "RENDER TEST CARD", screen.section.tab_label());
    let (_, content) = section_content_area(bounds, below_status.y);
    nav::draw(canvas, screen.section, &layout.nav);

    canvas.draw_text(
        screen.section.heading(),
        Point::new(content.x, content.y - 8.0),
        TextStyle::new(30.0, palette::INK)
            .with_weight(0.13)
            .with_tracking(0.08),
    );

    match screen.section {
        Section::Greyscale => greyscale::render(canvas, content),
        Section::Colour => colour::render(canvas, content),
        Section::Waveform => waveform::render(canvas, content),
        Section::Ghosting => {
            if let Some(ghosting_layout) = &layout.ghosting {
                ghosting::draw(canvas, screen.ghosting, ghosting_layout);
            }
        }
        Section::Text => lines_and_text::render(canvas, content),
        Section::Damage => {
            if let Some(damage_layout) = &layout.damage_grid {
                damage::draw(canvas, screen.damage_grid, damage_layout);
            }
        }
        Section::Geometry => geometry::render(canvas, content, surface),
    }

    chrome::draw_footer_actions(canvas, &["HOME"]);
}

/// Computes the layout and draws it, for callers that want both — every
/// existing call site, and every test that predates the split above.
pub(crate) fn render(
    canvas: &mut Canvas,
    screen: &RenderTestCardScreen,
    surface: SurfaceDescriptor,
) -> RenderTestCardLayout {
    let bounds = canvas.bounds();
    let layout = self::layout(Size::new(bounds.width as u32, bounds.height as u32), screen);
    draw(canvas, screen, surface, &layout);
    layout
}

/// Handles a tap against the layout a draw already produced.
pub(crate) fn press(
    screen: &mut RenderTestCardScreen,
    layout: &RenderTestCardLayout,
    pointer: &PointerEvent,
) -> Press {
    if !pointer.is_tap() {
        return Press::nothing();
    }
    let at = pointer.at;

    if layout.home.contains(at) {
        return Press::leaving(Action::Home);
    }

    if let Some(section) = layout.nav.hit_test(at)
        && section != screen.section
    {
        screen.section = section;
        return Press::everything();
    }

    match screen.section {
        Section::Ghosting => {
            if let Some(ghosting_layout) = &layout.ghosting
                && let Some(changed) = ghosting::press(&mut screen.ghosting, ghosting_layout, at)
            {
                return Press::regions(changed);
            }
        }
        Section::Damage => {
            if let Some(damage_layout) = &layout.damage_grid
                && let Some(rect) = damage::press(&mut screen.damage_grid, damage_layout, at)
            {
                return Press::regions(vec![rect]);
            }
        }
        _ => {}
    }
    Press::nothing()
}

#[cfg(test)]
mod tests {
    use super::{RenderTestCardScreen, layout, press, render};
    use crate::nav::Section;
    use paper_sdk::{
        Action, Canvas, ContactId, Damage, PixelFormat, Point, Pointer, PointerEvent, PointerPhase,
        SCREEN, Size, SurfaceDescriptor,
    };

    fn canvas() -> Canvas {
        Canvas::new(SCREEN).expect("a screen-sized canvas")
    }

    fn surface() -> SurfaceDescriptor {
        SurfaceDescriptor::packed(SCREEN, PixelFormat::Argb8888)
    }

    fn tap(at: Point) -> PointerEvent {
        PointerEvent::new(at, PointerPhase::Up, Pointer::Touch, ContactId::FIRST)
    }

    #[test]
    fn a_fresh_screen_starts_on_greyscale() {
        assert_eq!(RenderTestCardScreen::default().section, Section::Greyscale);
    }

    #[test]
    fn layout_needs_no_canvas_and_matches_what_render_draws() {
        for section in Section::ALL {
            let screen = RenderTestCardScreen {
                section,
                ..Default::default()
            };
            let drawn = render(&mut canvas(), &screen, surface());
            assert_eq!(layout(SCREEN, &screen), drawn, "{section:?}");
        }
    }

    #[test]
    fn every_section_draws_something_and_is_reachable_by_tab() {
        let mut canvas = canvas();
        let mut screen = RenderTestCardScreen::default();
        for (index, section) in Section::ALL.into_iter().enumerate() {
            let layout = render(&mut canvas, &screen, surface());
            assert_eq!(screen.section, section);
            let coverage = canvas.ink_coverage();
            assert!(coverage > 0.0, "{section:?} drew nothing");

            if let Some(next_tab) = layout.nav.tabs().get(index + 1) {
                press(&mut screen, &layout, &tap(next_tab.center()));
                assert_eq!(screen.section, Section::ALL[index + 1]);
            }
        }
    }

    #[test]
    fn tapping_the_active_tab_changes_nothing() {
        let mut canvas = canvas();
        let mut screen = RenderTestCardScreen::default();
        let layout = render(&mut canvas, &screen, surface());
        let tab = layout.nav.tabs()[0];
        let result = press(&mut screen, &layout, &tap(tab.center()));
        assert_eq!(screen.section, Section::Greyscale);
        assert_eq!(
            result.damage,
            Damage::Regions {
                regions: Vec::new()
            }
        );
    }

    #[test]
    fn switching_sections_claims_the_whole_viewport() {
        let mut canvas = canvas();
        let mut screen = RenderTestCardScreen::default();
        let layout = render(&mut canvas, &screen, surface());
        let colour_tab = layout.nav.tabs()[1];
        let result = press(&mut screen, &layout, &tap(colour_tab.center()));
        assert_eq!(screen.section, Section::Colour);
        assert_eq!(result.damage, Damage::Full);
    }

    #[test]
    fn home_is_always_reachable() {
        let mut canvas = canvas();
        let mut screen = RenderTestCardScreen::default();
        let layout = render(&mut canvas, &screen, surface());
        let result = press(&mut screen, &layout, &tap(layout.home.center()));
        assert_eq!(result.action, Action::Home);
    }

    #[test]
    fn a_hover_is_never_treated_as_a_tap() {
        let mut canvas = canvas();
        let mut screen = RenderTestCardScreen::default();
        let layout = render(&mut canvas, &screen, surface());
        let hover = PointerEvent::new(
            layout.home.center(),
            PointerPhase::Hover,
            Pointer::Pen,
            ContactId::FIRST,
        );
        let result = press(&mut screen, &layout, &hover);
        assert_eq!(result.action, Action::None);
    }

    #[test]
    fn cycling_ghosting_claims_only_the_patch_and_counter() {
        let mut canvas = canvas();
        let mut screen = RenderTestCardScreen {
            section: Section::Ghosting,
            ..Default::default()
        };
        let layout = render(&mut canvas, &screen, surface());
        let button = layout.ghosting.expect("ghosting drew a layout").button;

        let result = press(&mut screen, &layout, &tap(button.center()));
        match result.damage {
            Damage::Regions { regions } => assert_eq!(regions.len(), 2),
            other => panic!("expected two regions, got {other:?}"),
        }
        assert_eq!(result.action, Action::Redraw);
    }

    #[test]
    fn tapping_a_damage_cell_claims_exactly_one_region() {
        let mut canvas = canvas();
        let mut screen = RenderTestCardScreen {
            section: Section::Damage,
            ..Default::default()
        };
        let layout = render(&mut canvas, &screen, surface());
        let cell = layout
            .damage_grid
            .expect("damage drew a layout")
            .cell_rect(0);

        let result = press(&mut screen, &layout, &tap(cell.center()));
        match result.damage {
            Damage::Regions { regions } => assert_eq!(regions, vec![cell]),
            other => panic!("expected one region, got {other:?}"),
        }
    }

    #[test]
    fn the_geometry_section_reflects_the_surface_it_was_given() {
        let mut canvas = canvas();
        let screen = RenderTestCardScreen {
            section: Section::Geometry,
            ..Default::default()
        };
        let padded = SurfaceDescriptor {
            extent: Size::new(1620, 2160),
            stride_bytes: 6528,
            format: PixelFormat::Argb8888,
        };
        render(&mut canvas, &screen, padded);
        assert!(canvas.ink_coverage() > 0.0);
    }
}
