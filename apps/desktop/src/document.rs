//! One open document: its structure, its view state, and its textures.

use std::collections::HashMap;
use std::path::PathBuf;

use ypdf_render::{DocumentId, DocumentInfo, PageAnalysis, PageIndex, QuarterTurns};

use crate::annotate::AnnotatePanel;
use crate::compress::CompressDialog;
use crate::edit::EditSession;
use crate::forms::FormsPanel;
use crate::inspect::Inspection;
use crate::ocr::OcrDialog;
use crate::outline::OutlinePanel;
use crate::protect::{PasswordPrompt, ProtectDialog};
use crate::redact::RedactState;
use crate::search::Search;
use crate::textures::PageCache;
use crate::watermark::WatermarkDialog;

/// A selected run of characters on one page.
///
/// Selection does not span pages yet: each page is its own text layer, and
/// stitching them into one stream is reading-order work that belongs with the
/// text extraction, not with the mouse handling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    /// Page the selection is on.
    pub page: PageIndex,
    /// Where the drag started.
    pub anchor: usize,
    /// Where the pointer is now.
    pub head: usize,
}

impl Selection {
    /// The selected range as `(start, len)`, whichever way the drag went.
    #[must_use]
    pub fn range(self) -> (usize, usize) {
        let (lo, hi) = if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        };
        (lo, hi - lo + 1)
    }
}

/// How the page is sized to the viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zoom {
    /// Page width fills the viewport.
    FitWidth,
    /// The whole page is visible.
    FitPage,
    /// An explicit scale, where 1.0 is 72 dpi — one PDF point per point.
    Scale(f32),
}

impl Zoom {
    /// Zoom steps for the in/out buttons, in scale units.
    const STEPS: [f32; 12] = [
        0.25, 0.33, 0.5, 0.67, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0, 8.0,
    ];

    /// The next step up from `current`.
    #[must_use]
    pub fn zoom_in(current: f32) -> Self {
        Self::Scale(
            Self::STEPS
                .iter()
                .copied()
                .find(|s| *s > current * 1.001)
                .unwrap_or_else(|| Self::STEPS[Self::STEPS.len() - 1]),
        )
    }

    /// The next step down from `current`.
    #[must_use]
    pub fn zoom_out(current: f32) -> Self {
        Self::Scale(
            Self::STEPS
                .iter()
                .rev()
                .copied()
                .find(|s| *s < current * 0.999)
                .unwrap_or(Self::STEPS[0]),
        )
    }
}

