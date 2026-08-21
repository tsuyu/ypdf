//! The render thread's work queue.
//!
//! Three properties matter, and none of them come free from a plain channel:
//!
//! * **priority** — the page under the cursor must never wait behind a hundred
//!   thumbnails;
//! * **recency within a priority** — while scrolling, the newest request for a
//!   band of pages is the one worth doing, so each priority is LIFO;
//! * **de-duplication** — scrolling re-requests the same page many times per
//!   second, and rendering it twice is pure waste.
//!
//! Thumbnails are additionally capped: a 2000-page document must not be able to
//! queue 2000 rasters that will scroll out of view before they are drawn.

use std::collections::{HashMap, VecDeque};

use crate::types::{DocumentId, PageIndex, Priority, RenderRequest};

/// Maximum queued thumbnail requests. Beyond this the oldest are dropped; the
/// sidebar simply asks again when they scroll back into view.
const THUMBNAIL_CAPACITY: usize = 64;

/// Priority queue with de-duplication and per-document generation filtering.
#[derive(Debug, Default)]
pub struct RenderQueue {
    visible: VecDeque<RenderRequest>,
    nearby: VecDeque<RenderRequest>,
    thumbnail: VecDeque<RenderRequest>,
    /// Newest generation seen per document. Older requests are dead on arrival.
    generations: HashMap<DocumentId, u64>,
}

