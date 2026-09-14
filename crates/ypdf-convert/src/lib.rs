//! PDF to Markdown (spec §5).
//!
//! A PDF says where glyphs go. It does not say what a paragraph is, where a
//! heading starts, or which order two columns should be read in. Everything
//! this crate produces is therefore **inferred**, and the spec's rule for
//! conversion applies: report the quality rather than promise the layout.
//! That is what [`Warning`] is for — a caller that ignores it is claiming more
//! than this code can deliver.
//!
//! The pipeline is three steps, each testable on its own:
//!
//! 1. [`lines`] — characters into lines, by vertical band.
//! 2. [`blocks`] — lines into headings, paragraphs, and list items, by size,
//!    weight, and the gaps between them.
//! 3. [`markdown`] — blocks into text.
//!
//! It takes a [`PageText`] rather than a file, so the guessing can be tested
//! against pages laid out by hand where the right answer is known. Getting a
//! `PageText` out of a PDF is `ypdf-render`'s job.
//!
//! # What it does not do
//!
//! Tables, multi-column reading order, images, footnotes, and repeated headers
//! and footers. Columns are detected only so they can be reported; the text is
//! still emitted in the order the page gives it, which for a two-column page
//! means interleaved lines. A page whose text layer is empty is almost always
//! a scan, and needs OCR before there is anything here to convert.

pub mod blocks;
pub mod docx;
pub mod lines;
pub mod markdown;

#[cfg(test)]
mod testing;

use ypdf_render::PageText;

pub use blocks::{Block, DocumentStyle};
pub use lines::{Line, Run};

/// Something the caller should know about the conversion it just got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Warning {
    /// The page has no text layer at all. Almost always a scan.
    NoTextLayer {
        /// 1-based page number.
        page: u32,
    },
    /// The page looks like it is set in columns, which this converter reads
    /// straight across rather than down.
    PossibleColumns {
        /// 1-based page number.
        page: u32,
    },
}

impl Warning {
    /// A sentence fit to show a user.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NoTextLayer { page } => format!(
                "page {page} has no text layer — it is probably a scan, and needs OCR first"
            ),
            Self::PossibleColumns { page } => format!(
                "page {page} looks like it is set in columns; lines are read across, not down, so the text will be interleaved"
            ),
        }
    }
}

/// A finished Markdown conversion.
#[derive(Clone, Debug, Default)]
pub struct Conversion {
    /// The Markdown.
    pub markdown: String,
    /// What was inferred badly, or could not be inferred at all.
    pub warnings: Vec<Warning>,
}

/// A finished Word conversion.
#[derive(Clone, Debug, Default)]
pub struct Docx {
    /// The `.docx` file.
    pub bytes: Vec<u8>,
    /// What was inferred badly, or could not be inferred at all.
    pub warnings: Vec<Warning>,
}

/// A document's rebuilt structure, and everything that could not be rebuilt.
///
/// The output formats are writers over this: the guessing happens once, and
/// Markdown and Word are two ways of writing down the same answer.
#[derive(Clone, Debug, Default)]
pub struct Document {
    /// The blocks, in reading order across every page.
    pub blocks: Vec<Block>,
    /// What to tell the caller.
    pub warnings: Vec<Warning>,
}

/// Rebuild a document's structure from its pages.
///
/// Pages are given in order; the text of each is what `ypdf-render` extracted.
#[must_use]
pub fn read(pages: &[PageText]) -> Document {
    let per_page: Vec<Vec<Line>> = pages.iter().map(lines::lines_of).collect();
    let style = blocks::style_of(&per_page);

    let mut warnings = Vec::new();
    let mut all = Vec::new();

    for (i, lines) in per_page.iter().enumerate() {
        let number = u32::try_from(i + 1).unwrap_or(u32::MAX);
        if lines.is_empty() {
            warnings.push(Warning::NoTextLayer { page: number });
            continue;
        }
        if looks_like_columns(lines) {
            warnings.push(Warning::PossibleColumns { page: number });
        }
        all.extend(blocks::blocks_of(lines, &style));
    }

    tracing::info!(
        pages = pages.len(),
        body_size = style.body_size,
        headings = style.heading_sizes.len(),
        blocks = all.len(),
        warnings = warnings.len(),
        "rebuilt document structure"
    );

    Document {
        blocks: all,
        warnings,
    }
}

/// Convert a document's pages to Markdown.
#[must_use]
pub fn to_markdown(pages: &[PageText]) -> Conversion {
    let document = read(pages);
    Conversion {
        markdown: markdown::render(&document.blocks),
        warnings: document.warnings,
    }
}

