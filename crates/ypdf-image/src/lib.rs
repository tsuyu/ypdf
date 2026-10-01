//! Images into a PDF (spec §6).
//!
//! One picture per page, in the order given. The work that matters is not the
//! PDF plumbing — it is deciding how big the page is and where the image sits
//! on it, because that is what someone notices when they print the result.
//!
//! Two placements, and they answer different questions:
//!
//! * [`PageFit::Image`] makes the page the size of the picture at a chosen
//!   resolution. Nothing is cropped, nothing is padded, and a 4000×3000 photo
//!   at 300 dpi becomes a 13.3×10 inch page. This is right for archiving
//!   scans, where the page *is* the sheet that was scanned.
//! * [`PageFit::Fixed`] puts every picture on the same paper — A4, Letter —
//!   scaled to fit inside a margin and centred. This is right for anything
//!   destined for a printer, where pages of varying size are a nuisance.
//!
//! Image bytes go in untouched wherever possible: a JPEG is already a
//! DCTDecode stream, so it is embedded as-is rather than decoded and
//! re-encoded, which would lose quality to no purpose. PNG, WebP and TIFF are
//! decoded and stored losslessly.
//!
//! A file that cannot be read does not sink the run. Someone converting a
//! folder of two hundred scans should get a PDF of the 199 that are fine and
//! be told plainly about the one that is not.

use lopdf::{Document, Object, ObjectId, Stream, dictionary};
use ypdf_core::{Error, Result};
use ypdf_doc::{Pdf, image_xobject};

/// A paper size, in PDF points (72 to the inch).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageSize {
    /// Width in points.
    pub width: f32,
    /// Height in points.
    pub height: f32,
}

impl PageSize {
    /// ISO A4, 210×297 mm.
    pub const A4: Self = Self {
        width: 595.276,
        height: 841.89,
    };
    /// US Letter, 8.5×11 in.
    pub const LETTER: Self = Self {
        width: 612.0,
        height: 792.0,
    };
    /// US Legal, 8.5×14 in.
    pub const LEGAL: Self = Self {
        width: 612.0,
        height: 1008.0,
    };

    /// Look up a size by name, case-insensitively.
    pub fn parse(name: &str) -> Result<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "a4" => Ok(Self::A4),
            "letter" => Ok(Self::LETTER),
            "legal" => Ok(Self::LEGAL),
            other => Err(Error::Config {
                detail: format!("no page size called {other:?}. Known: a4, letter, legal"),
                source_path: None,
            }),
        }
    }

    /// The same size turned on its side.
    #[must_use]
    pub const fn turned(self) -> Self {
        Self {
            width: self.height,
            height: self.width,
        }
    }

    /// Is this sheet wider than it is tall?
    #[must_use]
    pub fn is_landscape(self) -> bool {
        self.width > self.height
    }
}

/// How big the page is and where the picture sits on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PageFit {
    /// The page is the picture, at this resolution in dots per inch.
    Image {
        /// Dots per inch. 72 means one pixel per point.
        dpi: f32,
    },
    /// Every page is this sheet; the picture is scaled to fit and centred.
    Fixed {
        /// The sheet.
        size: PageSize,
        /// Blank border on every side, in points.
        margin: f32,
    },
}

impl Default for PageFit {
    fn default() -> Self {
        // Scans are the common case, and a scan already knows how big it is.
        Self::Image { dpi: 300.0 }
    }
}

/// Settings for one conversion.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Page size and placement.
    pub fit: PageFit,
    /// Turn the sheet sideways for a picture that is wider than it is tall.
    ///
    /// Only meaningful with [`PageFit::Fixed`]: without it, a landscape photo
    /// on a portrait sheet is scaled down to the width of the paper and wastes
    /// half the page.
    pub auto_orient: bool,
}

/// Something that could not be done, said plainly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Warning {
    /// A file was left out of the document.
    Skipped {
        /// What it was called.
        name: String,
        /// Why it was skipped.
        reason: String,
    },
}

impl Warning {
    /// A line fit to print.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Skipped { name, reason } => format!("{name} was skipped: {reason}"),
        }
    }
}

/// A document built from pictures.
#[derive(Debug)]
pub struct Built {
    /// The document.
    pub pdf: Pdf,
    /// How many pages it has, which is how many pictures went in.
    pub pages: usize,
    /// Everything that was left out, and why.
    pub warnings: Vec<Warning>,
}

/// One picture to place.
#[derive(Clone, Copy, Debug)]
pub struct Source<'a> {
    /// What to call it if it has to be reported.
    pub name: &'a str,
    /// The file, as read from disk.
    pub bytes: &'a [u8],
}

