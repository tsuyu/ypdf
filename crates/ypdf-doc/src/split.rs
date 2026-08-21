//! Splitting a document into several (spec §3.2).

use ypdf_core::{Error, Result};

use crate::pdf::Pdf;
use crate::spec::PageSpec;

/// How to cut a document up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SplitMode {
    /// One output per page.
    EachPage,
    /// One output per fixed-size run of pages.
    EveryN(u32),
    /// One output per range in the expression, e.g. `1-10,11-20`.
    Ranges(Vec<PageSpec>),
    /// One output per top-level bookmark, cut where each one points.
    Bookmarks,
}

/// One piece of a split document, with the pages it came from.
#[derive(Debug)]
pub struct Piece {
    /// The document.
    pub pdf: Pdf,
    /// 1-based page numbers taken from the original, in order.
    pub pages: Vec<u32>,
    /// Suggested file stem, e.g. `"pages-1-10"`.
    pub name: String,
}

/// Split `pdf` according to `mode`.
///
/// Returns the pieces without writing anything: naming and placing files is a
/// front-end decision, and the CLI, the GUI, and a batch run each want it
/// differently.
pub fn split(pdf: &Pdf, mode: &SplitMode) -> Result<Vec<Piece>> {
    let count = pdf.page_count();
    if count == 0 {
        return Ok(Vec::new());
    }

    let groups: Vec<Vec<u32>> = match mode {
        SplitMode::EachPage => (1..=count).map(|page| vec![page]).collect(),
        SplitMode::EveryN(n) => {
            if *n == 0 {
                return Err(Error::InvalidPageRange {
                    spec: "split size must be at least 1 page".into(),
                });
            }
            (1..=count)
                .collect::<Vec<_>>()
                .chunks(*n as usize)
                .map(<[u32]>::to_vec)
                .collect()
        }
        SplitMode::Ranges(specs) => {
            let mut groups = Vec::with_capacity(specs.len());
            for spec in specs {
                let pages = spec.resolve(count)?;
                if pages.is_empty() {
                    return Err(Error::InvalidPageRange {
                        spec: spec.to_string(),
                    });
                }
                groups.push(pages);
            }
            groups
        }
        SplitMode::Bookmarks => bookmark_groups(pdf)?,
    };

    groups
        .into_iter()
        .map(|pages| {
            let name = name_for(&pages);
            Ok(Piece {
                pdf: pdf.extracted(&pages)?,
                pages,
                name,
            })
        })
        .collect()
}

/// Cut at every top-level bookmark destination.
///
/// A document with no outline is not an error; it simply has one piece, which
/// is the whole document. Refusing would make "split by bookmarks" fail on
/// exactly the files a user is most likely to try it on.
fn bookmark_groups(pdf: &Pdf) -> Result<Vec<Vec<u32>>> {
    let count = pdf.page_count();
    let mut starts = pdf.bookmark_pages()?;
    starts.sort_unstable();
    starts.dedup();
    starts.retain(|page| *page >= 1 && *page <= count);

    if starts.first() != Some(&1) {
        // Pages before the first bookmark still belong somewhere.
        starts.insert(0, 1);
    }

    let mut groups = Vec::with_capacity(starts.len());
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).map_or(count, |next| next - 1);
        if *start <= end {
            groups.push((*start..=end).collect());
        }
    }
    Ok(groups)
}

fn name_for(pages: &[u32]) -> String {
    match (pages.first(), pages.last()) {
        (Some(first), Some(last)) if first == last => format!("page-{first}"),
        (Some(first), Some(last)) => format!("pages-{first}-{last}"),
        _ => "pages".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_describe_the_pages_they_hold() {
        assert_eq!(name_for(&[3]), "page-3");
        assert_eq!(name_for(&[3, 4, 5]), "pages-3-5");
        assert_eq!(name_for(&[]), "pages");
    }
}
