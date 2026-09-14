//! Characters into lines.
//!
//! PDFium hands back characters in reading order with a box each. A line is
//! the first thing that has to be rebuilt from that, because everything above
//! it — paragraphs, headings, lists — is stated in terms of lines.
//!
//! Grouping is by vertical band rather than by an exact baseline: a line that
//! mixes sizes, or sets a word in a larger face, still reads as one line, and
//! its characters do not share a baseline value.

use ypdf_render::{CharBox, PageText};

/// A stretch of characters drawn in one face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// The text itself.
    pub text: String,
    /// Drawn in a bold face.
    pub bold: bool,
    /// Drawn in an italic face.
    pub italic: bool,
}

/// A horizontal hole inside a line.
///
/// Worth recording because a page set in columns has no gap *between* its
/// lines — the two columns sit at the same heights, so they are gathered into
/// one line with the gutter sitting inside it. That hole is the only trace the
/// column layout leaves.
#[derive(Clone, Copy, Debug)]
pub struct Gap {
    /// How wide, in points.
    pub width: f32,
    /// Where its middle is.
    pub centre: f32,
}

/// One line of text, rebuilt from the characters that sit on it.
#[derive(Clone, Debug)]
pub struct Line {
    /// Its text, split into runs wherever the face changes.
    pub runs: Vec<Run>,
    /// Leftmost edge, in points.
    pub left: f32,
    /// Rightmost edge, in points.
    pub right: f32,
    /// Top of the band it occupies.
    pub top: f32,
    /// Bottom of the band it occupies.
    pub bottom: f32,
    /// The size most of its characters are drawn at.
    pub size: f32,
    /// The widest hole between two of its characters, if it has one.
    pub widest_gap: Option<Gap>,
}

impl Line {
    /// The line's text with the runs joined back together.
    #[must_use]
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    /// Whether most of the line is bold, which is what makes a heading look
    /// like one even when it is set at body size.
    #[must_use]
    pub fn is_mostly_bold(&self) -> bool {
        let (bold, total) = self.runs.iter().fold((0, 0), |(bold, total), run| {
            let n = run.text.trim().chars().count();
            (bold + if run.bold { n } else { 0 }, total + n)
        });
        total > 0 && bold * 2 > total
    }
}

/// One character with everything the grouping needs already resolved.
struct Placed {
    ch: char,
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    size: f32,
    bold: bool,
    italic: bool,
}

impl Placed {
    fn centre(&self) -> f32 {
        f32::midpoint(self.top, self.bottom)
    }
}

/// Rebuild the lines of one page.
///
/// Returns them top to bottom, each with its characters left to right.
#[must_use]
pub fn lines_of(text: &PageText) -> Vec<Line> {
    let mut placed = placed_chars(text);
    if placed.is_empty() {
        return Vec::new();
    }

    // Top down, then left to right. PDFium's own order is usually already
    // this, but a file is free to draw its glyphs in any order it likes and
    // some generators do exactly that.
    placed.sort_by(|a, b| {
        b.centre()
            .partial_cmp(&a.centre())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.left
                    .partial_cmp(&b.left)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });

    let mut lines = Vec::new();
    let mut current: Vec<Placed> = Vec::new();
    let (mut top, mut bottom) = (f32::MIN, f32::MAX);

    for ch in placed {
        // A quarter of the character's own height of slack: enough for
        // subscripts and mixed sizes, not enough to swallow the next line.
        let slack = (ch.top - ch.bottom).abs() * 0.25;
        let joins =
            !current.is_empty() && ch.centre() <= top + slack && ch.centre() >= bottom - slack;

        if joins {
            top = top.max(ch.top);
            bottom = bottom.min(ch.bottom);
            current.push(ch);
        } else {
            if let Some(line) = finish(std::mem::take(&mut current)) {
                lines.push(line);
            }
            top = ch.top;
            bottom = ch.bottom;
            current.push(ch);
        }
    }
    if let Some(line) = finish(current) {
        lines.push(line);
    }

    lines
}

/// Resolve every character's box and face once, dropping the ones that cannot
/// contribute geometry.
fn placed_chars(text: &PageText) -> Vec<Placed> {
    let mut out = Vec::with_capacity(text.chars.len());
    for (i, c) in text.chars.iter().enumerate() {
        // Newlines and other control characters carry degenerate boxes, and
        // the line structure is being rebuilt from geometry anyway.
        if c.ch.is_control() {
            continue;
        }
        let face = text.face_of(i);
        out.push(Placed {
            ch: c.ch,
            left: c.rect.left,
            right: c.rect.right,
            top: c.rect.top,
            bottom: c.rect.bottom,
            size: size_of(c),
            bold: face.is_some_and(ypdf_render::FontFace::is_bold),
            italic: face.is_some_and(|f| f.italic),
        });
    }
    out
}

/// The drawn size, falling back to the box when a file reports none.
fn size_of(c: &CharBox) -> f32 {
    if c.size > 0.0 {
        c.size
    } else {
        (c.rect.top - c.rect.bottom).abs()
    }
}

