//! Page text and links.
//!
//! Text arrives as characters with boxes, not as a flat string with offsets
//! bolted on: search highlighting, selection, and copy all need to go from a
//! position in the text back to a rectangle on the page, and the only reliable
//! way to do that is to keep the two aligned from the start.
//!
//! Coordinates are PDF points with the origin at the bottom-left, exactly as
//! PDFium reports them. Converting to screen space is the viewer's job — it is
//! the only part that knows about zoom, rotation, and scroll.

use crate::types::PageIndex;

/// A rectangle in PDF points, origin bottom-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectPt {
    /// Left edge.
    pub left: f32,
    /// Bottom edge.
    pub bottom: f32,
    /// Right edge.
    pub right: f32,
    /// Top edge.
    pub top: f32,
}

impl RectPt {
    /// Smallest rectangle containing both.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            bottom: self.bottom.min(other.bottom),
            right: self.right.max(other.right),
            top: self.top.max(other.top),
        }
    }

    /// Does this rectangle contain the point?
    #[must_use]
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.left && x <= self.right && y >= self.bottom && y <= self.top
    }

    /// Vertical midpoint, used to decide whether two characters share a line.
    #[must_use]
    pub fn mid_y(self) -> f32 {
        (self.top + self.bottom) / 2.0
    }

    /// Height in points.
    #[must_use]
    pub fn height(self) -> f32 {
        (self.top - self.bottom).abs()
    }
}

/// One typeface as a page uses it.
///
/// Held once per page and referred to by index. A page of three thousand
/// glyphs draws them in a handful of faces, and a `String` per character would
/// cost more than the text itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontFace {
    /// Name as the file gives it, e.g. `"Helvetica-Bold"`. Embedded subsets
    /// usually carry a six-letter tag, as in `"ABCDEF+Minion-Regular"`.
    pub name: String,
    /// Weight from 100 to 900, when the file says. Absent is common.
    pub weight: Option<u32>,
    /// The face is italic or oblique.
    pub italic: bool,
    /// The face has serifs.
    pub serif: bool,
    /// Every glyph has the same width, as in a monospaced face.
    pub fixed_pitch: bool,
}

impl FontFace {
    /// Whether this face reads as bold.
    ///
    /// By declared weight when there is one — 600 rather than 700, so that
    /// semibold faces, which are used for headings as often as bold ones, are
    /// not missed. Otherwise by name, which is all a file without a weight
    /// gives us to go on.
    #[must_use]
    pub fn is_bold(&self) -> bool {
        match self.weight {
            Some(weight) => weight >= 600,
            None => {
                let name = self.name.to_ascii_lowercase();
                name.contains("bold") || name.contains("black") || name.contains("heavy")
            }
        }
    }
}

/// One character, where it sits, and how it is drawn.
#[derive(Clone, Copy, Debug)]
pub struct CharBox {
    /// The character itself.
    pub ch: char,
    /// Its box on the page.
    pub rect: RectPt,
    /// Font size in points as drawn, with the text matrix applied.
    ///
    /// This is the number that distinguishes a heading from body text, so it
    /// is the drawn size rather than the size named in the font dictionary.
    pub size: f32,
    /// Which of [`PageText::fonts`] drew it.
    pub font: u16,
}

/// Everything extracted from one page's text layer.
#[derive(Clone, Debug, Default)]
pub struct PageText {
    /// Characters in reading order, as PDFium reports them.
    pub chars: Vec<CharBox>,
    /// The faces those characters are drawn in, in first-seen order.
    pub fonts: Vec<FontFace>,
}

impl PageText {
    /// The page's text as a string.
    ///
    /// Character `i` of this string corresponds to `chars[i]` only for text
    /// that is entirely BMP-and-single-`char`; use [`Self::text_range`] rather
    /// than slicing by byte.
    #[must_use]
    pub fn text(&self) -> String {
        self.chars.iter().map(|c| c.ch).collect()
    }

    /// The face character `index` is drawn in.
    ///
    /// `None` when the text came from somewhere that carries no font table,
    /// such as a test fixture.
    #[must_use]
    pub fn face_of(&self, index: usize) -> Option<&FontFace> {
        let ch = self.chars.get(index)?;
        self.fonts.get(usize::from(ch.font))
    }

    /// The text of a character range.
    #[must_use]
    pub fn text_range(&self, start: usize, len: usize) -> String {
        self.chars
            .iter()
            .skip(start)
            .take(len)
            .map(|c| c.ch)
            .collect()
    }

    /// Is there any text at all? False for a scanned page — which is exactly
    /// the signal that it needs OCR (spec §7).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// Find every occurrence of `query`.
    #[must_use]
    pub fn search(&self, query: &str, options: SearchOptions) -> Vec<TextMatch> {
        let haystack: Vec<char> = self.chars.iter().map(|c| c.ch).collect();
        find_matches(&haystack, query, options)
            .into_iter()
            .map(|(start, len)| TextMatch {
                start,
                len,
                rects: self.rects_for(start, len),
            })
            .collect()
    }

