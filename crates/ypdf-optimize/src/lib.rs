//! PDF compression (spec §4).
//!
//! Two kinds of saving, in order of what actually helps: **images**, which are
//! nearly always where a large PDF's size is, and **structure** — unreferenced
//! objects, uncompressed content streams, metadata nobody needs.
//!
//! The report says what was done *and what was refused*. A compressor that
//! silently skips half a document while reporting a percentage is worse than
//! one that saves less and says so, because the first leaves you thinking the
//! file is as small as it can get.
//!
//! ```no_run
//! use ypdf_core::preset::Compression;
//! use ypdf_doc::Pdf;
//! use ypdf_optimize::optimize;
//!
//! # fn main() -> ypdf_core::Result<()> {
//! let mut pdf = Pdf::open("big.pdf")?;
//! let report = optimize(&mut pdf, &Compression::default())?;
//! println!("{}", report.to_human());
//! pdf.save("small.pdf")?;
//! # Ok(())
//! # }
//! ```

mod extract;
mod images;

use std::collections::BTreeMap;

use lopdf::{Object, ObjectId};
use ypdf_core::preset::Compression;
use ypdf_core::{CancelToken, ProgressReporter, Result};
use ypdf_doc::Pdf;

pub use extract::{ExtractedImage, ImageInventory, extract_images};
pub use images::Skipped;

/// What a compression run did (spec §4, "Statistics").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OptimizeReport {
    /// Size before, in bytes.
    pub before: u64,
    /// Size after, in bytes.
    pub after: u64,
    /// Images re-encoded.
    pub images_recompressed: usize,
    /// Bytes saved on images alone.
    pub image_bytes_saved: u64,
    /// Images left alone, and why.
    pub images_skipped: BTreeMap<&'static str, usize>,
    /// Unreferenced objects removed.
    pub objects_removed: usize,
    /// Content streams re-compressed.
    pub streams_compressed: usize,
    /// Whether metadata was stripped.
    pub metadata_removed: bool,
    /// Requested work this build cannot do, named so the report is not silent
    /// about it.
    pub not_supported: Vec<&'static str>,
}

impl OptimizeReport {
    /// Size reduction as a fraction of the original.
    #[must_use]
    pub fn reduction(&self) -> f32 {
        if self.before == 0 {
            return 0.0;
        }
        #[expect(clippy::cast_precision_loss, reason = "display only")]
        let ratio = self.after as f32 / self.before as f32;
        (1.0 - ratio).max(0.0)
    }

    /// Did the file actually get smaller?
    #[must_use]
    pub fn is_improvement(&self) -> bool {
        self.after < self.before
    }

    /// The statistics block from spec §4.
    #[must_use]
    pub fn to_human(&self) -> String {
        let mut out = format!(
            "Original:  {}\nOptimized: {}\nReduction: {:.1}%\n",
            format_size(self.before),
            format_size(self.after),
            self.reduction() * 100.0
        );

        if self.images_recompressed > 0 {
            out.push_str(&format!(
                "\nImages:    {} recompressed, {} saved\n",
                self.images_recompressed,
                format_size(self.image_bytes_saved)
            ));
        }
        if !self.images_skipped.is_empty() {
            let detail: Vec<String> = self
                .images_skipped
                .iter()
                .map(|(why, n)| format!("{n} {why}"))
                .collect();
            out.push_str(&format!("Untouched: {}\n", detail.join(", ")));
        }
        if self.objects_removed > 0 {
            out.push_str(&format!(
                "Objects:   {} unreferenced removed\n",
                self.objects_removed
            ));
        }
        if !self.not_supported.is_empty() {
            out.push_str(&format!("\nNot done:  {}\n", self.not_supported.join(", ")));
        }
        out
    }
}

/// Compress a document in place (spec §4).
///
/// The document is left usable whatever happens: every step either succeeds or
/// leaves its part of the file exactly as it was.
pub fn optimize(pdf: &mut Pdf, settings: &Compression) -> Result<OptimizeReport> {
    optimize_with(
        pdf,
        settings,
        &CancelToken::never(),
        &mut ProgressReporter::silent(),
    )
}