/// Everything the GUI knows about one open document.
pub struct OpenDocument {
    /// Engine-side identifier.
    pub id: DocumentId,
    /// Where it came from.
    pub path: PathBuf,
    /// Structure, once the render thread has reported it. `None` while opening.
    pub info: Option<DocumentInfo>,
    /// Failure that stopped it from opening or rendering.
    pub error: Option<String>,
    /// Current zoom mode.
    pub zoom: Zoom,
    /// Scale actually in use, resolved from [`Zoom`] each frame.
    pub scale: f32,
    /// Extra rotation applied to every page.
    pub rotation: QuarterTurns,
    /// Invert page colours for reading in the dark.
    pub invert: bool,
    /// Page the viewport is centred on, for the page indicator and scrolling.
    pub current_page: PageIndex,
    /// Bumped whenever previously requested rasters become worthless.
    pub generation: u64,
    /// Uploaded page textures.
    pub cache: PageCache,
    /// Set for one frame when the viewer should scroll to [`Self::current_page`].
    pub scroll_to_current: bool,
    /// Text and links per page, as the render thread extracts them.
    pub analyses: HashMap<PageIndex, PageAnalysis>,
    /// Pages already asked for, so scrolling does not re-ask every frame.
    pub analysis_requested: std::collections::HashSet<PageIndex>,
    /// Search state.
    pub search: Search,
    /// Current text selection, if any.
    pub selection: Option<Selection>,
    /// True while the pointer is down and dragging out a selection.
    pub selecting: bool,
    /// Page edits and their undo log.
    pub edits: EditSession,
    /// Pages selected in the sidebar, for the edit operations.
    pub selected_pages: std::collections::BTreeSet<PageIndex>,
    /// Where a `Shift` range selection extends from.
    pub selection_anchor: Option<PageIndex>,
    /// Page currently being dragged in the sidebar.
    pub dragging_page: Option<PageIndex>,
    /// Metadata, diagnostics, and the security scan.
    pub inspection: Inspection,
    /// The compression dialog and its last result.
    pub compress: CompressDialog,
    /// A compressed copy waiting to be saved or discarded.
    ///
    /// Held in memory rather than written anywhere: compression is destructive,
    /// so nothing touches the original until the user says so.
    pub compressed: Option<Vec<u8>>,
    /// Prompt shown when the file needs a password.
    pub password_prompt: PasswordPrompt,
    /// The Protect dialog (spec §8).
    pub protect: ProtectDialog,
    /// The OCR dialog and any run it is driving (spec §7).
    pub ocr: OcrDialog,
    /// The watermark dialog (spec §12).
    pub watermark: WatermarkDialog,
    /// Redaction marks and the panel that manages them (spec §11).
    pub redact: RedactState,
    /// The bookmarks panel (spec §17).
    pub outline: OutlinePanel,
    /// The form panel (spec §16).
    pub forms: FormsPanel,
    /// Annotation tools and the list of what is on the document (spec §10).
    pub annotate: AnnotatePanel,
    /// A copy carrying the annotations, waiting to be saved.
    pub annotation_bytes: Option<Vec<u8>>,
    /// A mark the viewer has just finished, waiting to be applied.
    ///
    /// The viewer draws without access to the render thread, so it leaves the
    /// annotation here and the application picks it up on the same frame.
    pub pending_annotation: Option<ypdf_annot::Annotation>,
    /// A filled or flattened copy waiting to be saved.
    pub form_result: Option<Vec<u8>>,
}

impl OpenDocument {
    /// A document that has been asked for but not yet opened.
    #[must_use]
    pub fn opening(id: DocumentId, path: PathBuf, texture_budget_mb: u64) -> Self {
        let path_for_edits = path.clone();
        Self {
            id,
            path,
            info: None,
            error: None,
            zoom: Zoom::FitWidth,
            scale: 1.0,
            rotation: QuarterTurns(0),
            invert: false,
            current_page: 0,
            generation: 1,
            cache: PageCache::new(texture_budget_mb),
            scroll_to_current: false,
            analyses: HashMap::new(),
            analysis_requested: std::collections::HashSet::new(),
            search: Search::default(),
            selection: None,
            selecting: false,
            edits: EditSession::new(path_for_edits),
            selected_pages: std::collections::BTreeSet::new(),
            selection_anchor: None,
            dragging_page: None,
            inspection: Inspection::default(),
            compress: CompressDialog::default(),
            compressed: None,
            password_prompt: PasswordPrompt::default(),
            protect: ProtectDialog::default(),
            ocr: OcrDialog::default(),
            watermark: WatermarkDialog::default(),
            redact: RedactState::new(),
            outline: OutlinePanel::default(),
            forms: FormsPanel::default(),
            annotate: AnnotatePanel::default(),
            annotation_bytes: None,
            pending_annotation: None,
            form_result: None,
        }
    }

    /// File name for the tab strip.
    #[must_use]
    pub fn title(&self) -> String {
        self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// Page count, or zero while opening.
    #[must_use]
    pub fn page_count(&self) -> PageIndex {
        self.info.as_ref().map_or(0, |i| i.page_count)
    }

    /// Size of a page in points, ignoring the view rotation.
    ///
    /// This is the coordinate space PDFium reports text and link boxes in, so
    /// it is what the point-to-screen mapping starts from.
    #[must_use]
    pub fn page_size_unrotated(&self, page: PageIndex) -> (f32, f32) {
        self.info
            .as_ref()
            .and_then(|i| {
                usize::try_from(page)
                    .ok()
                    .and_then(|p| i.page_sizes_pt.get(p))
                    .copied()
            })
            .unwrap_or((612.0, 792.0))
    }

    /// The selected text, if there is a selection.
    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        let analysis = self.analyses.get(&selection.page)?;
        let (start, len) = selection.range();
        let text = analysis.text.text_range(start, len);
        (!text.is_empty()).then_some(text)
    }

