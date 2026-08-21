//! Watermarks: text and images stamped onto pages (spec §12).
//!
//! Everything a watermark does is **additive**. It appends (or prepends) one
//! content stream and the resources that stream needs; nothing the page already
//! draws is touched, moved, or re-encoded. Removing a watermark applied this way
//! means deleting one object, which is the property that makes it safe to apply
//! to a document someone still has to work on.
//!
//! Two placements, and the difference matters:
//!
//! * **Over** the page — the usual `CONFIDENTIAL` diagonal. It sits on top of
//!   the text, so opacity is what keeps the page readable.
//! * **Under** the page — a logo or letterhead behind the content. Nothing is
//!   obscured at all, but anything opaque already on the page hides it.
//!
//! ```no_run
//! use ypdf_doc::Pdf;
//! use ypdf_watermark::{Placement, Position, Watermark, apply};
//!
//! # fn main() -> ypdf_core::Result<()> {
//! let mut pdf = Pdf::open("report.pdf")?;
//! let watermark = Watermark::text("CONFIDENTIAL")
//!     .with_position(Position::Center)
//!     .with_rotation(45.0)
//!     .with_opacity(0.15)
//!     .with_placement(Placement::Over);
//!
//! let pages: Vec<u32> = (1..=pdf.page_count()).collect();
//! apply(&mut pdf, &pages, &watermark)?;
//! pdf.save("report-watermarked.pdf")?;
//! # Ok(())
//! # }
//! ```

mod image_xobject;
mod metrics;
mod stamp;

use ypdf_core::{Error, Result};
use ypdf_doc::Pdf;

pub use image_xobject::ImageKind;
pub use metrics::Face;

/// Where on the page the watermark sits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Position {
    /// The middle of the page.
    #[default]
    Center,
    /// Top left corner, inside the margin.
    TopLeft,
    /// Top centre.
    TopCenter,
    /// Top right corner.
    TopRight,
    /// Bottom left corner.
    BottomLeft,
    /// Bottom centre.
    BottomCenter,
    /// Bottom right corner.
    BottomRight,
    /// Repeated across the whole page.
    ///
    /// The one placement that cannot be cropped off: useful when the point is
    /// that a copy is identifiable, rather than that it is labelled.
    Tiled,
}

/// Whether the watermark is drawn on top of the page or behind it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Placement {
    /// On top. What a `CONFIDENTIAL` stamp needs.
    #[default]
    Over,
    /// Behind. What a letterhead or a background logo needs.
    Under,
}

/// A colour, as red / green / blue in 0.0-1.0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colour(pub f32, pub f32, pub f32);

impl Default for Colour {
    /// A mid grey: dark enough to read, light enough not to fight the page.
    fn default() -> Self {
        Self(0.5, 0.5, 0.5)
    }
}

impl Colour {
    /// Parse `#rrggbb` or `rrggbb`.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the string is not six hexadecimal digits.
    pub fn parse(text: &str) -> Result<Self> {
        let hex = text.trim().trim_start_matches('#');
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Config {
                detail: format!("{text:?} is not a colour; expected something like #808080"),
                source_path: None,
            });
        }

        let component = |from: usize| -> f32 {
            u8::from_str_radix(&hex[from..from + 2], 16).map_or(0.0, |v| f32::from(v) / 255.0)
        };
        Ok(Self(component(0), component(2), component(4)))
    }
}

/// What the watermark draws.
#[derive(Clone, Debug)]
pub enum Content {
    /// A line of text.
    Text {
        /// The words.
        text: String,
        /// Which face to draw them in.
        face: Face,
        /// Point size, or `None` to fit the page width.
        size: Option<f32>,
        /// Colour.
        colour: Colour,
    },
    /// An image: a logo, a signature, a scanned stamp.
    Image {
        /// The file, as read from disk.
        bytes: Vec<u8>,
    },
}

/// A watermark, ready to apply.
#[derive(Clone, Debug)]
pub struct Watermark {
    /// What it draws.
    pub content: Content,
    /// Where it sits.
    pub position: Position,
    /// Degrees anticlockwise.
    pub rotation: f32,
    /// 0.0 (invisible) to 1.0 (solid).
    pub opacity: f32,
    /// Size multiplier, applied after fitting.
    pub scale: f32,
    /// Over the page or behind it.
    pub placement: Placement,
}

