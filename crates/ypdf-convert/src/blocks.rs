//! Lines into blocks.
//!
//! This is where the guessing happens, and it is worth being honest about
//! that: a PDF says where glyphs go, not what they mean. A heading is a line
//! that looks like one — larger than the text around it, or bold and short.
//! A paragraph is a run of lines with no unusual gap between them.
//!
//! Every threshold here is relative to the document's own body size rather
//! than an absolute point value, because 14pt is a heading in a 10pt document
//! and body text in a 14pt one.

use crate::lines::{Line, Run};

/// One piece of reconstructed document structure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// A heading, with `level` from 1 to 6.
    Heading { level: u8, runs: Vec<Run> },
    /// A run of body text.
    Paragraph { runs: Vec<Run> },
    /// One item of a list.
    ListItem { runs: Vec<Run>, ordered: bool },
}

/// What the document as a whole looks like.
///
/// Read from every page before any single page is turned into blocks: a line
/// can only be called large by comparison with the rest of the document.
#[derive(Clone, Debug, Default)]
pub struct DocumentStyle {
    /// The size most of the document's text is set at.
    pub body_size: f32,
    /// Sizes that stand above the body, largest first. Position in this list
    /// is the heading level.
    pub heading_sizes: Vec<f32>,
}

/// A line has to be this much larger than the body to count as a heading.
///
/// Ten per cent is within the noise of a text matrix; a quarter is a size
/// step someone chose on purpose.
const HEADING_RATIO: f32 = 1.15;

/// A gap larger than this fraction of the body size ends a paragraph.
const PARAGRAPH_GAP: f32 = 0.6;

/// Longer than this and a bold line is a sentence, not a heading.
const HEADING_MAX_CHARS: usize = 80;

/// Work out the document's body size and heading sizes.
#[must_use]
pub fn style_of(pages: &[Vec<Line>]) -> DocumentStyle {
    // Weighted by characters, not by lines: one huge title line must not
    // outvote the paragraphs underneath it.
    let mut weights: Vec<(f32, usize)> = Vec::new();
    for line in pages.iter().flatten() {
        let size = (line.size * 2.0).round() / 2.0;
        let n = line.text().trim().chars().count();
        match weights
            .iter_mut()
            .find(|(s, _)| (*s - size).abs() < f32::EPSILON)
        {
            Some((_, count)) => *count += n,
            None => weights.push((size, n)),
        }
    }

    let body_size = weights
        .iter()
        .max_by_key(|(_, n)| *n)
        .map_or(0.0, |(size, _)| *size);

    let mut heading_sizes: Vec<f32> = weights
        .iter()
        .map(|(size, _)| *size)
        .filter(|size| *size > body_size * HEADING_RATIO)
        .collect();
    heading_sizes.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    heading_sizes.truncate(6);

    DocumentStyle {
        body_size,
        heading_sizes,
    }
}

/// Turn one page's lines into blocks.
#[must_use]
pub fn blocks_of(lines: &[Line], style: &DocumentStyle) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut open: Option<(Vec<Run>, bool, bool)> = None; // runs, ordered, is_list

    for (i, line) in lines.iter().enumerate() {
        let previous = i.checked_sub(1).and_then(|p| lines.get(p));
        let broken = previous.is_some_and(|p| ends_paragraph(p, line, style));

        if let Some(level) = heading_level(line, style) {
            flush(&mut blocks, open.take());
            blocks.push(Block::Heading {
                level,
                runs: strip_emphasis(line.runs.clone()),
            });
            continue;
        }

        if let Some((marker, ordered)) = list_marker(&line.text()) {
            flush(&mut blocks, open.take());
            open = Some((without_prefix(line.runs.clone(), marker), ordered, true));
            continue;
        }

        match open.as_mut() {
            // A line that follows on with no unusual gap continues whatever is
            // open, list item or paragraph alike.
            Some((runs, _, _)) if !broken => join(runs, &line.runs),
            _ => {
                flush(&mut blocks, open.take());
                open = Some((line.runs.clone(), false, false));
            }
        }
    }

    flush(&mut blocks, open);
    blocks
}

/// Whether the gap between two lines ends whatever was open.
fn ends_paragraph(previous: &Line, line: &Line, style: &DocumentStyle) -> bool {
    let body = if style.body_size > 0.0 {
        style.body_size
    } else {
        line.size.max(1.0)
    };

    let gap = previous.bottom - line.top;
    if gap > body * PARAGRAPH_GAP {
        return true;
    }

    // A first line indented under the one above starts a new paragraph even
    // when the leading is unchanged, which is how most printed books set them.
    line.left - previous.left > body
}