    /// Merge a character range into one rectangle per line.
    ///
    /// A match that wraps across a line break must not be highlighted as one
    /// enormous box spanning both lines and everything between them.
    #[must_use]
    pub fn rects_for(&self, start: usize, len: usize) -> Vec<RectPt> {
        let mut rects: Vec<RectPt> = Vec::new();
        for c in self.chars.iter().skip(start).take(len) {
            match rects.last_mut() {
                // Same line if the vertical centres are within half a line
                // height of each other.
                Some(last) if same_line(*last, c.rect) => *last = last.union(c.rect),
                _ => rects.push(c.rect),
            }
        }
        rects
    }

    /// The character index nearest a point, for click-to-place-cursor.
    ///
    /// Prefers a character actually under the point; otherwise takes the
    /// closest one, so a drag through the margin still selects.
    #[must_use]
    pub fn char_at(&self, x: f32, y: f32) -> Option<usize> {
        if let Some(index) = self.chars.iter().position(|c| c.rect.contains(x, y)) {
            return Some(index);
        }
        self.chars
            .iter()
            .enumerate()
            .map(|(index, c)| {
                let dx = (x - c.rect.left.max(c.rect.right.min(x))).abs();
                let dy = (y - c.rect.bottom.max(c.rect.top.min(y))).abs();
                (index, dx.hypot(dy))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    }
}

fn same_line(a: RectPt, b: RectPt) -> bool {
    let tolerance = a.height().max(b.height()) * 0.5;
    (a.mid_y() - b.mid_y()).abs() <= tolerance
}

/// How to match a search query (spec §2.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    /// Distinguish upper from lower case.
    pub case_sensitive: bool,
    /// Only match whole words.
    pub whole_word: bool,
}

/// One search hit.
#[derive(Clone, Debug)]
pub struct TextMatch {
    /// Index of the first character.
    pub start: usize,
    /// Number of characters.
    pub len: usize,
    /// One rectangle per line the match covers.
    pub rects: Vec<RectPt>,
}

/// Character-index search over a decoded page.
///
/// Works on `char`s rather than bytes so a match index is directly a character
/// box index, and so case folding cannot desynchronize the two.
#[must_use]
pub fn find_matches(haystack: &[char], query: &str, options: SearchOptions) -> Vec<(usize, usize)> {
    let needle: Vec<char> = if options.case_sensitive {
        query.chars().collect()
    } else {
        query.chars().flat_map(char::to_lowercase).collect()
    };
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }

    let folded: Vec<char> = if options.case_sensitive {
        haystack.to_vec()
    } else {
        // Case folding can change the character count (ß -> ss), which would
        // break index alignment; fold per character and keep the first.
        haystack
            .iter()
            .map(|c| c.to_lowercase().next().unwrap_or(*c))
            .collect()
    };

    let mut matches = Vec::new();
    let mut start = 0;
    while start + needle.len() <= folded.len() {
        if folded[start..start + needle.len()] == needle[..]
            && (!options.whole_word || is_whole_word(&folded, start, needle.len()))
        {
            matches.push((start, needle.len()));
            start += needle.len();
            continue;
        }
        start += 1;
    }
    matches
}

fn is_whole_word(haystack: &[char], start: usize, len: usize) -> bool {
    let before = start.checked_sub(1).and_then(|i| haystack.get(i));
    let after = haystack.get(start + len);
    let boundary = |c: Option<&char>| c.is_none_or(|c| !c.is_alphanumeric() && *c != '_');
    boundary(before) && boundary(after)
}

/// Where a link goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    /// An external URL.
    Url(String),
    /// Another page in this document.
    Page(PageIndex),
    /// A destination yPDF does not follow — a remote document, an embedded
    /// file, or a launch action. Kept, not followed: launch actions are a
    /// security concern the scanner reports (spec §20, §26).
    Unsupported(&'static str),
}

/// A clickable region on a page.
#[derive(Clone, Debug)]
pub struct PageLink {
    /// Where it is.
    pub rect: RectPt,
    /// Where it goes.
    pub target: LinkTarget,
}

/// Text and links for one page, as one unit — the viewer needs both to draw a
/// page's interactive layer, and both come from the same PDFium page load.
#[derive(Clone, Debug)]
pub struct PageAnalysis {
    /// Which page.
    pub page: PageIndex,
    /// Its text layer.
    pub text: PageText,
    /// Its links.
    pub links: Vec<PageLink>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(text: &str) -> PageText {
        // One 10x12pt box per character, laid out left to right on one line.
        let chars = text
            .chars()
            .enumerate()
            .map(|(i, ch)| CharBox {
                ch,
                #[expect(clippy::cast_precision_loss, reason = "test fixture")]
                rect: RectPt {
                    left: i as f32 * 10.0,
                    bottom: 100.0,
                    right: i as f32 * 10.0 + 10.0,
                    top: 112.0,
                },
                size: 12.0,
                font: 0,
            })
            .collect();
        PageText {
            chars,
            fonts: Vec::new(),
        }
    }

