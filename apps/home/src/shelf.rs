//! Shelf entries and where their tiles land.

use paper_packages::{AppId, Manifest};
use paper_sdk::chrome::MARGIN;
use paper_sdk::{Canvas, Point, Rect, TextStyle, palette};

/// The mark drawn on a tile.
///
/// A closed set rather than an image path: Stage 1 has no asset pipeline, and
/// four drawn marks are honest about that where four missing PNGs would not
/// be. Real app icons arrive with the package format's asset handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShelfGlyph {
    /// A miniature chess board.
    Board,
    /// A shop bag, for the App Store.
    Store,
    /// A return arrow, for handing the screen back to stock reMarkable.
    Stock,
    /// A gear, for Settings.
    Gear,
}

/// One tile on the shelf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShelfEntry {
    label: String,
    detail: String,
    glyph: ShelfGlyph,
    launch: Option<AppId>,
}

impl ShelfEntry {
    /// Builds an entry from an installed app's manifest.
    ///
    /// Tapping it returns [`Action::Launch`](paper_sdk::Action::Launch) with
    /// the manifest's own id (ADR-0018) — this is the one constructor that
    /// makes a tile do something on its own rather than only carrying the
    /// text drawn on it.
    pub fn from_manifest(manifest: &Manifest, glyph: ShelfGlyph) -> Self {
        Self {
            label: manifest.name().to_string(),
            detail: format!("V{}", manifest.version()),
            glyph,
            launch: Some(manifest.id().clone()),
        }
    }

    /// Builds an entry for something that is not an app — the handoff back to
    /// stock reMarkable, which has no manifest, no id, and nothing to launch.
    pub fn action(label: impl Into<String>, detail: impl Into<String>, glyph: ShelfGlyph) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            glyph,
            launch: None,
        }
    }

    /// The tile's title.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The line under the title.
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Which mark to draw.
    pub fn glyph(&self) -> ShelfGlyph {
        self.glyph
    }

    /// The app a tap on this tile should launch, if it is one.
    pub fn launch(&self) -> Option<&AppId> {
        self.launch.as_ref()
    }
}

/// Where each shelf tile was drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct ShelfLayout {
    tiles: Vec<Rect>,
}

impl ShelfLayout {
    /// Lays out `count` tiles in two columns inside `area`, and reports how
    /// far down the shelf reaches.
    pub fn compute(area: Rect, count: usize) -> Self {
        const COLUMNS: usize = 2;
        const GAP: f32 = 40.0;
        const TILE_HEIGHT: f32 = 500.0;

        let width = (area.width - GAP * (COLUMNS - 1) as f32) / COLUMNS as f32;
        let tiles = (0..count)
            .map(|index| {
                let column = index % COLUMNS;
                let row = index / COLUMNS;
                Rect::new(
                    area.x + column as f32 * (width + GAP),
                    area.y + row as f32 * (TILE_HEIGHT + GAP),
                    width,
                    TILE_HEIGHT,
                )
            })
            .collect();
        Self { tiles }
    }

    /// Every tile, in entry order.
    pub fn tiles(&self) -> &[Rect] {
        &self.tiles
    }

    /// Which entry a press landed on.
    pub fn hit_test(&self, at: Point) -> Option<usize> {
        self.tiles.iter().position(|tile| tile.contains(at))
    }

    /// The `y` below the last row of tiles.
    pub fn bottom(&self) -> f32 {
        self.tiles
            .iter()
            .map(|tile| tile.bottom())
            .fold(0.0_f32, f32::max)
    }
}

/// Draws one tile.
pub(crate) fn draw_tile(canvas: &mut Canvas, rect: Rect, entry: &ShelfEntry, pressed: bool) {
    let radius = 28.0;
    canvas.fill_round_rect(rect, radius, palette::TILE);
    canvas.stroke_round_rect(rect, radius, palette::HAIRLINE, 3.0);
    if pressed {
        canvas.stroke_round_rect(rect.inset(10.0), radius - 10.0, palette::INK, 8.0);
    }

    let mark = Rect::new(rect.x, rect.y + 54.0, rect.width, 236.0).centered_square();
    match entry.glyph() {
        ShelfGlyph::Board => draw_board_mark(canvas, mark),
        ShelfGlyph::Store => draw_store_mark(canvas, mark),
        ShelfGlyph::Stock => draw_stock_mark(canvas, mark),
        ShelfGlyph::Gear => draw_gear_mark(canvas, mark),
    }

    let center_x = rect.center().x;
    let label_style = paper_sdk::chrome::fit_text(
        entry.label(),
        TextStyle::new(52.0, palette::INK)
            .with_weight(0.12)
            .with_tracking(0.1)
            .centered(),
        rect.width - 48.0,
        26.0,
    );
    canvas.draw_text(
        entry.label(),
        Point::new(center_x, rect.bottom() - 148.0),
        label_style,
    );
    canvas.draw_text(
        entry.detail(),
        Point::new(center_x, rect.bottom() - 66.0),
        TextStyle::new(28.0, palette::INK_SOFT)
            .with_tracking(0.2)
            .centered(),
    );
}

