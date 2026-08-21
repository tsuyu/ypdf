//! What an annotation is, in this crate's terms (spec §10).
//!
//! One type covers all of them rather than a type per subtype. They differ in
//! what geometry they carry — quads for a highlight, a polyline for freehand,
//! two points for a line — but everything else is shared: colour, opacity, line
//! width, an author, a note. A single shape keeps the CLI, the panel, and the
//! JSON in step, and makes "list what is on this page" one list.

use serde::{Deserialize, Serialize};

/// A rectangle in PDF points: left, bottom, right, top.
pub type Rect = [f32; 4];

/// A colour, red / green / blue in 0.0-1.0.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Colour(pub f32, pub f32, pub f32);

impl Default for Colour {
    /// The yellow every highlighter in the world is.
    fn default() -> Self {
        Self(1.0, 0.92, 0.23)
    }
}

impl Colour {
    /// Parse `#rrggbb` or `rrggbb`.
    ///
    /// # Errors
    ///
    /// [`ypdf_core::Error::Config`] when it is not six hexadecimal digits.
    pub fn parse(text: &str) -> ypdf_core::Result<Self> {
        let hex = text.trim().trim_start_matches('#');
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ypdf_core::Error::Config {
                detail: format!("{text:?} is not a colour; expected something like #ffe600"),
                source_path: None,
            });
        }
        let part = |from: usize| -> f32 {
            u8::from_str_radix(&hex[from..from + 2], 16).map_or(0.0, |v| f32::from(v) / 255.0)
        };
        Ok(Self(part(0), part(2), part(4)))
    }

    /// As `#rrggbb`.
    #[must_use]
    pub fn to_hex(self) -> String {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0-255"
        )]
        let byte = |value: f32| -> u8 { (value.clamp(0.0, 1.0) * 255.0).round() as u8 };
        format!(
            "#{:02x}{:02x}{:02x}",
            byte(self.0),
            byte(self.1),
            byte(self.2)
        )
    }
}

/// What the annotation draws.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Shape {
    /// Text marked with a translucent wash.
    Highlight {
        /// One rectangle per line of marked text.
        quads: Vec<Rect>,
    },
    /// A line under marked text.
    Underline {
        /// One rectangle per line.
        quads: Vec<Rect>,
    },
    /// A line through marked text.
    StrikeOut {
        /// One rectangle per line.
        quads: Vec<Rect>,
    },
    /// A sticky note: an icon that opens a comment.
    Note {
        /// Where the icon sits.
        at: [f32; 2],
    },
    /// A box of text drawn on the page.
    TextBox {
        /// Where it goes.
        rect: Rect,
        /// Point size. `None` fits the box.
        size: Option<f32>,
    },
    /// Freehand drawing: one or more strokes.
    Ink {
        /// Each stroke, as a list of points.
        strokes: Vec<Vec<[f32; 2]>>,
    },
    /// A rectangle.
    Rectangle {
        /// Where.
        rect: Rect,
        /// Fill colour, if it is filled.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Colour>,
    },
    /// An ellipse inscribed in a rectangle.
    Ellipse {
        /// Where.
        rect: Rect,
        /// Fill colour, if it is filled.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<Colour>,
    },
    /// A straight line, optionally with an arrowhead.
    Line {
        /// Where it starts.
        from: [f32; 2],
        /// Where it ends.
        to: [f32; 2],
        /// Draw an arrowhead at the end.
        #[serde(default)]
        arrow: bool,
    },
    /// A word stamped on the page: APPROVED, DRAFT, and the rest.
    Stamp {
        /// Where.
        rect: Rect,
        /// What it says.
        text: String,
    },
}

