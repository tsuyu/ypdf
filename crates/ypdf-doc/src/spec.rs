//! Page range expressions.
//!
//! The same syntax serves the GUI, the CLI (`--pages 1-20,30,45-`), and project
//! files, so it lives in the engine rather than in any one front end.
//!
//! Pages are 1-based here, because that is what a user means by "page 3". The
//! conversion to 0-based indices happens once, at [`PageSpec::resolve`].

use std::fmt;
use std::str::FromStr;

use ypdf_core::{Error, Result};

/// One element of a page expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    /// A single page, 1-based.
    Single(u32),
    /// An inclusive range, 1-based.
    Range(u32, u32),
    /// From a page to the end of the document.
    From(u32),
    /// From the start to a page.
    To(u32),
    /// Every page.
    All,
}

/// A parsed page expression, not yet checked against a document.
///
/// Validation needs the page count, which the expression does not carry —
/// `5-` is meaningful only once the document is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageSpec {
    parts: Vec<Part>,
    source: String,
}

impl PageSpec {
    /// Parse an expression such as `1-20,30,45-`.
    ///
    /// Accepted forms, comma separated: `N`, `A-B`, `A-`, `-B`, and `all`.
    pub fn parse(spec: &str) -> Result<Self> {
        let bad = || Error::InvalidPageRange {
            spec: spec.to_string(),
        };
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            return Err(bad());
        }

        let mut parts = Vec::new();
        for piece in trimmed.split(',') {
            let piece = piece.trim();
            if piece.is_empty() {
                return Err(bad());
            }
            if piece.eq_ignore_ascii_case("all") {
                parts.push(Part::All);
                continue;
            }

            let part = match piece.split_once('-') {
                None => Part::Single(number(piece).ok_or_else(bad)?),
                Some((start, "")) => Part::From(number(start).ok_or_else(bad)?),
                Some(("", end)) => Part::To(number(end).ok_or_else(bad)?),
                Some((start, end)) => {
                    let (start, end) =
                        (number(start).ok_or_else(bad)?, number(end).ok_or_else(bad)?);
                    if start > end {
                        return Err(bad());
                    }
                    Part::Range(start, end)
                }
            };
            parts.push(part);
        }

        Ok(Self {
            parts,
            source: trimmed.to_string(),
        })
    }

    /// Every page, in order.
    #[must_use]
    pub fn all() -> Self {
        Self {
            parts: vec![Part::All],
            source: "all".to_string(),
        }
    }

    /// A single page.
    #[must_use]
    pub fn single(page: u32) -> Self {
        Self {
            parts: vec![Part::Single(page)],
            source: page.to_string(),
        }
    }

    /// Expand to 1-based page numbers against a document of `page_count` pages.
    ///
    /// Order follows the expression — `9,1` means page 9 then page 1, which is
    /// what makes this usable for reordering as well as selection. Repeats are
    /// preserved for the same reason; use [`Self::resolve_unique`] when a page
    /// must appear at most once.
    pub fn resolve(&self, page_count: u32) -> Result<Vec<u32>> {
        if page_count == 0 {
            return Ok(Vec::new());
        }

        let mut pages = Vec::new();
        for part in &self.parts {
            let (start, end) = match *part {
                Part::Single(n) => (n, n),
                Part::Range(a, b) => (a, b),
                Part::From(a) => (a, page_count),
                Part::To(b) => (1, b),
                Part::All => (1, page_count),
            };
            if start == 0 {
                return Err(Error::InvalidPageRange {
                    spec: self.source.clone(),
                });
            }
            if end > page_count {
                return Err(Error::PageOutOfRange {
                    requested: end,
                    pages: page_count,
                });
            }
            pages.extend(start..=end);
        }
        Ok(pages)
    }

    /// Like [`Self::resolve`], but each page appears once, in ascending order.
    pub fn resolve_unique(&self, page_count: u32) -> Result<Vec<u32>> {
        let mut pages = self.resolve(page_count)?;
        pages.sort_unstable();
        pages.dedup();
        Ok(pages)
    }
}

fn number(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

impl FromStr for PageSpec {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl fmt::Display for PageSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(spec: &str, count: u32) -> Result<Vec<u32>> {
        PageSpec::parse(spec)?.resolve(count)
    }

    #[test]
    fn parses_the_forms_the_cli_documents() {
        assert_eq!(resolve("3", 10).expect("single"), vec![3]);
        assert_eq!(resolve("1-4", 10).expect("range"), vec![1, 2, 3, 4]);
        assert_eq!(resolve("8-", 10).expect("open end"), vec![8, 9, 10]);
        assert_eq!(resolve("-3", 10).expect("open start"), vec![1, 2, 3]);
        assert_eq!(resolve("all", 3).expect("all"), vec![1, 2, 3]);
        assert_eq!(
            resolve("1-2,5,9-10", 10).expect("mixed"),
            vec![1, 2, 5, 9, 10]
        );
    }

    #[test]
    fn whitespace_is_tolerated() {
        assert_eq!(resolve(" 1 - 2 , 5 ", 10).expect("spaced"), vec![1, 2, 5]);
    }

    #[test]
    fn order_is_preserved_so_the_spec_can_reorder() {
        assert_eq!(resolve("9,1,5", 10).expect("out of order"), vec![9, 1, 5]);
        assert_eq!(
            resolve("3,3", 10).expect("repeat"),
            vec![3, 3],
            "repeats duplicate a page"
        );
    }

    #[test]
    fn resolve_unique_sorts_and_deduplicates() {
        let spec = PageSpec::parse("9,1,5,1").expect("parses");
        assert_eq!(spec.resolve_unique(10).expect("unique"), vec![1, 5, 9]);
    }

    #[test]
    fn a_page_past_the_end_is_a_range_error() {
        let err = resolve("9-20", 10).expect_err("out of range");
        assert_eq!(err.code(), "E_PAGE_RANGE");
    }

    #[test]
    fn nonsense_is_a_spec_error_with_the_text_the_user_typed() {
        for spec in [
            "", "  ", "abc", "-", "1,,2", "4-2", "0", "0-3", "1.5", "-1-2", "1-2-3",
        ] {
            let err = PageSpec::parse(spec)
                .and_then(|s| s.resolve(10))
                .expect_err(&format!("{spec:?} must be rejected"));
            assert!(
                matches!(err.code(), "E_PAGE_SPEC" | "E_PAGE_RANGE"),
                "{spec:?} produced {}",
                err.code()
            );
        }
    }

    #[test]
    fn an_empty_document_resolves_to_nothing() {
        // Not an error: an operation over no pages is a no-op, not a failure.
        assert!(resolve("1-5", 0).expect("empty document").is_empty());
    }

    #[test]
    fn display_round_trips_the_source() {
        let spec = PageSpec::parse("1-2,5").expect("parses");
        assert_eq!(spec.to_string(), "1-2,5");
        assert_eq!(PageSpec::parse(&spec.to_string()).expect("reparses"), spec);
    }
}
