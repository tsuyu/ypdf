//! Rectangles and matrices, in PDF user space.
//!
//! Everything a redaction decides comes down to one question — is this mark
//! inside the area the user drew? — so the arithmetic that answers it is kept
//! in one place and tested on its own.

/// A 2×3 PDF transformation matrix, `[a b c d e f]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    /// Row 1, column 1.
    pub a: f32,
    /// Row 1, column 2.
    pub b: f32,
    /// Row 2, column 1.
    pub c: f32,
    /// Row 2, column 2.
    pub d: f32,
    /// Horizontal translation.
    pub e: f32,
    /// Vertical translation.
    pub f: f32,
}

impl Default for Matrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Matrix {
    /// The identity.
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// From the six operands of `cm` or `Tm`.
    #[must_use]
    pub const fn new(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// A translation.
    #[must_use]
    pub const fn translate(x: f32, y: f32) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: x,
            f: y,
        }
    }

    /// A scale.
    #[must_use]
    pub const fn scale(x: f32, y: f32) -> Self {
        Self {
            a: x,
            b: 0.0,
            c: 0.0,
            d: y,
            e: 0.0,
            f: 0.0,
        }
    }

    /// `self` then `other` — the order PDF composes in, where a new matrix is
    /// pre-multiplied onto the current one.
    #[must_use]
    pub fn then(self, other: Self) -> Self {
        Self {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.e * other.a + self.f * other.c + other.e,
            f: self.e * other.b + self.f * other.d + other.f,
        }
    }

    /// Apply to a point.
    #[must_use]
    pub fn apply(self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// The inverse, when there is one.
    ///
    /// A singular matrix means the content was scaled to nothing, and nothing
    /// is what it draws.
    #[must_use]
    pub fn invert(self) -> Option<Self> {
        let determinant = self.a * self.d - self.b * self.c;
        if determinant.abs() < f32::EPSILON {
            return None;
        }
        let inverse = 1.0 / determinant;
        Some(Self {
            a: self.d * inverse,
            b: -self.b * inverse,
            c: -self.c * inverse,
            d: self.a * inverse,
            e: (self.c * self.f - self.d * self.e) * inverse,
            f: (self.b * self.e - self.a * self.f) * inverse,
        })
    }

    /// How much this matrix scales lengths, roughly.
    ///
    /// Used to turn a font size in text space into one in page space.
    #[must_use]
    pub fn scale_factor(self) -> f32 {
        let x = (self.a * self.a + self.b * self.b).sqrt();
        let y = (self.c * self.c + self.d * self.d).sqrt();
        ((x * y).abs()).sqrt()
    }
}

/// An axis-aligned rectangle in PDF user space, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    /// Left.
    pub x0: f32,
    /// Bottom.
    pub y0: f32,
    /// Right.
    pub x1: f32,
    /// Top.
    pub y1: f32,
}

impl Rect {
    /// From any two opposite corners, in any order.
    #[must_use]
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    /// The rectangle containing every one of `points`.
    #[must_use]
    pub fn around(points: &[(f32, f32)]) -> Option<Self> {
        let first = points.first()?;
        let mut rect = Self {
            x0: first.0,
            y0: first.1,
            x1: first.0,
            y1: first.1,
        };
        for (x, y) in points.iter().skip(1) {
            rect.x0 = rect.x0.min(*x);
            rect.y0 = rect.y0.min(*y);
            rect.x1 = rect.x1.max(*x);
            rect.y1 = rect.y1.max(*y);
        }
        Some(rect)
    }

    /// Width.
    #[must_use]
    pub fn width(self) -> f32 {
        self.x1 - self.x0
    }

    /// Height.
    #[must_use]
    pub fn height(self) -> f32 {
        self.y1 - self.y0
    }

