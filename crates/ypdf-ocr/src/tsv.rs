//! Parsing Tesseract's TSV output.
//!
//! The format is one row per layout element, with `level 5` meaning a word.
//! Everything above that level is structure — blocks, paragraphs, lines — whose
//! `text` column is empty, so only level 5 carries anything to place.
//!
//! Columns, in order: `level page_num block_num par_num line_num word_num left
//! top width height conf text`. Parsing is by index rather than by header name
//! because the header has been identical since Tesseract 3.05, and a row that
//! does not fit is skipped rather than guessed at.

/// One recognized word and where it sits, in image pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    /// The text.
    pub text: String,
    /// Distance from the left edge, in pixels.
    pub left: i32,
    /// Distance from the top edge, in pixels.
    pub top: i32,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
    /// Tesseract's confidence, 0-100.
    pub confidence: f32,
    /// Which recognized line this word belongs to.
    ///
    /// Kept so extraction produces lines rather than a bag of words: a text
    /// layer whose words are in the wrong order is worse than no text layer,
    /// because search finds it and shows nonsense.
    pub line: (i32, i32, i32),
}

/// Parse Tesseract TSV into words.
///
/// Rows that are not words, are empty, or fall below `min_confidence` are
/// dropped: a text layer full of low-confidence guesses makes search *worse*,
/// since every wrong hit costs someone a page turn.
#[must_use]
pub fn parse(tsv: &str, min_confidence: f32) -> Vec<Word> {
    let mut words = Vec::new();

    for line in tsv.lines() {
        let columns: Vec<&str> = line.split('\t').collect();
        if columns.len() < 12 {
            continue;
        }
        if columns[0] != "5" {
            // Not a word row — or the header, whose first column is "level".
            continue;
        }

        let text = columns[11].trim();
        if text.is_empty() {
            continue;
        }

        let (Ok(block), Ok(par), Ok(line_num)) = (
            columns[2].parse::<i32>(),
            columns[3].parse::<i32>(),
            columns[4].parse::<i32>(),
        ) else {
            continue;
        };
        let (Ok(left), Ok(top), Ok(width), Ok(height)) = (
            columns[6].parse::<i32>(),
            columns[7].parse::<i32>(),
            columns[8].parse::<i32>(),
            columns[9].parse::<i32>(),
        ) else {
            continue;
        };
        let Ok(confidence) = columns[10].parse::<f32>() else {
            continue;
        };

        if confidence < min_confidence || width <= 0 || height <= 0 {
            continue;
        }

        words.push(Word {
            text: text.to_string(),
            left,
            top,
            width,
            height,
            confidence,
            line: (block, par, line_num),
        });
    }

    words
}

/// Mean confidence across the words, for reporting.
#[must_use]
pub fn mean_confidence(words: &[Word]) -> f32 {
    if words.is_empty() {
        return 0.0;
    }
    #[expect(clippy::cast_precision_loss, reason = "word counts are small")]
    let count = words.len() as f32;
    words.iter().map(|w| w.confidence).sum::<f32>() / count
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext
1\t1\t0\t0\t0\t0\t0\t0\t1224\t1584\t-1\t
2\t1\t1\t0\t0\t0\t100\t120\t900\t40\t-1\t
5\t1\t1\t1\t1\t1\t100\t120\t180\t40\t96.5\tQuarterly
5\t1\t1\t1\t1\t2\t300\t120\t120\t40\t95.1\tReport
5\t1\t1\t1\t2\t1\t100\t200\t90\t36\t12.0\trnaybe
5\t1\t1\t1\t2\t2\t220\t200\t60\t36\t88.0\t
";

    #[test]
    fn only_word_rows_become_words() {
        let words = parse(SAMPLE, 0.0);
        // Two structural rows and one blank-text row are not words.
        assert_eq!(words.len(), 3);
        assert_eq!(words[0].text, "Quarterly");
        assert_eq!(words[0].left, 100);
        assert_eq!(words[0].width, 180);
    }

    #[test]
    fn low_confidence_guesses_are_dropped() {
        // A text layer full of wrong words makes search worse than no layer:
        // every false hit costs a page turn.
        let words = parse(SAMPLE, 60.0);
        assert_eq!(words.len(), 2);
        assert!(words.iter().all(|w| w.text != "rnaybe"));
    }

    #[test]
    fn line_membership_is_kept() {
        let words = parse(SAMPLE, 0.0);
        assert_eq!(words[0].line, words[1].line, "same line");
        assert_ne!(words[0].line, words[2].line, "different line");
    }

    #[test]
    fn a_truncated_row_is_skipped_rather_than_guessed_at() {
        let words = parse("5\t1\t1\t1\t1\t1\t100\t120\n", 0.0);
        assert!(words.is_empty());
    }

    #[test]
    fn a_row_with_unparseable_numbers_is_skipped() {
        let bad = "5\t1\t1\t1\t1\t1\tx\t120\t180\t40\t96.5\tQuarterly\n";
        assert!(parse(bad, 0.0).is_empty());
    }

    #[test]
    fn zero_sized_boxes_are_dropped() {
        let flat = "5\t1\t1\t1\t1\t1\t100\t120\t0\t40\t96.5\tQuarterly\n";
        assert!(parse(flat, 0.0).is_empty());
    }

    #[test]
    fn mean_confidence_of_nothing_is_zero_rather_than_a_division() {
        assert!((mean_confidence(&[]) - 0.0).abs() < f32::EPSILON);
        let words = parse(SAMPLE, 60.0);
        let mean = mean_confidence(&words);
        assert!((90.0..100.0).contains(&mean), "{mean}");
    }
}