/// Compress a document, reporting progress and honouring cancellation.
pub fn optimize_with(
    pdf: &mut Pdf,
    settings: &Compression,
    cancel: &CancelToken,
    progress: &mut ProgressReporter,
) -> Result<OptimizeReport> {
    let before = pdf.to_bytes()?.len() as u64;
    let mut report = OptimizeReport {
        before,
        ..OptimizeReport::default()
    };

    // Lossless means "do not throw pixels away", not "do nothing": resampling
    // an image that is drawn at a fraction of its size is still lossless in the
    // only sense that matters, but re-encoding to JPEG is not.
    let lossless = settings.quality >= 100;

    progress.set_stage("recompressing images");
    cancel.check()?;
    let outcomes = images::optimize_images(
        pdf.raw_mut(),
        images::ImageSettings {
            dpi: settings.dpi,
            quality: settings.quality,
            lossless,
        },
    );

    for outcome in outcomes {
        match outcome {
            images::Outcome::Recompressed { before, after } => {
                report.images_recompressed += 1;
                report.image_bytes_saved += (before.saturating_sub(after)) as u64;
            }
            images::Outcome::Skipped(reason) => {
                *report.images_skipped.entry(reason.describe()).or_insert(0) += 1;
            }
        }
    }

    if settings.remove_unused_objects {
        progress.set_stage("removing unreferenced objects");
        cancel.check()?;
        report.objects_removed = prune(pdf);
    }

    if settings.compress_streams {
        progress.set_stage("compressing streams");
        cancel.check()?;
        report.streams_compressed = compress_streams(pdf);
    }

    if settings.linearize {
        // Linearization means rewriting the file with a hint table and a
        // specific object order. Claiming it without doing it would make the
        // report a lie.
        report.not_supported.push("linearization");
    }
    if settings.optimize_fonts {
        report.not_supported.push("font subsetting");
    }

    cancel.check()?;
    report.after = pdf.to_bytes()?.len() as u64;
    progress.set_stage("done");

    tracing::info!(
        before = report.before,
        after = report.after,
        images = report.images_recompressed,
        "compression finished"
    );

    Ok(report)
}

/// Strip document metadata (spec §4, "Remove metadata").
///
/// Separate from [`optimize`] because it is a privacy decision, not a size one:
/// the saving is negligible and the effect — losing the author, the dates, the
/// XMP — is not something to bundle into "make this smaller".
pub fn strip_metadata(pdf: &mut Pdf) -> usize {
    let doc = pdf.raw_mut();
    let mut removed = 0;

    if doc.trailer.remove(b"Info".as_slice()).is_some() {
        removed += 1;
    }
    if let Ok(catalog) = doc.catalog_mut()
        && catalog.remove(b"Metadata".as_slice()).is_some()
    {
        removed += 1;
    }
    removed
}

/// Remove objects nothing references.
fn prune(pdf: &mut Pdf) -> usize {
    let before = pdf.raw().objects.len();
    pdf.raw_mut().prune_objects();
    before.saturating_sub(pdf.raw().objects.len())
}

/// Deflate content streams that are stored uncompressed.
fn compress_streams(pdf: &mut Pdf) -> usize {
    let doc = pdf.raw_mut();
    let candidates: Vec<ObjectId> = doc
        .objects
        .iter()
        .filter(|(_, object)| {
            object.as_stream().is_ok_and(|stream| {
                // Only streams with no filter at all: anything already
                // compressed is someone else's decision, and images were
                // handled above.
                !stream.dict.has(b"Filter")
                    && !stream.dict.has(b"Subtype")
                    && stream.content.len() > 512
            })
        })
        .map(|(id, _)| *id)
        .collect();

    let mut compressed = 0;
    for id in candidates {
        let Some(Object::Stream(stream)) = doc.objects.get_mut(&id) else {
            continue;
        };
        let original = stream.content.len();
        if stream.compress().is_ok() && stream.content.len() < original {
            compressed += 1;
        }
    }
    compressed
}

/// Human-readable byte count.
#[must_use]
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    #[expect(clippy::cast_precision_loss, reason = "display only")]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_statistics_block_matches_the_spec_shape() {
        let report = OptimizeReport {
            before: 48_200_000,
            after: 12_700_000,
            images_recompressed: 12,
            image_bytes_saved: 35_000_000,
            ..OptimizeReport::default()
        };
        let text = report.to_human();

        assert!(text.contains("Original:"));
        assert!(text.contains("Optimized:"));
        assert!(text.contains("73.7%"), "got {text}");
        assert!(report.is_improvement());
    }

    #[test]
    fn a_file_that_grew_is_not_an_improvement() {
        let report = OptimizeReport {
            before: 100,
            after: 120,
            ..OptimizeReport::default()
        };
        assert!(!report.is_improvement());
        assert_eq!(
            report.reduction(),
            0.0,
            "a negative reduction is reported as none"
        );
    }

    #[test]
    fn unsupported_work_is_named_rather_than_hidden() {
        let report = OptimizeReport {
            before: 100,
            after: 90,
            not_supported: vec!["linearization"],
            ..OptimizeReport::default()
        };
        assert!(report.to_human().contains("Not done:  linearization"));
    }

    #[test]
    fn skipped_images_are_reported_with_their_reasons() {
        let mut skipped = BTreeMap::new();
        skipped.insert("unsupported filter", 3);
        skipped.insert("already small", 40);
        let report = OptimizeReport {
            before: 100,
            after: 80,
            images_skipped: skipped,
            ..OptimizeReport::default()
        };

        let text = report.to_human();
        assert!(text.contains("40 already small"), "got {text}");
        assert!(text.contains("3 unsupported filter"), "got {text}");
    }

    #[test]
    fn an_empty_document_does_not_divide_by_zero() {
        assert_eq!(OptimizeReport::default().reduction(), 0.0);
    }

    #[test]
    fn sizes_render_readably() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(48_200_000), "46.0 MB");
    }
}
