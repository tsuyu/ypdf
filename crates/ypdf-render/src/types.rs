//! The vocabulary the GUI and the render thread exchange.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use ypdf_core::Error;

/// Identifies one open document. Unique for the life of the process, so a
/// stale event for a closed document is always recognizable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocumentId(u64);

impl DocumentId {
    /// Allocate the next identifier.
    #[must_use]
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// The raw value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl Default for DocumentId {
    fn default() -> Self {
        Self::new()
    }
}

/// Zero-based page index. PDFium counts pages with a C `int`.
pub type PageIndex = i32;

/// How urgently a render is wanted.
///
/// The queue is drained in this order, so a page the user is looking at never
/// waits behind a hundred thumbnails.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// On screen right now.
    Visible,
    /// Just off screen; likely next.
    Nearby,
    /// Sidebar thumbnail.
    Thumbnail,
}

impl Priority {
    /// All priorities, most urgent first.
    pub(crate) const ORDER: [Self; 3] = [Self::Visible, Self::Nearby, Self::Thumbnail];
}

/// Quarter-turn rotation applied on top of the page's own rotation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuarterTurns(pub u8);

impl QuarterTurns {
    /// Normalize to `0..4`.
    #[must_use]
    pub const fn normalized(self) -> u8 {
        self.0 % 4
    }

    /// Rotate a further quarter turn clockwise.
    #[must_use]
    pub const fn turned(self) -> Self {
        Self((self.0 + 1) % 4)
    }

    /// True when the rotation swaps width and height.
    #[must_use]
    pub const fn is_quarter(self) -> bool {
        self.normalized() % 2 == 1
    }
}

/// One render request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderRequest {
    /// Which document.
    pub doc: DocumentId,
    /// Which page.
    pub page: PageIndex,
    /// Target width in device pixels. Height follows the page aspect ratio.
    pub target_width: u32,
    /// Extra rotation to apply.
    pub rotation: QuarterTurns,
    /// How urgently it is wanted.
    pub priority: Priority,
    /// Bumped by the GUI whenever previously requested output becomes
    /// worthless — a zoom change, a rotation, a closed tab. The thread drops
    /// anything older, which is how cancellation works without a token per
    /// request.
    pub generation: u64,
}

impl RenderRequest {
    /// The identity used for de-duplication: same page, same size, same
    /// rotation means the same picture.
    pub(crate) fn key(&self) -> (DocumentId, PageIndex, u32, u8) {
        (
            self.doc,
            self.page,
            self.target_width,
            self.rotation.normalized(),
        )
    }
}

/// What the render thread knows about a document once it is open.
#[derive(Clone, Debug)]
pub struct DocumentInfo {
    /// Where it came from.
    pub path: PathBuf,
    /// Number of pages.
    pub page_count: PageIndex,
    /// Page sizes in PDF points, in page order. Lets the GUI lay out and size
    /// placeholders before a single page has been rendered.
    pub page_sizes_pt: Vec<(f32, f32)>,
}

impl DocumentInfo {
    /// Aspect ratio (width / height) of one page, if it exists.
    #[must_use]
    pub fn aspect(&self, page: PageIndex) -> Option<f32> {
        let (w, h) = *self.page_sizes_pt.get(usize::try_from(page).ok()?)?;
        (h > 0.0).then_some(w / h)
    }
}

/// A finished page raster, ready to become a texture.
#[derive(Clone)]
pub struct RenderedPage {
    /// Which document.
    pub doc: DocumentId,
    /// Which page.
    pub page: PageIndex,
    /// The generation this was rendered for.
    pub generation: u64,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Rotation that was applied.
    pub rotation: QuarterTurns,
    /// Tightly packed RGBA8, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for RenderedPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderedPage")
            .field("doc", &self.doc)
            .field("page", &self.page)
            .field("generation", &self.generation)
            .field("size", &(self.width, self.height))
            .field("bytes", &self.rgba.len())
            .finish()
    }
}

/// Everything the render thread sends back.
#[derive(Debug)]
pub enum RenderEvent {
    /// A document was opened.
    Opened {
        /// Which document.
        doc: DocumentId,
        /// Its structure.
        info: Box<DocumentInfo>,
    },
    /// A page finished rendering.
    Page(Box<RenderedPage>),
    /// A page's text layer and links were extracted.
    Analyzed {
        /// Which document.
        doc: DocumentId,
        /// Its text and links.
        analysis: Box<crate::text::PageAnalysis>,
    },
    /// Something failed. `page` is set when the failure was page-specific.
    Failed {
        /// Which document.
        doc: DocumentId,
        /// Which page, when the failure was page-specific.
        page: Option<PageIndex>,
        /// What went wrong.
        error: Error,
    },
    /// A document was closed and its resources released.
    Closed {
        /// Which document.
        doc: DocumentId,
    },
}