impl Shape {
    /// The PDF subtype name.
    #[must_use]
    pub const fn subtype(&self) -> &'static str {
        match self {
            Self::Highlight { .. } => "Highlight",
            Self::Underline { .. } => "Underline",
            Self::StrikeOut { .. } => "StrikeOut",
            Self::Note { .. } => "Text",
            Self::TextBox { .. } => "FreeText",
            Self::Ink { .. } => "Ink",
            Self::Rectangle { .. } => "Square",
            Self::Ellipse { .. } => "Circle",
            Self::Line { .. } => "Line",
            Self::Stamp { .. } => "Stamp",
        }
    }

    /// The word used in listings and JSON.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Highlight { .. } => "highlight",
            Self::Underline { .. } => "underline",
            Self::StrikeOut { .. } => "strike-through",
            Self::Note { .. } => "note",
            Self::TextBox { .. } => "text box",
            Self::Ink { .. } => "drawing",
            Self::Rectangle { .. } => "rectangle",
            Self::Ellipse { .. } => "ellipse",
            Self::Line { .. } => "line",
            Self::Stamp { .. } => "stamp",
        }
    }

    /// The area it occupies, in points.
    ///
    /// Everything a reader does with an annotation starts from `/Rect`, so this
    /// has to cover the whole drawing — a line that pokes outside its rectangle
    /// is clipped away by some readers and drawn by others.
    #[must_use]
    pub fn bounds(&self) -> Rect {
        match self {
            Self::Highlight { quads } | Self::Underline { quads } | Self::StrikeOut { quads } => {
                union(quads.iter().copied()).unwrap_or([0.0, 0.0, 0.0, 0.0])
            }
            // Big enough for a reader's own icon, which is what it draws.
            Self::Note { at } => [at[0], at[1] - 20.0, at[0] + 20.0, at[1]],
            Self::TextBox { rect, .. }
            | Self::Rectangle { rect, .. }
            | Self::Ellipse { rect, .. }
            | Self::Stamp { rect, .. } => *rect,
            Self::Ink { strokes } => union(strokes.iter().map(|stroke| {
                let xs = stroke.iter().map(|point| point[0]);
                let ys = stroke.iter().map(|point| point[1]);
                [
                    xs.clone().fold(f32::MAX, f32::min),
                    ys.clone().fold(f32::MAX, f32::min),
                    xs.fold(f32::MIN, f32::max),
                    ys.fold(f32::MIN, f32::max),
                ]
            }))
            .unwrap_or([0.0, 0.0, 0.0, 0.0]),
            Self::Line { from, to, .. } => [
                from[0].min(to[0]),
                from[1].min(to[1]),
                from[0].max(to[0]),
                from[1].max(to[1]),
            ],
        }
    }

    /// The area it occupies once its stroke is taken into account.
    ///
    /// A horizontal line has no height at all as pure geometry, and a `/Rect`
    /// of zero height is clipped away by every reader. The stroke extends half
    /// its width beyond the path on each side, so that is what the rectangle
    /// has to hold — plus a little slack, since joins and arrowheads reach
    /// further than the line itself.
    #[must_use]
    pub fn bounds_with_width(&self, width: f32) -> Rect {
        let bounds = self.bounds();
        let pad = match self {
            Self::Line { arrow, .. } => {
                if *arrow {
                    (width * 4.0).clamp(4.0, 20.0)
                } else {
                    width
                }
            }
            Self::Ink { .. } => width,
            // The others are drawn inside their own rectangle.
            _ => 0.0,
        };

        [
            bounds[0] - pad,
            bounds[1] - pad,
            bounds[2] + pad,
            bounds[3] + pad,
        ]
    }
}

fn union(rects: impl Iterator<Item = Rect>) -> Option<Rect> {
    rects.reduce(|a, b| {
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]
    })
}

/// One annotation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    /// 1-based page it is on.
    pub page: u32,
    /// What it draws.
    pub shape: Shape,
    /// Its colour.
    #[serde(default)]
    pub colour: Colour,
    /// 0.0 to 1.0.
    #[serde(default = "one")]
    pub opacity: f32,
    /// Stroke width, in points.
    #[serde(default = "default_width")]
    pub width: f32,
    /// The comment attached to it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub contents: String,
    /// Who made it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// When, as a PDF date string, when the file carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
}

const fn one() -> f32 {
    1.0
}

const fn default_width() -> f32 {
    1.5
}

impl Annotation {
    /// An annotation with the usual defaults.
    #[must_use]
    pub fn new(page: u32, shape: Shape) -> Self {
        Self {
            page,
            shape,
            colour: Colour::default(),
            opacity: 1.0,
            width: 1.5,
            contents: String::new(),
            author: String::new(),
            modified: None,
        }
    }

    /// Set the colour.
    #[must_use]
    pub const fn with_colour(mut self, colour: Colour) -> Self {
        self.colour = colour;
        self
    }

    /// Set the opacity, clamped to 0.0-1.0.
    #[must_use]
    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// Set the stroke width.
    #[must_use]
    pub fn with_width(mut self, width: f32) -> Self {
        self.width = width.max(0.1);
        self
    }

    /// Attach a comment.
    #[must_use]
    pub fn with_contents(mut self, contents: impl Into<String>) -> Self {
        self.contents = contents.into();
        self
    }

