//! Document-wide search state (spec §2.1).
//!
//! Searching needs the text of every page, which arrives asynchronously from
//! the render thread. Rather than blocking until the whole document is
//! extracted, the query is kept and re-run against each page as its text
//! shows up, so results accumulate while the user is already reading them.

use std::collections::BTreeMap;

use ypdf_render::{PageIndex, PageText, RectPt, SearchOptions};

/// One hit, with the page it is on.
#[derive(Clone, Debug)]
pub struct Hit {
    /// Page the hit is on.
    pub page: PageIndex,
    /// First character index within that page's text.
    pub start: usize,
    /// Length in characters.
    pub len: usize,
    /// Where to draw the highlight, one rectangle per line.
    pub rects: Vec<RectPt>,
}

/// Search query, options, and results so far.
#[derive(Debug, Default)]
pub struct Search {
    /// What the user typed.
    pub query: String,
    /// Matching options.
    pub options: SearchOptions,
    /// Is the search bar open?
    pub open: bool,
    /// Hits per page, so a re-extracted page replaces its own results rather
    /// than duplicating them. `BTreeMap` keeps pages in reading order.
    hits: BTreeMap<PageIndex, Vec<Hit>>,
    /// Index into the flattened hit list.
    current: usize,
}

impl Search {
    /// Drop every result. Called when the query or the options change.
    pub fn clear_results(&mut self) {
        self.hits.clear();
        self.current = 0;
    }

    /// Run the current query against one page's text.
    ///
    /// Idempotent: running it twice for the same page replaces that page's
    /// hits instead of adding a second copy.
    pub fn index_page(&mut self, page: PageIndex, text: &PageText) {
        if self.query.is_empty() {
            return;
        }
        let hits: Vec<Hit> = text
            .search(&self.query, self.options)
            .into_iter()
            .map(|m| Hit {
                page,
                start: m.start,
                len: m.len,
                rects: m.rects,
            })
            .collect();

        if hits.is_empty() {
            self.hits.remove(&page);
        } else {
            self.hits.insert(page, hits);
        }
        self.current = self.current.min(self.total().saturating_sub(1));
    }

    /// Total hits found so far.
    #[must_use]
    pub fn total(&self) -> usize {
        self.hits.values().map(Vec::len).sum()
    }

    /// Hits on one page, for drawing highlights.
    #[must_use]
    pub fn hits_on(&self, page: PageIndex) -> &[Hit] {
        self.hits.get(&page).map_or(&[], Vec::as_slice)
    }

    /// The hit the viewer should be showing.
    #[must_use]
    pub fn current(&self) -> Option<&Hit> {
        self.flat().nth(self.current)
    }

    /// Position of the current hit in the whole result list, 1-based.
    #[must_use]
    pub fn current_number(&self) -> usize {
        if self.total() == 0 {
            0
        } else {
            self.current + 1
        }
    }

    /// Is this hit the one currently selected? Used to paint it differently.
    #[must_use]
    pub fn is_current(&self, page: PageIndex, start: usize) -> bool {
        self.current()
            .is_some_and(|h| h.page == page && h.start == start)
    }

    /// Move to the next hit, wrapping at the end.
    pub fn next(&mut self) -> Option<&Hit> {
        let total = self.total();
        if total == 0 {
            return None;
        }
        self.current = (self.current + 1) % total;
        self.current()
    }

    /// Move to the previous hit, wrapping at the start.
    pub fn previous(&mut self) -> Option<&Hit> {
        let total = self.total();
        if total == 0 {
            return None;
        }
        self.current = (self.current + total - 1) % total;
        self.current()
    }

    fn flat(&self) -> impl Iterator<Item = &Hit> {
        self.hits.values().flat_map(|hits| hits.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ypdf_render::CharBox;

    fn page_text(text: &str) -> PageText {
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
            })
            .collect();
        PageText { chars }
    }

    fn search_for(query: &str) -> Search {
        Search {
            query: query.to_string(),
            ..Search::default()
        }
    }

    #[test]
    fn results_accumulate_as_pages_arrive() {
        let mut search = search_for("fox");
        assert_eq!(search.total(), 0);

        search.index_page(2, &page_text("a fox here"));
        assert_eq!(search.total(), 1);

        search.index_page(0, &page_text("fox and fox"));
        assert_eq!(search.total(), 3);
    }

    #[test]
    fn re_indexing_a_page_replaces_its_hits() {
        let mut search = search_for("fox");
        search.index_page(1, &page_text("fox fox"));
        search.index_page(1, &page_text("fox fox"));
        assert_eq!(search.total(), 2, "a page must not double-count");
    }

    #[test]
    fn navigation_follows_reading_order_and_wraps() {
        let mut search = search_for("x");
        search.index_page(5, &page_text("x"));
        search.index_page(1, &page_text("x x"));

        // Page 1 comes first even though page 5 was indexed first.
        assert_eq!(search.current().expect("a hit").page, 1);
        assert_eq!(search.current_number(), 1);

        assert_eq!(search.next().expect("second").page, 1);
        assert_eq!(search.next().expect("third").page, 5);
        // Wraps back to the beginning.
        assert_eq!(search.next().expect("wrapped").page, 1);
        assert_eq!(search.previous().expect("back").page, 5);
    }

    #[test]
    fn options_narrow_the_result() {
        let mut search = search_for("fox");
        search.index_page(0, &page_text("fox foxes Fox"));
        assert_eq!(search.total(), 3);

        search.options = SearchOptions {
            case_sensitive: true,
            whole_word: true,
        };
        search.clear_results();
        search.index_page(0, &page_text("fox foxes Fox"));
        assert_eq!(search.total(), 1);
    }

    #[test]
    fn an_empty_query_finds_nothing_and_navigating_is_harmless() {
        let mut search = Search::default();
        search.index_page(0, &page_text("anything"));
        assert_eq!(search.total(), 0);
        assert!(search.next().is_none());
        assert!(search.previous().is_none());
        assert_eq!(search.current_number(), 0);
    }
}