/// Convert a document's pages to a Word document.
#[must_use]
pub fn to_docx(pages: &[PageText]) -> Docx {
    let document = read(pages);
    Docx {
        bytes: docx::write(&document.blocks),
        warnings: document.warnings,
    }
}

/// A page has to have at least this many lines before its layout says anything.
const MIN_LINES_FOR_COLUMNS: usize = 6;

/// Whether a page looks like it is set in columns.
///
/// Not by looking for a gap between lines: two columns sit at the same
/// heights, so their lines are gathered into single lines that run straight
/// across the gutter. The gutter shows up as a hole *inside* those lines,
/// always at about the same place — which is exactly what this looks for, and
/// is also precisely the text that will come out interleaved.
fn looks_like_columns(lines: &[Line]) -> bool {
    if lines.len() < MIN_LINES_FOR_COLUMNS {
        return false;
    }

    let left = lines.iter().fold(f32::MAX, |acc, l| acc.min(l.left));
    let right = lines.iter().fold(f32::MIN, |acc, l| acc.max(l.right));
    let span = right - left;
    if span <= 0.0 {
        return false;
    }

    // Narrower than a twelfth of the page and it is word spacing, not a gutter.
    let centres: Vec<f32> = lines
        .iter()
        .filter_map(|line| line.widest_gap)
        .filter(|gap| gap.width >= span / 12.0)
        .map(|gap| gap.centre)
        .collect();

    // A gutter runs down most of the page. A handful of wide gaps is a table,
    // a ragged right edge, or one line of spaced-out capitals.
    if centres.len() * 5 < lines.len() * 2 {
        return false;
    }

    let lowest = centres.iter().fold(f32::MAX, |acc, c| acc.min(*c));
    let highest = centres.iter().fold(f32::MIN, |acc, c| acc.max(*c));
    highest - lowest <= span / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use testing::{page, placed_line};

    #[test]
    fn a_page_with_no_text_is_reported_rather_than_silently_empty() {
        let converted = to_markdown(&[PageText::default()]);
        assert!(converted.markdown.is_empty());
        assert_eq!(converted.warnings, vec![Warning::NoTextLayer { page: 1 }]);
        assert!(converted.warnings[0].message().contains("OCR"));
    }

    #[test]
    fn a_title_and_a_paragraph_come_out_as_markdown() {
        let text = page(&[
            placed_line("The Title", 72.0, 700.0, 20.0),
            placed_line("Body text that carries on", 72.0, 660.0, 10.0),
            placed_line("across two lines.", 72.0, 648.0, 10.0),
        ]);
        let converted = to_markdown(&[text]);
        assert_eq!(
            converted.markdown,
            "# The Title\n\nBody text that carries on across two lines."
        );
        assert!(converted.warnings.is_empty());
    }

    #[test]
    fn two_columns_are_reported_because_they_are_read_across() {
        // Eight rows, each with text in both columns at the same height —
        // which is what makes them merge into one line apiece.
        let mut rows = Vec::new();
        for i in 0..8 {
            #[expect(clippy::cast_precision_loss, reason = "test fixture")]
            let y = 700.0 - i as f32 * 12.0;
            rows.push(placed_line("left column text", 72.0, y, 10.0));
            rows.push(placed_line("right column text", 320.0, y, 10.0));
        }
        let converted = to_markdown(&[page(&rows)]);
        assert!(
            converted
                .warnings
                .contains(&Warning::PossibleColumns { page: 1 }),
            "got {:?}",
            converted.warnings
        );
        assert!(
            converted.warnings[0].message().contains("interleaved"),
            "the warning has to say what will actually be wrong with the output"
        );
    }

    #[test]
    fn a_single_column_page_is_not_mistaken_for_two() {
        let rows: Vec<_> = (0..8)
            .map(|i| {
                #[expect(clippy::cast_precision_loss, reason = "test fixture")]
                let y = 700.0 - i as f32 * 12.0;
                placed_line("an ordinary line of running text", 72.0, y, 10.0)
            })
            .collect();
        let converted = to_markdown(&[page(&rows)]);
        assert!(converted.warnings.is_empty(), "{:?}", converted.warnings);
    }

    #[test]
    fn page_numbers_in_warnings_are_one_based() {
        let text = page(&[placed_line("only page two has text", 72.0, 700.0, 10.0)]);
        let converted = to_markdown(&[PageText::default(), text]);
        assert_eq!(converted.warnings, vec![Warning::NoTextLayer { page: 1 }]);
    }
}
