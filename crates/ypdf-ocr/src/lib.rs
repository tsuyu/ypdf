//! OCR: making a scan searchable without changing what it shows (spec §7).
//!
//! The rule this crate is built on: **the scan is never modified.** OCR adds an
//! invisible text layer over the page and nothing else. If the recognition is
//! poor, the document still shows exactly what it always showed — a bad text
//! layer costs a wrong search hit, not a lost page. Replacing the image with
//! recognized text would be the other trade, and no amount of accuracy makes it
//! a good one for someone's only copy of a document.
//!
//! Tesseract is driven as a **sidecar process** rather than linked. See
//! [`engine`] for why, and for what happens when it is not installed.
//!
//! ```no_run
//! use ypdf_doc::Pdf;
//! use ypdf_ocr::{OcrSettings, Tesseract, ocr_page};
//!
//! # fn main() -> ypdf_core::Result<()> {
//! let engine = Tesseract::find()?;
//! let mut pdf = Pdf::open("scan.pdf")?;
//! // `rgba` comes from the renderer; OCR never rasterizes anything itself.
//! # let (rgba, width, height) = (vec![], 0, 0);
//! let report = ocr_page(
//!     &engine,
//!     &mut pdf,
//!     0,
//!     &rgba,
//!     (width, height),
//!     &OcrSettings::default(),
//! )?;
//! println!("{} words", report.words);
//! pdf.save("scan-searchable.pdf")?;
//! # Ok(())
//! # }
//! ```

pub mod engine;
mod layer;
mod tsv;

use std::io::Write as _;

use ypdf_core::{Error, Result};
use ypdf_doc::Pdf;

pub use engine::Tesseract;
pub use layer::LayerReport;
pub use tsv::Word;

/// How to run recognition.
#[derive(Clone, Debug)]
pub struct OcrSettings {
    /// Tesseract language specification, e.g. `eng` or `eng+msa`.
    pub language: String,
    /// Words below this confidence are discarded.
    pub min_confidence: f32,
    /// Skip pages that already have extractable text.
    ///
    /// On by default: a page with real text does not need a guess laid over it,
    /// and adding one gives search two answers for the same words.
    pub skip_pages_with_text: bool,
}

impl Default for OcrSettings {
    fn default() -> Self {
        Self {
            language: "eng".to_string(),
            // Tesseract's own confidences run low on clean scans of small text;
            // 40 drops the nonsense without discarding usable words.
            min_confidence: 40.0,
            skip_pages_with_text: true,
        }
    }
}

/// What recognizing one page produced.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PageReport {
    /// Words placed in the text layer.
    pub words: usize,
    /// Words recognized but dropped because the layer font cannot encode them.
    pub unencodable: usize,
    /// Mean confidence across the words that were kept.
    pub confidence: f32,
    /// The page already had text, so nothing was done.
    pub skipped_has_text: bool,
}

/// Does this page look like a scan — an image with no text of its own?
///
/// The signal diagnostics already reports: a page with no extractable text is
/// either empty or a picture of words.
#[must_use]
pub fn page_has_text(pdf: &Pdf, page: u32) -> bool {
    pdf.raw()
        .extract_text(&[page + 1])
        .is_ok_and(|text| text.trim().chars().any(char::is_alphanumeric))
}

/// Recognize one page and add an invisible text layer for it.
///
/// `rgba` is the rendered page, `size_px` its dimensions. Rasterizing is the
/// caller's job: PDFium lives on one dedicated thread, and this crate has no
/// business knowing about it.
pub fn ocr_page(
    engine: &Tesseract,
    pdf: &mut Pdf,
    page: u32,
    rgba: &[u8],
    size_px: (u32, u32),
    settings: &OcrSettings,
) -> Result<PageReport> {
    if settings.skip_pages_with_text && page_has_text(pdf, page) {
        return Ok(PageReport {
            skipped_has_text: true,
            ..PageReport::default()
        });
    }

    let page_id = *pdf
        .page_ids()
        .get(page as usize)
        .ok_or(Error::PageOutOfRange {
            requested: page + 1,
            pages: pdf.page_count(),
        })?;
    let page_size_pt = page_size(pdf, page)?;

    // Tesseract reads a file, so the raster goes to a temporary one that is
    // removed whatever happens next.
    let image = TempImage::write(rgba, size_px)?;
    let output = engine.recognize_tsv(image.path(), &settings.language)?;
    let words = tsv::parse(&output, settings.min_confidence);
    let confidence = tsv::mean_confidence(&words);

    let report = layer::add_text_layer(pdf.raw_mut(), page_id, &words, size_px, page_size_pt);

    tracing::info!(
        page = page + 1,
        words = report.placed,
        unencodable = report.unencodable,
        confidence,
        "page recognized"
    );

    Ok(PageReport {
        words: report.placed,
        unencodable: report.unencodable,
        confidence,
        skipped_has_text: false,
    })
}