/// Turn a line's characters into runs, inserting the spaces the file left out.
fn finish(mut chars: Vec<Placed>) -> Option<Line> {
    if chars.is_empty() {
        return None;
    }
    chars.sort_by(|a, b| {
        a.left
            .partial_cmp(&b.left)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let left = chars.iter().fold(f32::MAX, |acc, c| acc.min(c.left));
    let right = chars.iter().fold(f32::MIN, |acc, c| acc.max(c.right));
    let top = chars.iter().fold(f32::MIN, |acc, c| acc.max(c.top));
    let bottom = chars.iter().fold(f32::MAX, |acc, c| acc.min(c.bottom));
    let size = modal_size(&chars);

    let mut runs: Vec<Run> = Vec::new();
    let mut previous_right: Option<f32> = None;
    let mut widest_gap: Option<Gap> = None;

    for ch in &chars {
        // Plenty of files place words by moving the pen rather than by drawing
        // a space. Without this, "the quick brown fox" arrives as one word.
        let gapped = previous_right.is_some_and(|prev| ch.left - prev > size * 0.25);
        let needs_space = gapped && !ch.ch.is_whitespace();

        if let Some(prev) = previous_right {
            let width = ch.left - prev;
            if width > widest_gap.map_or(0.0, |g: Gap| g.width) {
                widest_gap = Some(Gap {
                    width,
                    centre: f32::midpoint(prev, ch.left),
                });
            }
        }

        match runs.last_mut() {
            Some(run) if run.bold == ch.bold && run.italic == ch.italic => {
                if needs_space && !run.text.ends_with(' ') {
                    run.text.push(' ');
                }
                run.text.push(ch.ch);
            }
            _ => {
                let mut text = String::new();
                if needs_space && !runs.is_empty() {
                    text.push(' ');
                }
                text.push(ch.ch);
                runs.push(Run {
                    text,
                    bold: ch.bold,
                    italic: ch.italic,
                });
            }
        }
        previous_right = Some(ch.right);
    }

    trim_edges(&mut runs);
    if runs.iter().all(|r| r.text.trim().is_empty()) {
        return None;
    }

    Some(Line {
        runs,
        left,
        right,
        top,
        bottom,
        size,
        widest_gap,
    })
}

/// The size most of the line's characters are drawn at.
///
/// Modal rather than mean: one large capital at the start of a paragraph must
/// not drag the whole line into looking like a heading.
fn modal_size(chars: &[Placed]) -> f32 {
    let mut counts: Vec<(f32, usize)> = Vec::new();
    for ch in chars {
        if ch.ch.is_whitespace() {
            continue;
        }
        // Half-point buckets: the same nominal size arrives with tiny
        // differences once a text matrix has been applied.
        let size = (ch.size * 2.0).round() / 2.0;
        match counts
            .iter_mut()
            .find(|(s, _)| (*s - size).abs() < f32::EPSILON)
        {
            Some((_, n)) => *n += 1,
            None => counts.push((size, 1)),
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map_or(0.0, |(size, _)| size)
}

/// Drop leading and trailing whitespace without losing the runs it sat in.
fn trim_edges(runs: &mut Vec<Run>) {
    while let Some(first) = runs.first_mut() {
        let trimmed = first.text.trim_start().to_string();
        if trimmed.is_empty() {
            runs.remove(0);
        } else {
            first.text = trimmed;
            break;
        }
    }
    while let Some(last) = runs.last_mut() {
        let trimmed = last.text.trim_end().to_string();
        if trimmed.is_empty() {
            runs.pop();
        } else {
            last.text = trimmed;
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{page, placed_line};

    #[test]
    fn characters_on_one_band_become_one_line() {
        let text = page(&[placed_line("hello", 100.0, 700.0, 12.0)]);
        let lines = lines_of(&text);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text(), "hello");
        assert!((lines[0].size - 12.0).abs() < 0.01);
    }

    #[test]
    fn separate_bands_become_separate_lines_top_down() {
        let text = page(&[
            placed_line("second", 100.0, 680.0, 12.0),
            placed_line("first", 100.0, 700.0, 12.0),
        ]);
        let lines = lines_of(&text);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text(), "first", "the higher line comes first");
        assert_eq!(lines[1].text(), "second");
    }

    #[test]
    fn a_wide_gap_becomes_a_space_even_when_the_file_draws_none() {
        // Two words set 20pt apart with no space glyph between them.
        let mut chars = placed_line("one", 100.0, 700.0, 12.0);
        chars.extend(placed_line("two", 160.0, 700.0, 12.0));
        let lines = lines_of(&page(&[chars]));
        assert_eq!(lines[0].text(), "one two");
    }

    #[test]
    fn a_face_change_splits_the_line_into_runs() {
        let mut chars = placed_line("plain", 100.0, 700.0, 12.0);
        let mut bold = placed_line("loud", 140.0, 700.0, 12.0);
        for c in &mut bold {
            c.1 = true;
        }
        chars.extend(bold);

        let lines = lines_of(&page(&[chars]));
        assert_eq!(lines[0].runs.len(), 2);
        assert!(!lines[0].runs[0].bold);
        assert!(lines[0].runs[1].bold);
        assert_eq!(lines[0].text(), "plain loud");
    }

    #[test]
    fn a_blank_line_is_not_a_line() {
        let text = page(&[placed_line("   ", 100.0, 700.0, 12.0)]);
        assert!(lines_of(&text).is_empty());
    }

    #[test]
    fn one_large_capital_does_not_make_the_line_a_heading_size() {
        let mut chars = placed_line("T", 100.0, 700.0, 24.0);
        chars.extend(placed_line("he rest of it", 112.0, 700.0, 12.0));
        let lines = lines_of(&page(&[chars]));
        assert!(
            (lines[0].size - 12.0).abs() < 0.01,
            "modal size should follow the body text, got {}",
            lines[0].size
        );
    }

    #[test]
    fn mostly_bold_is_judged_on_the_text_not_the_run_count() {
        let mut chars = placed_line("A heading that is bold", 100.0, 700.0, 12.0);
        for c in &mut chars {
            c.1 = true;
        }
        let lines = lines_of(&page(&[chars]));
        assert!(lines[0].is_mostly_bold());
    }
}