/// The heading level of a line, if it is one.
fn heading_level(line: &Line, style: &DocumentStyle) -> Option<u8> {
    let text = line.text();
    let text = text.trim();
    if text.is_empty() || list_marker(text).is_some() {
        return None;
    }

    let rank = style
        .heading_sizes
        .iter()
        .position(|size| (line.size - size).abs() < f32::EPSILON);
    if let Some(rank) = rank {
        return u8::try_from(rank + 1).ok().map(|level| level.min(6));
    }

    // No size step, but bold and short reads as a heading — the commonest way
    // a plain word processor marks one.
    if line.is_mostly_bold()
        && text.chars().count() <= HEADING_MAX_CHARS
        && line.size >= style.body_size
    {
        let level = style.heading_sizes.len() + 1;
        return u8::try_from(level).ok().map(|level| level.min(6));
    }

    None
}

/// The list marker a line opens with, if any, and whether it is numbered.
fn list_marker(text: &str) -> Option<(&str, bool)> {
    let trimmed = text.trim_start();

    for bullet in ["•", "‣", "▪", "·", "-", "–", "—", "*"] {
        if let Some(rest) = trimmed.strip_prefix(bullet)
            && rest.starts_with(char::is_whitespace)
        {
            return Some((bullet, false));
        }
    }

    // `1.` or `1)`, up to three digits: more than that and it is a year or a
    // measurement, not a list.
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty() && digits.len() <= 3 {
        let rest = &trimmed[digits.len()..];
        if let Some(after) = rest.strip_prefix(['.', ')'])
            && after.starts_with(char::is_whitespace)
        {
            return Some((&trimmed[..=digits.len()], true));
        }
    }

    None
}

/// Remove a list marker from the front of a line's runs.
fn without_prefix(mut runs: Vec<Run>, marker: &str) -> Vec<Run> {
    if let Some(first) = runs.first_mut() {
        let text = first.text.trim_start();
        let text = text.strip_prefix(marker).unwrap_or(text);
        first.text = text.trim_start().to_string();
    }
    runs.retain(|run| !run.text.is_empty());
    runs
}

/// Append one line's runs to an open block.
fn join(runs: &mut Vec<Run>, next: &[Run]) {
    // A word broken across a line break is rejoined; anything else gets the
    // space the line break stood for.
    let hyphenated = runs
        .last()
        .is_some_and(|run| run.text.ends_with('-') && !run.text.ends_with("--"));

    if hyphenated {
        if let Some(last) = runs.last_mut() {
            last.text.pop();
        }
    } else if let Some(last) = runs.last_mut()
        && !last.text.ends_with(' ')
    {
        last.text.push(' ');
    }

    for run in next {
        match runs.last_mut() {
            Some(open) if open.bold == run.bold && open.italic == run.italic => {
                open.text.push_str(&run.text);
            }
            _ => runs.push(run.clone()),
        }
    }
}

/// A heading is already strong; marking words inside it as well reads as noise.
fn strip_emphasis(runs: Vec<Run>) -> Vec<Run> {
    let text: String = runs.iter().map(|r| r.text.as_str()).collect();
    vec![Run {
        text,
        bold: false,
        italic: false,
    }]
}