    /// Set the author.
    #[must_use]
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = author.into();
        self
    }

    /// A one-line description for a listing.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut out = format!("page {:>3}  {:<14}", self.page, self.shape.label());
        if !self.author.is_empty() {
            out.push_str(&format!(" {:<16}", self.author));
        }
        if !self.contents.is_empty() {
            let text: String = self.contents.chars().take(60).collect();
            out.push_str(&format!(" {text}"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_round_trips_through_hex() {
        let colour = Colour::parse("#ffe600").expect("parses");
        assert_eq!(colour.to_hex(), "#ffe600");
    }

    #[test]
    fn something_that_is_not_a_colour_says_what_one_looks_like() {
        let error = Colour::parse("yellowish").expect_err("refused");
        assert!(error.report().to_human().contains("#ffe600"));
    }

    #[test]
    fn the_bounds_of_a_highlight_cover_every_line_it_marks() {
        let shape = Shape::Highlight {
            quads: vec![[100.0, 700.0, 300.0, 712.0], [100.0, 686.0, 220.0, 698.0]],
        };
        assert_eq!(shape.bounds(), [100.0, 686.0, 300.0, 712.0]);
    }

    #[test]
    fn the_bounds_of_a_drawing_cover_every_stroke() {
        let shape = Shape::Ink {
            strokes: vec![
                vec![[10.0, 10.0], [20.0, 30.0]],
                vec![[5.0, 50.0], [40.0, 12.0]],
            ],
        };
        assert_eq!(shape.bounds(), [5.0, 10.0, 40.0, 50.0]);
    }

    #[test]
    fn a_line_drawn_backwards_still_has_a_positive_rectangle() {
        // `/Rect` with a negative width is clipped to nothing by some readers.
        let shape = Shape::Line {
            from: [300.0, 700.0],
            to: [100.0, 650.0],
            arrow: true,
        };
        assert_eq!(shape.bounds(), [100.0, 650.0, 300.0, 700.0]);
    }

    #[test]
    fn every_shape_has_the_subtype_a_reader_expects() {
        let cases = [
            (Shape::Highlight { quads: Vec::new() }, "Highlight"),
            (Shape::Note { at: [0.0, 0.0] }, "Text"),
            (
                Shape::TextBox {
                    rect: [0.0, 0.0, 1.0, 1.0],
                    size: None,
                },
                "FreeText",
            ),
            (
                Shape::Ink {
                    strokes: Vec::new(),
                },
                "Ink",
            ),
            (
                Shape::Rectangle {
                    rect: [0.0, 0.0, 1.0, 1.0],
                    fill: None,
                },
                "Square",
            ),
            (
                Shape::Ellipse {
                    rect: [0.0, 0.0, 1.0, 1.0],
                    fill: None,
                },
                "Circle",
            ),
        ];
        for (shape, subtype) in cases {
            assert_eq!(shape.subtype(), subtype, "{shape:?}");
        }
    }

    #[test]
    fn a_horizontal_line_still_has_a_rectangle_with_area() {
        // Zero height as pure geometry; a `/Rect` like that is clipped to
        // nothing by every reader, so the stroke width has to be counted.
        let shape = Shape::Line {
            from: [100.0, 400.0],
            to: [300.0, 400.0],
            arrow: false,
        };
        assert_eq!(shape.bounds(), [100.0, 400.0, 300.0, 400.0]);

        let padded = shape.bounds_with_width(2.0);
        assert!(padded[3] - padded[1] > 0.0, "{padded:?}");
        assert!(padded[2] - padded[0] > 200.0);
    }

    #[test]
    fn an_arrow_gets_room_for_its_head() {
        let shape = Shape::Line {
            from: [100.0, 400.0],
            to: [300.0, 400.0],
            arrow: true,
        };
        let plain = Shape::Line {
            from: [100.0, 400.0],
            to: [300.0, 400.0],
            arrow: false,
        };
        assert!(
            shape.bounds_with_width(1.5)[3] > plain.bounds_with_width(1.5)[3],
            "an arrowhead reaches further than the line"
        );
    }

    #[test]
    fn a_shape_drawn_inside_its_own_rectangle_is_not_padded() {
        let shape = Shape::Rectangle {
            rect: [10.0, 10.0, 100.0, 60.0],
            fill: None,
        };
        assert_eq!(shape.bounds_with_width(3.0), shape.bounds());
    }

    #[test]
    fn opacity_and_width_are_clamped_rather_than_trusted() {
        let annotation = Annotation::new(1, Shape::Note { at: [0.0, 0.0] })
            .with_opacity(5.0)
            .with_width(-3.0);
        assert!((annotation.opacity - 1.0).abs() < f32::EPSILON);
        assert!(annotation.width > 0.0);
    }

    #[test]
    fn a_description_reads_as_a_line_of_a_listing() {
        let annotation = Annotation::new(3, Shape::Highlight { quads: Vec::new() })
            .with_author("Ada")
            .with_contents("check this figure");
        let line = annotation.describe();
        assert!(line.contains("page   3"), "{line}");
        assert!(line.contains("highlight"), "{line}");
        assert!(line.contains("Ada"), "{line}");
        assert!(line.contains("check this figure"), "{line}");
    }
}