fn draw_board_mark(canvas: &mut Canvas, rect: Rect) {
    let cells = 4.0;
    let cell = rect.width / cells;
    for row in 0..4 {
        for column in 0..4 {
            if (row + column) % 2 == 0 {
                canvas.fill_rect(
                    Rect::new(
                        rect.x + column as f32 * cell,
                        rect.y + row as f32 * cell,
                        cell,
                        cell,
                    ),
                    palette::BOARD_DARK,
                );
            }
        }
    }
    canvas.stroke_rect(rect, palette::INK, 5.0);
}

fn draw_store_mark(canvas: &mut Canvas, rect: Rect) {
    let bag = Rect::new(
        rect.x,
        rect.y + rect.height * 0.28,
        rect.width,
        rect.height * 0.72,
    );
    canvas.fill_round_rect(bag, 16.0, palette::PAPER);
    canvas.stroke_round_rect(bag, 16.0, palette::INK, 5.0);

    // The handle: two uprights and a bar, which reads as a bag at tile size
    // where an arc would just look like a smudge.
    let left = rect.x + rect.width * 0.30;
    let right = rect.x + rect.width * 0.70;
    let top = rect.y + rect.height * 0.06;
    canvas.stroke_polyline(
        &[
            Point::new(left, bag.y),
            Point::new(left, top + 18.0),
            Point::new(left + 18.0, top),
            Point::new(right - 18.0, top),
            Point::new(right, top + 18.0),
            Point::new(right, bag.y),
        ],
        palette::INK,
        5.0,
    );
}

fn draw_stock_mark(canvas: &mut Canvas, rect: Rect) {
    let center = rect.center();
    let radius = rect.width * 0.42;
    canvas.fill_circle(center, radius, palette::PAPER);
    canvas.stroke_polyline(&circle(center, radius), palette::INK, 5.0);

    let arm = radius * 0.52;
    canvas.stroke_polyline(
        &[
            Point::new(center.x + arm, center.y),
            Point::new(center.x - arm, center.y),
        ],
        palette::INK,
        8.0,
    );
    canvas.stroke_polyline(
        &[
            Point::new(center.x - arm * 0.2, center.y - arm * 0.6),
            Point::new(center.x - arm, center.y),
            Point::new(center.x - arm * 0.2, center.y + arm * 0.6),
        ],
        palette::INK,
        8.0,
    );
}

fn draw_gear_mark(canvas: &mut Canvas, rect: Rect) {
    let center = rect.center();
    let radius = rect.width * 0.32;

    // Eight teeth around the rim, drawn as filled wedges rather than rotated
    // rectangles: the canvas fills axis-aligned rectangles only, and a tooth
    // drawn as one would point the right way at four of the eight angles.
    // They are laid down first and the body is filled over their roots, so
    // only the part outside the rim shows.
    for step in 0..8 {
        let angle = step as f32 / 8.0 * std::f32::consts::TAU;
        let tooth: Vec<Point> = [(-0.22, 0.90), (-0.13, 1.34), (0.13, 1.34), (0.22, 0.90)]
            .into_iter()
            .map(|(offset, scale)| {
                let at = angle + offset;
                Point::new(
                    center.x + at.cos() * radius * scale,
                    center.y + at.sin() * radius * scale,
                )
            })
            .collect();
        canvas.fill_polygon(&tooth, palette::INK);
    }

    canvas.fill_circle(center, radius, palette::PAPER);
    canvas.stroke_polyline(&circle(center, radius), palette::INK, 5.0);
    // The hub, so the mark reads as a gear rather than a cog-shaped blob.
    canvas.stroke_polyline(&circle(center, radius * 0.38), palette::INK, 5.0);
}

fn circle(center: Point, radius: f32) -> Vec<Point> {
    (0..=48)
        .map(|step| {
            let angle = step as f32 / 48.0 * std::f32::consts::TAU;
            Point::new(
                center.x + radius * angle.cos(),
                center.y + radius * angle.sin(),
            )
        })
        .collect()
}

/// The shelf's own content width, given a canvas width.
pub(crate) fn shelf_area(canvas_width: f32, top: f32, height: f32) -> Rect {
    Rect::new(MARGIN, top, canvas_width - MARGIN * 2.0, height)
}

#[cfg(test)]
mod tests {
    use super::{ShelfEntry, ShelfGlyph, ShelfLayout, draw_tile};
    use paper_packages::Manifest;
    use paper_sdk::chrome::MIN_TOUCH_TARGET;
    use paper_sdk::{Canvas, Point, Rect, Size};