impl Watermark {
    /// A text watermark with the usual defaults: grey, diagonal, faint.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: Content::Text {
                text: text.into(),
                face: Face::Bold,
                size: None,
                colour: Colour::default(),
            },
            position: Position::Center,
            rotation: 45.0,
            // Faint enough to read the page through, solid enough to see.
            opacity: 0.15,
            scale: 1.0,
            placement: Placement::Over,
        }
    }

    /// An image watermark.
    #[must_use]
    pub fn image(bytes: Vec<u8>) -> Self {
        Self {
            content: Content::Image { bytes },
            position: Position::Center,
            rotation: 0.0,
            opacity: 0.5,
            scale: 1.0,
            placement: Placement::Over,
        }
    }

    /// Set the position.
    #[must_use]
    pub const fn with_position(mut self, position: Position) -> Self {
        self.position = position;
        self
    }

    /// Set the rotation, in degrees.
    #[must_use]
    pub const fn with_rotation(mut self, degrees: f32) -> Self {
        self.rotation = degrees;
        self
    }

    /// Set the opacity, 0.0 to 1.0.
    #[must_use]
    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// Set the scale multiplier.
    #[must_use]
    pub fn with_scale(mut self, scale: f32) -> Self {
        self.scale = scale.max(0.01);
        self
    }

    /// Set whether it goes over the page or behind it.
    #[must_use]
    pub const fn with_placement(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }

    /// Anything obviously unusable, before a document is touched.
    fn validate(&self) -> Result<()> {
        if let Content::Text { text, .. } = &self.content
            && text.trim().is_empty()
        {
            return Err(Error::Config {
                detail: "a text watermark needs some text".into(),
                source_path: None,
            });
        }
        if self.opacity <= 0.0 {
            // Applying it would write an object nobody can see and report
            // success, which reads as the feature being broken.
            return Err(Error::Config {
                detail: "an opacity of zero would stamp something invisible".into(),
                source_path: None,
            });
        }
        Ok(())
    }
}

/// What applying a watermark did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Pages stamped.
    pub pages: usize,
    /// Characters the base-14 font could not encode, if any.
    ///
    /// Non-zero means the text was refused rather than mangled.
    pub unencodable: usize,
    /// How an image watermark was stored, when it was one.
    ///
    /// Worth reporting: a logo stored without its alpha mask would be drawn on
    /// a black rectangle, and knowing which path it took says whether to look.
    pub image: Option<ImageKind>,
}

/// Stamp `watermark` onto the given 1-based `pages`.
///
/// # Errors
///
/// Returns before touching the document if the watermark is unusable, if a page
/// number is out of range, or if an image cannot be decoded.
pub fn apply(pdf: &mut Pdf, pages: &[u32], watermark: &Watermark) -> Result<Report> {
    watermark.validate()?;

    let count = pdf.page_count();
    for page in pages {
        if *page == 0 || *page > count {
            return Err(Error::PageOutOfRange {
                requested: *page,
                pages: count,
            });
        }
    }

    let report = stamp::apply(pdf, pages, watermark)?;
    tracing::info!(
        pages = report.pages,
        placement = ?watermark.placement,
        "watermark applied"
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_parses_with_or_without_the_hash() {
        assert_eq!(
            Colour::parse("#ff0000").expect("red"),
            Colour(1.0, 0.0, 0.0)
        );
        let grey = Colour::parse("808080").expect("grey");
        assert!((grey.0 - 0.502).abs() < 0.01, "{grey:?}");
    }

    #[test]
    fn a_string_that_is_not_a_colour_says_what_one_looks_like() {
        let error = Colour::parse("reddish").expect_err("not a colour");
        assert!(error.report().to_human().contains("#808080"));
    }

    #[test]
    fn opacity_is_clamped_rather_than_trusted() {
        assert!((Watermark::text("X").with_opacity(4.0).opacity - 1.0).abs() < f32::EPSILON);
        assert!((Watermark::text("X").with_opacity(-1.0).opacity - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn an_empty_text_watermark_is_refused() {
        assert!(Watermark::text("   ").validate().is_err());
    }

    #[test]
    fn a_fully_transparent_watermark_is_refused_rather_than_written() {
        // Reporting success for something nobody can see reads as a bug in the
        // feature, and sends someone looking in the wrong place.
        assert!(
            Watermark::text("DRAFT")
                .with_opacity(0.0)
                .validate()
                .is_err()
        );
    }

    #[test]
    fn the_text_defaults_are_the_ones_people_actually_want() {
        let watermark = Watermark::text("CONFIDENTIAL");
        assert_eq!(watermark.position, Position::Center);
        assert!((watermark.rotation - 45.0).abs() < f32::EPSILON);
        assert!(watermark.opacity < 0.3, "faint enough to read through");
        assert_eq!(watermark.placement, Placement::Over);
    }
}