/// Build a PDF from pictures, one page each, in the order given.
///
/// Unreadable files are reported as warnings rather than failing the run; an
/// input where *every* file fails is an error, because an empty PDF is not a
/// document and returning one silently would be worse than saying so.
pub fn to_pdf(sources: &[Source<'_>], options: &Options) -> Result<Built> {
    if sources.is_empty() {
        return Err(Error::Config {
            detail: "no images to convert".into(),
            source_path: None,
        });
    }

    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();

    let mut page_ids = Vec::with_capacity(sources.len());
    let mut warnings = Vec::new();

    for source in sources {
        match page_for(&mut document, pages_id, source, options) {
            Ok(id) => page_ids.push(id),
            Err(e) => warnings.push(Warning::Skipped {
                name: source.name.to_string(),
                reason: e.reason().unwrap_or_else(|| e.message()),
            }),
        }
    }

    if page_ids.is_empty() {
        return Err(Error::Config {
            detail: format!(
                "none of the {} file(s) could be read as an image",
                sources.len()
            ),
            source_path: None,
        });
    }

    let count = page_ids.len();
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Count" => count as i64,
            "Kids" => page_ids.iter().map(|id| Object::Reference(*id)).collect::<Vec<_>>(),
        }),
    );

    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    document.trailer.set("Root", catalog);

    let mut bytes = Vec::new();
    document.save_to(&mut bytes).map_err(|e| Error::Backend {
        backend: "lopdf",
        detail: format!("the document could not be written: {e}"),
    })?;

    tracing::info!(
        pages = count,
        skipped = warnings.len(),
        "built a PDF from images"
    );

    Ok(Built {
        pdf: Pdf::from_bytes(&bytes)?,
        pages: count,
        warnings,
    })
}

/// Add one picture as one page.
fn page_for(
    document: &mut Document,
    parent: ObjectId,
    source: &Source<'_>,
    options: &Options,
) -> Result<ObjectId> {
    let placed = image_xobject::place(document, source.bytes)?;
    let (page, box_) = layout(placed.width, placed.height, options);

    // The image XObject is drawn in a 1×1 unit square, so the matrix is the
    // rectangle it should cover: width, height, then the lower-left corner.
    let content = format!(
        "q {:.4} 0 0 {:.4} {:.4} {:.4} cm /Im0 Do Q",
        box_.width, box_.height, box_.x, box_.y
    );
    let contents = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));

    Ok(document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => parent,
        "MediaBox" => vec![
            0.into(),
            0.into(),
            Object::Real(page.width),
            Object::Real(page.height),
        ],
        "Resources" => dictionary! {
            "XObject" => dictionary! { "Im0" => placed.id },
        },
        "Contents" => contents,
    }))
}

/// Where the picture goes on the page.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