    fn area() -> Rect {
        Rect::new(56.0, 300.0, 1508.0, 1200.0)
    }

    #[test]
    fn entries_take_their_text_from_the_manifest() {
        let manifest = Manifest::parse(
            r#"
            [app]
            id = "dev.calum.chess"
            name = "Chess"
            version = "0.3.1"
            protocol = "1.0"
            entrypoint = "bin/chess"
            "#,
        )
        .expect("valid manifest");

        let entry = ShelfEntry::from_manifest(&manifest, ShelfGlyph::Board);
        assert_eq!(entry.label(), "Chess");
        assert_eq!(entry.detail(), "V0.3.1");
        assert_eq!(entry.glyph(), ShelfGlyph::Board);
        assert_eq!(entry.launch(), Some(manifest.id()));
    }

    #[test]
    fn non_app_entries_do_not_need_a_manifest() {
        let entry = ShelfEntry::action("Return to stock", "reMarkable", ShelfGlyph::Stock);
        assert_eq!(entry.label(), "Return to stock");
        assert_eq!(entry.detail(), "reMarkable");
        assert_eq!(entry.launch(), None, "there is nothing to launch it into");
    }

    #[test]
    fn tiles_fill_two_columns_in_entry_order() {
        let layout = ShelfLayout::compute(area(), 3);
        let tiles = layout.tiles();
        assert_eq!(tiles.len(), 3);

        assert!((tiles[0].x - area().x).abs() < 1e-3);
        assert!(tiles[1].x > tiles[0].x, "the second tile is to the right");
        assert!((tiles[1].right() - area().right()).abs() < 1e-3);
        assert!(tiles[2].y > tiles[0].y, "the third tile wraps to a new row");
        assert!((tiles[2].x - tiles[0].x).abs() < 1e-3);
    }

    #[test]
    fn tiles_never_overlap() {
        let layout = ShelfLayout::compute(area(), 6);
        let tiles = layout.tiles();
        for (i, a) in tiles.iter().enumerate() {
            for b in tiles.iter().skip(i + 1) {
                let separated = a.right() <= b.x + 1e-3
                    || b.right() <= a.x + 1e-3
                    || a.bottom() <= b.y + 1e-3
                    || b.bottom() <= a.y + 1e-3;
                assert!(separated, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn tiles_are_far_bigger_than_the_minimum_touch_target() {
        for tile in ShelfLayout::compute(area(), 4).tiles() {
            assert!(tile.shortest_side() >= MIN_TOUCH_TARGET * 2.0);
        }
    }

    #[test]
    fn hit_testing_finds_the_tile_that_was_pressed() {
        let layout = ShelfLayout::compute(area(), 3);
        for (index, tile) in layout.tiles().iter().enumerate() {
            assert_eq!(layout.hit_test(tile.center()), Some(index));
            assert_eq!(layout.hit_test(Point::new(tile.x, tile.y)), Some(index));
        }
        assert_eq!(layout.hit_test(Point::new(0.0, 0.0)), None);
        assert_eq!(layout.hit_test(Point::new(810.0, 5000.0)), None);
    }

    /// Two glyphs that draw the same picture would put the same mark on two
    /// tiles and send a tap to the wrong app; a glyph that draws nothing
    /// leaves a blank tile. Neither is visible to a layout assertion, and
    /// both are what a missing match arm looks like.
    #[test]
    fn each_glyph_draws_a_mark_of_its_own() {
        let glyphs = [
            ShelfGlyph::Board,
            ShelfGlyph::Store,
            ShelfGlyph::Stock,
            ShelfGlyph::Gear,
        ];
        let mut seen: Vec<(ShelfGlyph, f32)> = Vec::new();
        for glyph in glyphs {
            let mut canvas =
                Canvas::new(Size::new(700, 700)).expect("a tile-sized canvas allocates");
            let blank = canvas.ink_coverage();
            draw_tile(
                &mut canvas,
                Rect::new(20.0, 20.0, 660.0, 660.0),
                &ShelfEntry::action("App", "V1", glyph),
                false,
            );
            let coverage = canvas.ink_coverage();
            assert!(coverage > blank, "{glyph:?} drew nothing");
            if let Some((other, _)) = seen
                .iter()
                .find(|(_, other)| (other - coverage).abs() < 1e-4)
            {
                panic!("{glyph:?} and {other:?} draw the same mark");
            }
            seen.push((glyph, coverage));
        }
    }

    #[test]
    fn an_empty_shelf_has_no_tiles_and_no_extent() {
        let layout = ShelfLayout::compute(area(), 0);
        assert!(layout.tiles().is_empty());
        assert_eq!(layout.bottom(), 0.0);
        assert_eq!(layout.hit_test(Point::new(100.0, 400.0)), None);
    }
}
