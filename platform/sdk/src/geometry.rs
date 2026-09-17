//! Points, sizes and rectangles in canvas space.
//!
//! Canvas space is `f32` pixels with the origin top-left, `y` growing down,
//! and a fixed extent of [`SCREEN`](crate::SCREEN). Physical window pixels are
//! a different space and are converted only through
//! [`DisplayMapping`](crate::DisplayMapping) — mixing the two silently is the
//! bug this separation exists to prevent.

/// A position in canvas space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// Distance from the left edge.
    pub x: f32,
    /// Distance from the top edge.
    pub y: f32,
}

impl Point {
    /// The canvas origin.
    pub const ORIGIN: Point = Point::new(0.0, 0.0);

    /// Builds a point.
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// This point moved by `(dx, dy)`.
    pub const fn offset(self, dx: f32, dy: f32) -> Self {
        Self::new(self.x + dx, self.y + dy)
    }
}

/// An integer pixel extent — a canvas, a window, a framebuffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Size {
    /// Extent along `x`.
    pub width: u32,
    /// Extent along `y`.
    pub height: u32,
}

impl Size {
    /// Builds a size.
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// `width / height`, or `0.0` for a zero-height size.
    pub fn aspect_ratio(self) -> f32 {
        if self.height == 0 {
            0.0
        } else {
            self.width as f32 / self.height as f32
        }
    }

    /// Whether either dimension is zero, which most drawing paths cannot use.
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// An axis-aligned rectangle in canvas space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Extent along `x`.
    pub width: f32,
    /// Extent along `y`.
    pub height: f32,
}

impl Rect {
    /// Builds a rectangle from its top-left corner and extent.
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Builds a rectangle from two opposite corners.
    pub fn from_corners(a: Point, b: Point) -> Self {
        let (x0, x1) = if a.x <= b.x { (a.x, b.x) } else { (b.x, a.x) };
        let (y0, y1) = if a.y <= b.y { (a.y, b.y) } else { (b.y, a.y) };
        Self::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Right edge.
    pub fn right(self) -> f32 {
        self.x + self.width
    }

    /// Bottom edge.
    pub fn bottom(self) -> f32 {
        self.y + self.height
    }

    /// Midpoint.
    pub fn center(self) -> Point {
        Point::new(self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    /// Whether `point` is inside, treating the top-left edges as inclusive and
    /// the bottom-right edges as exclusive so adjacent rectangles never both
    /// claim the same pixel.
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
    }

    /// This rectangle shrunk by `amount` on every side.
    pub fn inset(self, amount: f32) -> Self {
        Self::new(
            self.x + amount,
            self.y + amount,
            (self.width - amount * 2.0).max(0.0),
            (self.height - amount * 2.0).max(0.0),
        )
    }

    /// The largest centred square inside this rectangle.
    pub fn centered_square(self) -> Self {
        let side = self.width.min(self.height);
        Self::new(
            self.x + (self.width - side) / 2.0,
            self.y + (self.height - side) / 2.0,
            side,
            side,
        )
    }

    /// The smallest side, which is what a touch-target check cares about.
    pub fn shortest_side(self) -> f32 {
        self.width.min(self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::{Point, Rect, Size};

    #[test]
    fn contains_is_half_open() {
        let rect = Rect::new(10.0, 10.0, 100.0, 50.0);
        assert!(rect.contains(Point::new(10.0, 10.0)));
        assert!(rect.contains(Point::new(109.9, 59.9)));
        assert!(!rect.contains(Point::new(110.0, 30.0)));
        assert!(!rect.contains(Point::new(30.0, 60.0)));
        assert!(!rect.contains(Point::new(9.9, 30.0)));
    }

    #[test]
    fn adjacent_rects_never_both_claim_a_point() {
        let left = Rect::new(0.0, 0.0, 50.0, 10.0);
        let right = Rect::new(50.0, 0.0, 50.0, 10.0);
        let boundary = Point::new(50.0, 5.0);
        assert!(!left.contains(boundary));
        assert!(right.contains(boundary));
    }

    #[test]
    fn centered_square_fits_and_centres() {
        let square = Rect::new(0.0, 0.0, 200.0, 100.0).centered_square();
        assert_eq!(square, Rect::new(50.0, 0.0, 100.0, 100.0));
    }

    #[test]
    fn inset_never_goes_negative() {
        let tiny = Rect::new(0.0, 0.0, 4.0, 4.0).inset(10.0);
        assert_eq!(tiny.width, 0.0);
        assert_eq!(tiny.height, 0.0);
    }

    #[test]
    fn size_reports_emptiness_and_aspect() {
        assert!(Size::new(0, 10).is_empty());
        assert!(!Size::new(1620, 2160).is_empty());
        assert!((Size::new(1620, 2160).aspect_ratio() - 0.75).abs() < 1e-6);
        assert_eq!(Size::new(10, 0).aspect_ratio(), 0.0);
    }

    #[test]
    fn from_corners_normalises_order() {
        let a = Rect::from_corners(Point::new(10.0, 20.0), Point::new(0.0, 5.0));
        assert_eq!(a, Rect::new(0.0, 5.0, 10.0, 15.0));
    }
}