    /// Does any part of `other` fall inside this rectangle?
    ///
    /// Touching edges do not count: a glyph that ends exactly where a redaction
    /// begins is not inside it.
    #[must_use]
    pub fn intersects(self, other: Self) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }

    /// Is `other` entirely inside this rectangle?
    #[must_use]
    pub fn contains(self, other: Self) -> bool {
        other.x0 >= self.x0 && other.x1 <= self.x1 && other.y0 >= self.y0 && other.y1 <= self.y1
    }

    /// The overlapping part, if there is one.
    #[must_use]
    pub fn intersection(self, other: Self) -> Option<Self> {
        if !self.intersects(other) {
            return None;
        }
        Some(Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        })
    }

    /// Grow by `amount` on every side.
    ///
    /// Redactions are grown slightly before being tested against glyphs: a
    /// rectangle drawn exactly around a word should take the whole word, and
    /// glyph boxes computed from metrics are approximate.
    #[must_use]
    pub fn grown(self, amount: f32) -> Self {
        Self {
            x0: self.x0 - amount,
            y0: self.y0 - amount,
            x1: self.x1 + amount,
            y1: self.y1 + amount,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rectangle_normalizes_whichever_corners_it_is_given() {
        let a = Rect::new(10.0, 20.0, 30.0, 40.0);
        let b = Rect::new(30.0, 40.0, 10.0, 20.0);
        assert_eq!(a, b);
        assert!((a.width() - 20.0).abs() < f32::EPSILON);
    }

    #[test]
    fn touching_edges_do_not_count_as_overlapping() {
        // A glyph that ends exactly where a redaction starts is outside it.
        let left = Rect::new(0.0, 0.0, 10.0, 10.0);
        let right = Rect::new(10.0, 0.0, 20.0, 10.0);
        assert!(!left.intersects(right));
        assert!(left.intersects(Rect::new(9.9, 0.0, 20.0, 10.0)));
    }

    #[test]
    fn containment_is_stricter_than_intersection() {
        let outer = Rect::new(0.0, 0.0, 10.0, 10.0);
        let straddling = Rect::new(5.0, 5.0, 15.0, 15.0);
        assert!(outer.intersects(straddling));
        assert!(!outer.contains(straddling));
        assert!(outer.contains(Rect::new(1.0, 1.0, 9.0, 9.0)));
    }

    #[test]
    fn the_overlap_is_the_part_inside_both() {
        let overlap = Rect::new(0.0, 0.0, 10.0, 10.0)
            .intersection(Rect::new(5.0, 5.0, 15.0, 15.0))
            .expect("they overlap");
        assert_eq!(overlap, Rect::new(5.0, 5.0, 10.0, 10.0));
    }

    #[test]
    fn a_matrix_composes_the_way_pdf_does() {
        // Scale then translate: the translation is not scaled.
        let combined = Matrix::scale(2.0, 2.0).then(Matrix::translate(10.0, 0.0));
        assert_eq!(combined.apply(1.0, 1.0), (12.0, 2.0));
    }

    #[test]
    fn inverting_a_matrix_undoes_it() {
        let matrix = Matrix::new(2.0, 0.0, 0.0, 3.0, 10.0, 20.0);
        let inverse = matrix.invert().expect("invertible");
        let (x, y) = matrix.apply(4.0, 5.0);
        let (back_x, back_y) = inverse.apply(x, y);
        assert!((back_x - 4.0).abs() < 0.001, "{back_x}");
        assert!((back_y - 5.0).abs() < 0.001, "{back_y}");
    }

    #[test]
    fn a_matrix_that_flattens_everything_has_no_inverse() {
        assert!(Matrix::scale(0.0, 1.0).invert().is_none());
    }

    #[test]
    fn a_box_around_rotated_corners_covers_all_of_them() {
        let rect = Rect::around(&[(0.0, 0.0), (10.0, 5.0), (-3.0, 8.0)]).expect("some points");
        assert_eq!(rect, Rect::new(-3.0, 0.0, 10.0, 8.0));
    }

    #[test]
    fn growing_takes_in_a_little_more_on_every_side() {
        let grown = Rect::new(10.0, 10.0, 20.0, 20.0).grown(1.0);
        assert_eq!(grown, Rect::new(9.0, 9.0, 21.0, 21.0));
    }
}