/// The page's size in points, from its media box.
fn page_size(pdf: &Pdf, page: u32) -> Result<(f32, f32)> {
    let page_id = *pdf
        .page_ids()
        .get(page as usize)
        .ok_or(Error::PageOutOfRange {
            requested: page + 1,
            pages: pdf.page_count(),
        })?;

    let media_box = pdf
        .raw()
        .get_dictionary(page_id)
        .ok()
        .and_then(|dict| dict.get(b"MediaBox").ok()?.as_array().ok().cloned());

    let Some(values) = media_box else {
        // US Letter, the same default the rest of the engine uses.
        return Ok((612.0, 792.0));
    };

    let number = |index: usize| -> f32 {
        values
            .get(index)
            .and_then(|value| value.as_float().ok())
            .unwrap_or(0.0)
    };

    let width = number(2) - number(0);
    let height = number(3) - number(1);
    if width <= 0.0 || height <= 0.0 {
        return Ok((612.0, 792.0));
    }
    Ok((width, height))
}

/// A PNG on disk that removes itself.
#[derive(Debug)]
struct TempImage {
    path: std::path::PathBuf,
}

impl TempImage {
    fn write(rgba: &[u8], size: (u32, u32)) -> Result<Self> {
        let expected = (size.0 as usize) * (size.1 as usize) * 4;
        if rgba.len() < expected {
            return Err(Error::Config {
                detail: format!(
                    "the raster is {} bytes; {}x{} RGBA needs {expected}",
                    rgba.len(),
                    size.0,
                    size.1
                ),
                source_path: None,
            });
        }

        let buffer = image::RgbaImage::from_raw(size.0, size.1, rgba[..expected].to_vec())
            .ok_or_else(|| Error::Config {
                detail: "the raster does not match its stated size".into(),
                source_path: None,
            })?;

        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(buffer)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| Error::Backend {
                backend: "image",
                detail: e.to_string(),
            })?;

        // A name unique to this process and this moment: OCR runs in parallel
        // over pages, and two of them sharing a file would recognize the wrong
        // page or nothing at all.
        let name = format!(
            "ypdf-ocr-{}-{:?}.png",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let path = std::env::temp_dir().join(name);

        let mut file = std::fs::File::create(&path).map_err(|e| Error::io(&path, e))?;
        file.write_all(&png).map_err(|e| Error::io(&path, e))?;

        Ok(Self { path })
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempImage {
    fn drop(&mut self) {
        // The page image is the user's document. Leaving copies of it in a
        // world-readable temporary directory is a privacy leak, not a
        // housekeeping detail (spec §27).
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    #[test]
    fn a_page_with_real_text_is_recognized_as_having_text() {
        let pdf = Pdf::open(fixture("many-pages.pdf")).expect("opens");
        assert!(page_has_text(&pdf, 0));
    }

    #[test]
    fn the_default_settings_do_not_overwrite_pages_that_already_read() {
        let settings = OcrSettings::default();
        assert!(settings.skip_pages_with_text);
        assert_eq!(settings.language, "eng");
    }

    #[test]
    fn the_page_size_comes_from_the_media_box() {
        let pdf = Pdf::open(fixture("many-pages.pdf")).expect("opens");
        let size = page_size(&pdf, 0).expect("a size");
        assert_eq!(size, (612.0, 792.0));
    }

    #[test]
    fn a_page_beyond_the_end_is_refused() {
        let pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        let error = page_size(&pdf, 9).expect_err("no such page");
        assert_eq!(error.code(), "E_PAGE_RANGE");
    }

    #[test]
    fn a_raster_that_does_not_match_its_size_is_refused() {
        let error = TempImage::write(&[0; 16], (100, 100)).expect_err("too small");
        assert_eq!(error.code(), "E_CONFIG");
    }

    #[test]
    fn the_temporary_image_is_removed_when_it_is_dropped() {
        // The page image is the user's document; leaving it in the temporary
        // directory would be a privacy leak.
        let path = {
            let image = TempImage::write(&[255; 4 * 4 * 4], (4, 4)).expect("writes");
            let path = image.path().to_path_buf();
            assert!(path.is_file());
            path
        };
        assert!(!path.exists(), "the temporary file outlived its owner");
    }
}