    #[test]
    fn a_face_is_bold_by_weight_when_the_file_declares_one() {
        let semibold = FontFace {
            weight: Some(600),
            ..FontFace::default()
        };
        assert!(
            semibold.is_bold(),
            "semibold sets headings as often as bold"
        );

        let book = FontFace {
            weight: Some(400),
            name: "Something-Bold".to_string(),
            ..FontFace::default()
        };
        assert!(
            !book.is_bold(),
            "a declared weight outranks a name that disagrees with it"
        );
    }

    #[test]
    fn a_face_without_a_weight_falls_back_to_its_name() {
        let by_name = FontFace {
            name: "ABCDEF+Minion-Bold".to_string(),
            ..FontFace::default()
        };
        assert!(by_name.is_bold(), "a subset tag must not hide the name");

        let plain = FontFace {
            name: "Minion-Regular".to_string(),
            ..FontFace::default()
        };
        assert!(!plain.is_bold());
    }

    #[test]
    fn a_page_without_a_font_table_reports_no_face() {
        let text = page("hi");
        assert!(
            text.face_of(0).is_none(),
            "an absent table is not a face at index zero"
        );
        assert!(text.face_of(99).is_none(), "and neither is a missing char");
    }

    #[test]
    fn faces_are_looked_up_per_character() {
        let mut text = page("hi");
        text.fonts = vec![
            FontFace {
                name: "Body".to_string(),
                ..FontFace::default()
            },
            FontFace {
                name: "Head".to_string(),
                weight: Some(700),
                ..FontFace::default()
            },
        ];
        text.chars[1].font = 1;

        assert_eq!(text.face_of(0).expect("face").name, "Body");
        assert!(text.face_of(1).expect("face").is_bold());
    }

    #[test]
    fn search_is_case_insensitive_by_default() {
        let p = page("The Quick Brown Fox");
        assert_eq!(p.search("quick", SearchOptions::default()).len(), 1);
        let sensitive = SearchOptions {
            case_sensitive: true,
            whole_word: false,
        };
        assert!(p.search("quick", sensitive).is_empty());
        assert_eq!(p.search("Quick", sensitive).len(), 1);
    }

    #[test]
    fn whole_word_rejects_substrings() {
        let p = page("fox foxes unfox fox.");
        let loose = SearchOptions::default();
        let strict = SearchOptions {
            case_sensitive: false,
            whole_word: true,
        };
        assert_eq!(p.search("fox", loose).len(), 4);
        // Only the standalone "fox" and the one before the full stop.
        assert_eq!(p.search("fox", strict).len(), 2);
    }

    #[test]
    fn matches_do_not_overlap() {
        let p = page("aaaa");
        assert_eq!(p.search("aa", SearchOptions::default()).len(), 2);
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        let p = page("anything");
        assert!(p.search("", SearchOptions::default()).is_empty());
        assert!(
            p.search("longer than the page", SearchOptions::default())
                .is_empty()
        );
    }

    #[test]
    fn a_match_yields_one_rect_per_line() {
        let p = page("hello world");
        let hits = p.search("world", SearchOptions::default());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rects.len(), 1);
        let rect = hits[0].rects[0];
        assert_eq!(rect.left, 60.0);
        assert_eq!(rect.right, 110.0);
    }

    #[test]
    fn a_match_spanning_two_lines_yields_two_rects() {
        let mut p = page("abcdef");
        // Push the second half onto a lower line.
        for c in p.chars.iter_mut().skip(3) {
            c.rect.bottom -= 20.0;
            c.rect.top -= 20.0;
        }
        assert_eq!(p.rects_for(0, 6).len(), 2);
    }

    #[test]
    fn text_range_extracts_what_was_matched() {
        let p = page("the quick brown fox");
        let hit = &p.search("brown", SearchOptions::default())[0];
        assert_eq!(p.text_range(hit.start, hit.len), "brown");
    }

    #[test]
    fn char_at_finds_the_character_under_a_point() {
        let p = page("abcdef");
        assert_eq!(p.char_at(25.0, 105.0), Some(2));
        // Outside the text, the nearest character wins.
        assert_eq!(p.char_at(-50.0, 105.0), Some(0));
        assert_eq!(p.char_at(500.0, 105.0), Some(5));
    }

    #[test]
    fn a_page_with_no_text_layer_is_empty() {
        // The signal that a page is a scan and needs OCR.
        assert!(PageText::default().is_empty());
        assert!(
            PageText::default()
                .search("x", SearchOptions::default())
                .is_empty()
        );
    }
}