impl RenderQueue {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Total queued requests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.visible.len() + self.nearby.len() + self.thumbnail.len()
    }

    /// Is there nothing to do?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Queue a request, unless it is stale or already queued.
    ///
    /// A newer generation for a document silently discards everything older for
    /// that document: that is the cancellation path for a zoom or rotation.
    pub fn push(&mut self, request: RenderRequest) {
        let current = self
            .generations
            .entry(request.doc)
            .or_insert(request.generation);
        if request.generation < *current {
            return;
        }
        if request.generation > *current {
            *current = request.generation;
            self.retain_current_generations();
        }

        if self.contains(&request) {
            return;
        }

        let lane = self.lane_mut(request.priority);
        lane.push_front(request);
        if request.priority == Priority::Thumbnail {
            while lane.len() > THUMBNAIL_CAPACITY {
                lane.pop_back();
            }
        }
    }

    /// Take the most urgent, most recent request.
    pub fn pop(&mut self) -> Option<RenderRequest> {
        for priority in Priority::ORDER {
            if let Some(request) = self.lane_mut(priority).pop_front() {
                return Some(request);
            }
        }
        None
    }

    /// Drop everything belonging to a document, e.g. when its tab closes.
    pub fn drop_document(&mut self, doc: DocumentId) {
        self.generations.remove(&doc);
        for priority in Priority::ORDER {
            self.lane_mut(priority).retain(|r| r.doc != doc);
        }
    }

    /// Is this request still worth doing?
    ///
    /// Checked again immediately before rendering: a request can go stale while
    /// it sits in the queue.
    #[must_use]
    pub fn is_current(&self, request: &RenderRequest) -> bool {
        self.generations
            .get(&request.doc)
            .is_none_or(|g| request.generation >= *g)
    }

    /// Record a generation without queueing work, so later stale requests are
    /// dropped even if nothing has been queued yet.
    pub fn observe_generation(&mut self, doc: DocumentId, generation: u64) {
        let entry = self.generations.entry(doc).or_insert(generation);
        if generation > *entry {
            *entry = generation;
            self.retain_current_generations();
        }
    }

    /// Pages currently queued for a document, for diagnostics and tests.
    #[must_use]
    pub fn queued_pages(&self, doc: DocumentId) -> Vec<PageIndex> {
        Priority::ORDER
            .iter()
            .flat_map(|p| self.lane(*p).iter())
            .filter(|r| r.doc == doc)
            .map(|r| r.page)
            .collect()
    }

    fn contains(&self, request: &RenderRequest) -> bool {
        self.lane(request.priority)
            .iter()
            .any(|queued| queued.key() == request.key())
    }

    fn retain_current_generations(&mut self) {
        let generations = std::mem::take(&mut self.generations);
        for priority in Priority::ORDER {
            self.lane_mut(priority)
                .retain(|r| generations.get(&r.doc).is_none_or(|g| r.generation >= *g));
        }
        self.generations = generations;
    }

    fn lane(&self, priority: Priority) -> &VecDeque<RenderRequest> {
        match priority {
            Priority::Visible => &self.visible,
            Priority::Nearby => &self.nearby,
            Priority::Thumbnail => &self.thumbnail,
        }
    }

    fn lane_mut(&mut self, priority: Priority) -> &mut VecDeque<RenderRequest> {
        match priority {
            Priority::Visible => &mut self.visible,
            Priority::Nearby => &mut self.nearby,
            Priority::Thumbnail => &mut self.thumbnail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::QuarterTurns;

    fn request(doc: DocumentId, page: PageIndex, priority: Priority) -> RenderRequest {
        RenderRequest {
            doc,
            page,
            target_width: 800,
            rotation: QuarterTurns(0),
            priority,
            generation: 1,
        }
    }

    #[test]
    fn visible_pages_jump_the_queue() {
        let doc = DocumentId::new();
        let mut queue = RenderQueue::new();
        queue.push(request(doc, 10, Priority::Thumbnail));
        queue.push(request(doc, 11, Priority::Nearby));
        queue.push(request(doc, 12, Priority::Visible));

        assert_eq!(queue.pop().expect("visible").page, 12);
        assert_eq!(queue.pop().expect("nearby").page, 11);
        assert_eq!(queue.pop().expect("thumbnail").page, 10);
        assert!(queue.pop().is_none());
    }

    #[test]
    fn newest_request_in_a_lane_wins() {
        let doc = DocumentId::new();
        let mut queue = RenderQueue::new();
        for page in 0..5 {
            queue.push(request(doc, page, Priority::Visible));
        }
        // Scrolling asked for 0..5; page 4 is where the user ended up.
        assert_eq!(queue.pop().expect("newest").page, 4);
    }

    #[test]
    fn identical_requests_are_not_queued_twice() {
        let doc = DocumentId::new();
        let mut queue = RenderQueue::new();
        for _ in 0..50 {
            queue.push(request(doc, 3, Priority::Visible));
        }
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn a_new_generation_discards_the_old_work() {
        let doc = DocumentId::new();
        let mut queue = RenderQueue::new();
        queue.push(request(doc, 1, Priority::Visible));
        queue.push(request(doc, 2, Priority::Thumbnail));

        let mut zoomed = request(doc, 7, Priority::Visible);
        zoomed.generation = 2;
        queue.push(zoomed);

        assert_eq!(queue.queued_pages(doc), vec![7]);
        assert!(!queue.is_current(&request(doc, 1, Priority::Visible)));
    }

    #[test]
    fn thumbnails_are_capped() {
        let doc = DocumentId::new();
        let mut queue = RenderQueue::new();
        for page in 0..500 {
            queue.push(request(doc, page, Priority::Thumbnail));
        }
        assert_eq!(queue.len(), THUMBNAIL_CAPACITY);
        // The cap keeps the newest requests, which are the ones on screen.
        assert_eq!(queue.pop().expect("newest").page, 499);
    }

    #[test]
    fn closing_a_document_clears_its_work() {
        let a = DocumentId::new();
        let b = DocumentId::new();
        let mut queue = RenderQueue::new();
        queue.push(request(a, 1, Priority::Visible));
        queue.push(request(b, 1, Priority::Visible));

        queue.drop_document(a);
        assert_eq!(queue.queued_pages(a), Vec::<PageIndex>::new());
        assert_eq!(queue.queued_pages(b), vec![1]);
    }

    #[test]
    fn different_zoom_levels_are_different_work() {
        let doc = DocumentId::new();
        let mut queue = RenderQueue::new();
        queue.push(request(doc, 1, Priority::Visible));
        let mut wider = request(doc, 1, Priority::Visible);
        wider.target_width = 1600;
        queue.push(wider);
        assert_eq!(queue.len(), 2);
    }
}