fn flush(blocks: &mut Vec<Block>, open: Option<(Vec<Run>, bool, bool)>) {
    let Some((runs, ordered, is_list)) = open else {
        return;
    };
    if runs.iter().all(|run| run.text.trim().is_empty()) {
        return;
    }
    blocks.push(if is_list {
        Block::ListItem { runs, ordered }
    } else {
        Block::Paragraph { runs }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::lines_of;
    use crate::testing::{bold, page, placed_line};

    fn lines(rows: &[(&str, f32, f32)]) -> Vec<Line> {
        let glyphs: Vec<Vec<_>> = rows
            .iter()
            .map(|(text, y, size)| placed_line(text, 72.0, *y, *size))
            .collect();
        lines_of(&page(&glyphs))
    }

    #[test]
    fn the_body_size_is_the_one_most_of_the_text_is_set_in() {
        let page = lines(&[
            ("A Very Large Title", 700.0, 24.0),
            ("Body text here that runs on", 680.0, 10.0),
            ("and continues at the same size", 668.0, 10.0),
        ]);
        let style = style_of(&[page]);
        assert!((style.body_size - 10.0).abs() < 0.01, "{style:?}");
        assert_eq!(style.heading_sizes, vec![24.0]);
    }

    #[test]
    fn a_larger_line_becomes_a_heading_and_the_rest_a_paragraph() {
        let page = lines(&[
            ("Introduction", 700.0, 18.0),
            ("The first sentence.", 676.0, 10.0),
        ]);
        let style = style_of(std::slice::from_ref(&page));
        let blocks = blocks_of(&page, &style);

        assert_eq!(
            blocks,
            vec![
                Block::Heading {
                    level: 1,
                    runs: vec![Run {
                        text: "Introduction".to_string(),
                        bold: false,
                        italic: false
                    }]
                },
                Block::Paragraph {
                    runs: vec![Run {
                        text: "The first sentence.".to_string(),
                        bold: false,
                        italic: false
                    }]
                },
            ]
        );
    }

    #[test]
    fn heading_levels_follow_the_order_of_the_sizes() {
        // The body has to carry most of the characters, because that is what
        // decides which size counts as body in the first place.
        let page = lines(&[
            ("Title", 700.0, 24.0),
            ("Section", 670.0, 16.0),
            (
                "body text long enough to outweigh the headings it sits under",
                650.0,
                10.0,
            ),
        ]);
        let style = style_of(std::slice::from_ref(&page));
        let blocks = blocks_of(&page, &style);
        assert!(matches!(blocks[0], Block::Heading { level: 1, .. }));
        assert!(matches!(blocks[1], Block::Heading { level: 2, .. }));
        assert!(matches!(blocks[2], Block::Paragraph { .. }));
    }

    #[test]
    fn close_lines_join_into_one_paragraph_and_a_gap_starts_another() {
        let page = lines(&[
            ("The first line", 700.0, 10.0),
            ("runs straight on", 688.0, 10.0),
            ("but this one is apart", 650.0, 10.0),
        ]);
        let style = style_of(std::slice::from_ref(&page));
        let blocks = blocks_of(&page, &style);

        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            Block::Paragraph { runs } => {
                let text: String = runs.iter().map(|r| r.text.as_str()).collect();
                assert_eq!(text, "The first line runs straight on");
            }
            other => panic!("expected a paragraph, got {other:?}"),
        }
    }

    #[test]
    fn a_word_broken_across_lines_is_rejoined_without_a_space() {
        let page = lines(&[("hyphen-", 700.0, 10.0), ("ation", 688.0, 10.0)]);
        let style = style_of(std::slice::from_ref(&page));
        match &blocks_of(&page, &style)[0] {
            Block::Paragraph { runs } => {
                let text: String = runs.iter().map(|r| r.text.as_str()).collect();
                assert_eq!(text, "hyphenation");
            }
            other => panic!("expected a paragraph, got {other:?}"),
        }
    }

    #[test]
    fn bulleted_and_numbered_lines_become_list_items_without_their_markers() {
        let page = lines(&[
            ("- first", 700.0, 10.0),
            ("2. second", 660.0, 10.0),
            ("1999. not a list", 620.0, 10.0),
        ]);
        let style = style_of(std::slice::from_ref(&page));
        let blocks = blocks_of(&page, &style);

        assert_eq!(
            blocks[0],
            Block::ListItem {
                runs: vec![Run {
                    text: "first".to_string(),
                    bold: false,
                    italic: false
                }],
                ordered: false
            }
        );
        assert!(matches!(blocks[1], Block::ListItem { ordered: true, .. }));
        assert!(
            matches!(blocks[2], Block::Paragraph { .. }),
            "a four-digit year is not a list marker: {:?}",
            blocks[2]
        );
    }

    #[test]
    fn a_short_bold_line_at_body_size_still_reads_as_a_heading() {
        let glyphs = vec![
            bold(placed_line("Methods", 72.0, 700.0, 10.0)),
            placed_line("We did the following.", 72.0, 686.0, 10.0),
        ];
        let page = lines_of(&page(&glyphs));
        let style = style_of(std::slice::from_ref(&page));
        let blocks = blocks_of(&page, &style);

        assert!(
            matches!(blocks[0], Block::Heading { level: 1, .. }),
            "got {:?}",
            blocks[0]
        );
    }

    #[test]
    fn a_long_bold_line_is_a_sentence_not_a_heading() {
        let long = "This whole sentence happens to be set in bold for emphasis, \
                    which does not make it a heading at all";
        let glyphs = vec![bold(placed_line(long, 72.0, 700.0, 10.0))];
        let page = lines_of(&page(&glyphs));
        let style = style_of(std::slice::from_ref(&page));
        assert!(matches!(
            blocks_of(&page, &style)[0],
            Block::Paragraph { .. }
        ));
    }
}