/// Work out the page size and the rectangle the picture fills.
fn layout(pixels_wide: u32, pixels_high: u32, options: &Options) -> (PageSize, Rect) {
    #[expect(clippy::cast_precision_loss, reason = "pixel counts, not money")]
    let (width, height) = (pixels_wide as f32, pixels_high as f32);

    match options.fit {
        PageFit::Image { dpi } => {
            // A dpi of zero would be a page of nothing; treat it as "one pixel
            // per point", which is what a PDF means by no scaling at all.
            let scale = if dpi > 0.0 { 72.0 / dpi } else { 1.0 };
            let page = PageSize {
                width: width * scale,
                height: height * scale,
            };
            (
                page,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: page.width,
                    height: page.height,
                },
            )
        }
        PageFit::Fixed { size, margin } => {
            let landscape = width > height;
            let page = if options.auto_orient && landscape != size.is_landscape() {
                size.turned()
            } else {
                size
            };

            // A margin wider than the paper would give a negative box; clamp it
            // so a silly number produces a small picture, not an inside-out one.
            let margin = margin.max(0.0).min(page.width.min(page.height) / 2.0 - 1.0);
            let available_w = (page.width - margin * 2.0).max(1.0);
            let available_h = (page.height - margin * 2.0).max(1.0);

            let scale = (available_w / width).min(available_h / height);
            let drawn_w = width * scale;
            let drawn_h = height * scale;
            (
                page,
                Rect {
                    x: (page.width - drawn_w) / 2.0,
                    y: (page.height - drawn_h) / 2.0,
                    width: drawn_w,
                    height: drawn_h,
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny PNG, built rather than stored: the test should not depend on a
    /// fixture file to say what 2×3 pixels mean.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let image =
            image::RgbImage::from_fn(width, height, |x, _| image::Rgb([(x % 256) as u8, 128, 64]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("the png encodes");
        bytes
    }

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let image =
            image::RgbImage::from_fn(width, height, |_, y| image::Rgb([32, (y % 256) as u8, 200]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .expect("the jpeg encodes");
        bytes
    }

    #[test]
    fn a_page_per_picture_in_the_order_given() {
        let a = png(10, 10);
        let b = jpeg(20, 10);
        let built = to_pdf(
            &[
                Source {
                    name: "a.png",
                    bytes: &a,
                },
                Source {
                    name: "b.jpg",
                    bytes: &b,
                },
            ],
            &Options::default(),
        )
        .expect("builds");

        assert_eq!(built.pages, 2);
        assert_eq!(built.pdf.page_count(), 2);
        assert!(built.warnings.is_empty());
    }

    #[test]
    fn at_300_dpi_the_page_is_the_picture() {
        // 300 pixels at 300 dpi is one inch, which is 72 points.
        let (page, box_) = layout(300, 600, &Options::default());
        assert!((page.width - 72.0).abs() < 0.01, "{page:?}");
        assert!((page.height - 144.0).abs() < 0.01, "{page:?}");
        assert_eq!(box_.x, 0.0);
        assert_eq!(box_.y, 0.0);
    }

    #[test]
    fn a_fixed_page_centres_the_picture_inside_the_margin() {
        let options = Options {
            fit: PageFit::Fixed {
                size: PageSize::A4,
                margin: 36.0,
            },
            auto_orient: false,
        };
        let (page, box_) = layout(1000, 1000, &options);

        assert_eq!(page, PageSize::A4);
        assert!(
            (box_.width - (PageSize::A4.width - 72.0)).abs() < 0.01,
            "a square picture on A4 is limited by the width: {box_:?}"
        );
        assert!((box_.width - box_.height).abs() < 0.01, "still square");
        let left = box_.x;
        let right = page.width - (box_.x + box_.width);
        assert!((left - right).abs() < 0.01, "centred: {left} vs {right}");
    }

    #[test]
    fn a_wide_picture_turns_the_paper_when_asked() {
        let options = Options {
            fit: PageFit::Fixed {
                size: PageSize::A4,
                margin: 0.0,
            },
            auto_orient: true,
        };
        let (page, _) = layout(2000, 1000, &options);
        assert!(
            page.is_landscape(),
            "the sheet should have turned: {page:?}"
        );
        assert_eq!(page, PageSize::A4.turned());
    }

    #[test]
    fn without_auto_orient_the_paper_stays_put() {
        let options = Options {
            fit: PageFit::Fixed {
                size: PageSize::A4,
                margin: 0.0,
            },
            auto_orient: false,
        };
        let (page, box_) = layout(2000, 1000, &options);
        assert_eq!(page, PageSize::A4);
        assert!(
            box_.width <= page.width + 0.01,
            "it still has to fit: {box_:?}"
        );
    }

    #[test]
    fn a_margin_wider_than_the_paper_does_not_turn_the_box_inside_out() {
        let options = Options {
            fit: PageFit::Fixed {
                size: PageSize::A4,
                margin: 10_000.0,
            },
            auto_orient: false,
        };
        let (_, box_) = layout(100, 100, &options);
        assert!(box_.width > 0.0 && box_.height > 0.0, "{box_:?}");
    }

    #[test]
    fn one_unreadable_file_is_a_warning_not_a_failure() {
        let good = png(8, 8);
        let built = to_pdf(
            &[
                Source {
                    name: "good.png",
                    bytes: &good,
                },
                Source {
                    name: "notes.txt",
                    bytes: b"this is not an image",
                },
            ],
            &Options::default(),
        )
        .expect("the good one still converts");

        assert_eq!(built.pages, 1);
        assert_eq!(built.warnings.len(), 1);
        assert!(
            built.warnings[0].message().contains("notes.txt"),
            "{:?}",
            built.warnings[0]
        );
    }

    #[test]
    fn a_run_where_nothing_is_an_image_is_an_error() {
        let result = to_pdf(
            &[Source {
                name: "notes.txt",
                bytes: b"not an image",
            }],
            &Options::default(),
        );
        assert!(result.is_err(), "an empty PDF is not a document");
    }

    #[test]
    fn nothing_at_all_is_an_error() {
        assert!(to_pdf(&[], &Options::default()).is_err());
    }

    #[test]
    fn page_sizes_are_named_case_insensitively() {
        assert_eq!(PageSize::parse("A4").expect("a4"), PageSize::A4);
        assert_eq!(
            PageSize::parse(" letter ").expect("letter"),
            PageSize::LETTER
        );
        assert!(PageSize::parse("foolscap").is_err());
    }
}
