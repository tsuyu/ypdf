//! Helvetica character widths, for placing text where it was asked for.
//!
//! A watermark's glyphs are **visible**, unlike the OCR text layer's, so
//! "centred" has to actually be centred. That needs the width of the string in
//! the font it will be drawn in, which means the font's own metrics.
//!
//! The table covers ASCII, which is what `CONFIDENTIAL`, `DRAFT`, and `COPY`
//! are made of. Anything outside it falls back to an average advance, and the
//! result is a watermark a percent or two off centre — visible only if you
//! measure it, and better than refusing to stamp the page.

/// Widths for Helvetica, in 1/1000 em, for codes 32-126.
///
/// From the Adobe AFM metrics for the base-14 fonts.
const HELVETICA: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722, 722, 667,
    611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500,
    222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584,
];

/// Widths for Helvetica-Bold, same range.
const HELVETICA_BOLD: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556,
    556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, 975, 722, 722, 722, 722, 667,
    611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667,
    667, 611, 333, 278, 333, 584, 556, 333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556,
    278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584,
];

/// Used for anything the table does not cover.
const AVERAGE: u16 = 556;

/// Which of the two faces to measure against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Face {
    /// Helvetica.
    #[default]
    Regular,
    /// Helvetica-Bold.
    Bold,
}

impl Face {
    /// The PDF base font name.
    #[must_use]
    pub const fn base_font(self) -> &'static str {
        match self {
            Self::Regular => "Helvetica",
            Self::Bold => "Helvetica-Bold",
        }
    }

    fn widths(self) -> &'static [u16; 95] {
        match self {
            Self::Regular => &HELVETICA,
            Self::Bold => &HELVETICA_BOLD,
        }
    }
}

/// The width of `text` at `size` points.
#[must_use]
pub fn width_of(text: &str, size: f32, face: Face) -> f32 {
    let widths = face.widths();
    let thousandths: u32 = text
        .chars()
        .map(|ch| {
            let code = ch as u32;
            if (32..=126).contains(&code) {
                // The index is inside the table by the range check above.
                let index = (code - 32) as usize;
                u32::from(widths.get(index).copied().unwrap_or(AVERAGE))
            } else {
                u32::from(AVERAGE)
            }
        })
        .sum();

    #[expect(clippy::cast_precision_loss, reason = "text lengths are small")]
    let ems = thousandths as f32 / 1000.0;
    ems * size
}

/// Cap height as a fraction of the font size, for vertical centring.
///
/// Centring on the full em box puts short capitalized text visibly low, because
/// most of the descender space is empty.
pub const CAP_HEIGHT: f32 = 0.717;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_string_measures_the_way_the_metrics_say() {
        // "AV" in Helvetica: 667 + 667 = 1334/1000 em, so 13.34pt at size 10.
        let width = width_of("AV", 10.0, Face::Regular);
        assert!((width - 13.34).abs() < 0.01, "{width}");
    }

    #[test]
    fn bold_is_wider_than_regular_for_the_same_text() {
        let regular = width_of("CONFIDENTIAL", 24.0, Face::Regular);
        let bold = width_of("CONFIDENTIAL", 24.0, Face::Bold);
        assert!(bold > regular, "{bold} vs {regular}");
    }

    #[test]
    fn width_scales_with_size() {
        let small = width_of("DRAFT", 10.0, Face::Regular);
        let large = width_of("DRAFT", 20.0, Face::Regular);
        assert!((large - small * 2.0).abs() < 0.001);
    }

    #[test]
    fn a_space_is_not_zero_width() {
        // Otherwise "TOP SECRET" centres as though it were one word.
        assert!(width_of(" ", 10.0, Face::Regular) > 0.0);
    }

    #[test]
    fn characters_outside_the_table_fall_back_rather_than_panicking() {
        let width = width_of("\u{6c49}", 10.0, Face::Regular);
        assert!(width > 0.0, "an approximate width, not a crash");
    }

    #[test]
    fn the_empty_string_has_no_width() {
        assert!((width_of("", 24.0, Face::Regular) - 0.0).abs() < f32::EPSILON);
    }
}