    /// Size of a page in points, with the view rotation applied.
    #[must_use]
    pub fn page_size_pt(&self, page: PageIndex) -> (f32, f32) {
        let (w, h) = self
            .info
            .as_ref()
            .and_then(|i| {
                usize::try_from(page)
                    .ok()
                    .and_then(|p| i.page_sizes_pt.get(p))
                    .copied()
            })
            // A page whose size could not be read still needs a box to draw in;
            // US Letter is the least surprising guess.
            .unwrap_or((612.0, 792.0));
        if self.rotation.is_quarter() {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// Invalidate every outstanding render for this document.
    ///
    /// Called on zoom and rotation: work already queued is for the old view and
    /// would arrive as a wrongly sized texture.
    pub fn invalidate(&mut self) {
        self.generation += 1;
    }

    /// Toggle inverted page colours.
    ///
    /// Only the textures change; nothing is re-rendered, because inversion is
    /// applied on upload.
    pub fn toggle_invert(&mut self) {
        self.invert = !self.invert;
    }

    /// Apply a quarter turn clockwise.
    pub fn rotate(&mut self) {
        self.rotation = self.rotation.turned();
        // Character boxes are still valid — they are in unrotated page space —
        // but a selection drawn at the old orientation is not worth keeping.
        self.selection = None;
        self.cache.clear();
        self.invalidate();
    }

    /// Switch zoom mode, keeping the current page in view.
    pub fn set_zoom(&mut self, zoom: Zoom) {
        if self.zoom != zoom {
            self.zoom = zoom;
            self.invalidate();
            self.scroll_to_current = true;
        }
    }

    /// Step to the next or previous search hit and select it.
    ///
    /// Selecting the match is what makes "find" and "copy what I found" one
    /// gesture instead of two.
    pub fn focus_hit(&mut self, forward: bool) -> Option<PageIndex> {
        let hit = if forward {
            self.search.next()
        } else {
            self.search.previous()
        }?;
        let (page, start, len) = (hit.page, hit.start, hit.len);
        self.selection = Some(Selection {
            page,
            anchor: start,
            head: start + len.saturating_sub(1),
        });
        self.go_to(page);
        Some(page)
    }

    /// The pages the edit operations act on: the sidebar selection, or the
    /// page being read when nothing is selected.
    ///
    /// Falling back to the current page is what makes "rotate" work without
    /// first making the user select anything.
    #[must_use]
    pub fn target_pages(&self) -> Vec<u32> {
        if self.selected_pages.is_empty() {
            return vec![u32::try_from(self.current_page + 1).unwrap_or(1)];
        }
        self.selected_pages
            .iter()
            .map(|p| u32::try_from(p + 1).unwrap_or(1))
            .collect()
    }

    /// Forget everything derived from the document's old contents.
    ///
    /// Called after an edit: rasters, text layers, search hits, and selections
    /// all describe pages that may no longer exist.
    pub fn invalidate_content(&mut self) {
        self.cache.clear();
        self.analyses.clear();
        self.analysis_requested.clear();
        self.search.clear_results();
        self.selection = None;
        self.selected_pages.clear();
        self.selection_anchor = None;
        self.dragging_page = None;
        // The reports describe the document as it was before the edit.
        self.inspection.invalidate();
        // So does the outline: deleting a page moves every bookmark after it.
        self.outline.invalidate();
        self.forms.invalidate();
        self.annotate.invalidate();
        self.invalidate();
    }

    /// Move the view to a page.
    pub fn go_to(&mut self, page: PageIndex) {
        let last = (self.page_count() - 1).max(0);
        self.current_page = page.clamp(0, last);
        self.scroll_to_current = true;
    }

    /// Resolve [`Self::zoom`] against the viewport, in points.
    ///
    /// Returns the scale factor from PDF points to screen points.
    #[must_use]
    pub fn resolve_scale(&self, viewport: egui::Vec2, page: PageIndex) -> f32 {
        let (w, h) = self.page_size_pt(page);
        if w <= 0.0 || h <= 0.0 {
            return 1.0;
        }
        match self.zoom {
            Zoom::Scale(scale) => scale,
            // The margin keeps the page off the scrollbar and the panel edge.
            Zoom::FitWidth => ((viewport.x - PAGE_MARGIN * 2.0) / w).max(0.05),
            Zoom::FitPage => {
                let by_width = (viewport.x - PAGE_MARGIN * 2.0) / w;
                let by_height = (viewport.y - PAGE_MARGIN * 2.0) / h;
                by_width.min(by_height).max(0.05)
            }
        }
    }
}

/// Space around a page in the scroll area, in screen points.
pub const PAGE_MARGIN: f32 = 16.0;

impl std::fmt::Debug for OpenDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenDocument")
            .field("id", &self.id)
            .field("path", &self.path)
            .field("pages", &self.page_count())
            .field("zoom", &self.zoom)
            .field("generation", &self.generation)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> OpenDocument {
        let mut doc = OpenDocument::opening(DocumentId::new(), PathBuf::from("t.pdf"), 64);
        doc.info = Some(DocumentInfo {
            path: PathBuf::from("t.pdf"),
            page_count: 3,
            page_sizes_pt: vec![(612.0, 792.0); 3],
        });
        doc
    }

    #[test]
    fn zoom_steps_move_one_stop_at_a_time() {
        assert_eq!(Zoom::zoom_in(1.0), Zoom::Scale(1.25));
        assert_eq!(Zoom::zoom_out(1.0), Zoom::Scale(0.75));
        // And clamp at the ends rather than running away.
        assert_eq!(Zoom::zoom_in(100.0), Zoom::Scale(8.0));
        assert_eq!(Zoom::zoom_out(0.01), Zoom::Scale(0.25));
    }

    #[test]
    fn rotation_swaps_the_page_box() {
        let mut doc = document();
        assert_eq!(doc.page_size_pt(0), (612.0, 792.0));
        doc.rotate();
        assert_eq!(doc.page_size_pt(0), (792.0, 612.0));
    }

    #[test]
    fn rotating_invalidates_outstanding_work() {
        let mut doc = document();
        let before = doc.generation;
        doc.rotate();
        assert!(doc.generation > before);
    }

    #[test]
    fn fit_width_fills_the_viewport() {
        let doc = document();
        let scale = doc.resolve_scale(egui::vec2(612.0 + PAGE_MARGIN * 2.0, 400.0), 0);
        assert!((scale - 1.0).abs() < 1e-3, "expected 1.0, got {scale}");
    }

    #[test]
    fn fit_page_uses_the_tighter_dimension() {
        let mut doc = document();
        doc.zoom = Zoom::FitPage;
        // Wide but short viewport: height is the binding constraint.
        let scale = doc.resolve_scale(egui::vec2(2000.0, 396.0 + PAGE_MARGIN * 2.0), 0);
        assert!((scale - 0.5).abs() < 1e-3, "expected 0.5, got {scale}");
    }

    #[test]
    fn going_past_the_end_clamps() {
        let mut doc = document();
        doc.go_to(99);
        assert_eq!(doc.current_page, 2);
        doc.go_to(-5);
        assert_eq!(doc.current_page, 0);
    }

    #[test]
    fn an_unmeasured_page_still_gets_a_box() {
        let doc = OpenDocument::opening(DocumentId::new(), PathBuf::from("t.pdf"), 8);
        assert_eq!(doc.page_size_pt(0), (612.0, 792.0));
    }
}
