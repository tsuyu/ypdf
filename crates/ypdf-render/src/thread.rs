//! The render thread and the handle the GUI talks to it through.
//!
//! PDFium keeps global state and is not thread-safe, so exactly one thread ever
//! touches it. Every document lives on that thread; the GUI holds only ids.
//! This is the constraint the whole architecture is built around — see
//! `PLAN.md` §2.1.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use pdfium_render::prelude::{
    PdfDocument, PdfFontWeight, PdfPageRenderRotation, PdfRenderConfig, Pdfium, Pixels,
};
use ypdf_core::{Error, ParseFailure, Result};

use crate::errors::from_pdfium;
use crate::library;
use crate::queue::RenderQueue;
use crate::text::{CharBox, FontFace, LinkTarget, PageAnalysis, PageLink, PageText, RectPt};
use crate::types::{
    DocumentId, DocumentInfo, PageIndex, Priority, QuarterTurns, RenderEvent, RenderRequest,
    RenderedPage,
};

/// Widest raster we will produce, in pixels.
///
/// Above this, GPU texture limits and memory cost both bite; the viewer tiles
/// or downsamples instead of asking for more.
const MAX_TARGET_WIDTH: u32 = 8192;

/// Guards the one-render-thread-at-a-time rule. PDFium has no lock of its own;
/// this is the lock.
static LIVE: AtomicBool = AtomicBool::new(false);

/// Instructions to the render thread.
#[derive(Debug)]
enum Command {
    Open {
        doc: DocumentId,
        path: PathBuf,
        password: Option<String>,
    },
    Close {
        doc: DocumentId,
    },
    Render(RenderRequest),
    Analyze {
        doc: DocumentId,
        page: PageIndex,
    },
    Reopen {
        doc: DocumentId,
        bytes: Vec<u8>,
    },
    Invalidate {
        doc: DocumentId,
        generation: u64,
    },
    Shutdown,
}

/// The GUI's connection to the render thread.
///
/// Dropping it shuts the thread down and waits for it, so PDFium is never torn
/// down while a render is in flight.
#[derive(Debug)]
pub struct RenderHandle {
    commands: Sender<Command>,
    events: Receiver<RenderEvent>,
    join: Option<JoinHandle<()>>,
}

impl RenderHandle {
    /// Start the render thread and bind PDFium on it.
    ///
    /// Returns the binding error rather than starting a thread that cannot
    /// work, so the application can report "PDFium not found" at startup with
    /// the paths it searched.
    pub fn spawn() -> Result<Self> {
        if LIVE.swap(true, Ordering::SeqCst) {
            return Err(Error::Backend {
                backend: "pdfium",
                detail: "A render thread is already running. PDFium is not thread-safe, so                          there is exactly one; share the existing RenderHandle."
                    .into(),
            });
        }

        let (commands_tx, commands_rx) = unbounded();
        let (events_tx, events_rx) = unbounded();
        let (ready_tx, ready_rx) = bounded(1);

        let join = std::thread::Builder::new()
            .name("ypdf-render".into())
            .spawn(move || render_thread(&commands_rx, &events_tx, &ready_tx))
            .map_err(|e| {
                LIVE.store(false, Ordering::SeqCst);
                Error::Backend {
                    backend: "pdfium",
                    detail: format!("Could not start the render thread: {e}"),
                }
            })?;

        // The thread reports the outcome of binding before it starts working.
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                commands: commands_tx,
                events: events_rx,
                join: Some(join),
            }),
            Ok(Err(e)) => {
                let _ = join.join();
                LIVE.store(false, Ordering::SeqCst);
                Err(e)
            }
            Err(_) => {
                let _ = join.join();
                LIVE.store(false, Ordering::SeqCst);
                Err(Error::Backend {
                    backend: "pdfium",
                    detail: "The render thread stopped before it was ready.".into(),
                })
            }
        }
    }

    /// Ask for a document to be opened. The id is usable immediately; an
    /// [`RenderEvent::Opened`] or [`RenderEvent::Failed`] follows.
    pub fn open(&self, path: impl Into<PathBuf>, password: Option<String>) -> DocumentId {
        let doc = DocumentId::new();
        self.send(Command::Open {
            doc,
            path: path.into(),
            password,
        });
        doc
    }

    /// Replace an open document with an edited copy held in memory.
    ///
    /// Editing happens in `ypdf-doc`, which never loads PDFium; handing the
    /// result back as bytes is what lets the viewer show the edit without a
    /// temporary file on disk. The id stays the same, so the tab keeps its
    /// place; an [`RenderEvent::Opened`] follows with the new structure, and
    /// every raster and text layer the caller holds for this document is stale
    /// from that point.
    pub fn reopen(&self, doc: DocumentId, bytes: Vec<u8>) {
        self.send(Command::Reopen { doc, bytes });
    }

    /// Close a document and release its memory.
    pub fn close(&self, doc: DocumentId) {
        self.send(Command::Close { doc });
    }

    /// Queue a page render.
    pub fn request(&self, request: RenderRequest) {
        self.send(Command::Render(request));
    }

    /// Ask for a page's text layer and links.
    ///
    /// Answered only when no page is waiting to be rasterized, so extracting
    /// text for a search can never stall the page being read.
    pub fn analyze(&self, doc: DocumentId, page: PageIndex) {
        self.send(Command::Analyze { doc, page });
    }

    /// Declare that everything rendered for `doc` before `generation` is
    /// worthless — after a zoom or a rotation.
    pub fn invalidate(&self, doc: DocumentId, generation: u64) {
        self.send(Command::Invalidate { doc, generation });
    }

    /// Drain everything the thread has sent since the last call. Never blocks,
    /// so it is safe to call once per frame.
    pub fn drain_events(&self) -> impl Iterator<Item = RenderEvent> + '_ {
        self.events.try_iter()
    }

    /// Block until the next event. For tests and headless callers.
    pub fn recv_event(&self) -> Option<RenderEvent> {
        self.events.recv().ok()
    }

    fn send(&self, command: Command) {
        // A closed channel means the thread is gone; the GUI keeps running and
        // simply stops getting pages.
        if self.commands.send(command).is_err() {
            tracing::warn!("render thread is gone; command dropped");
        }
    }
}

impl Drop for RenderHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        LIVE.store(false, Ordering::SeqCst);
    }
}

/// Thread body. Owns PDFium and every open document.
fn render_thread(
    commands: &Receiver<Command>,
    events: &Sender<RenderEvent>,
    ready: &Sender<Result<()>>,
) {
    let pdfium: &'static Pdfium = match library::instance() {
        Ok(pdfium) => pdfium,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };

    if ready.send(Ok(())).is_err() {
        return;
    }

    let mut documents: HashMap<DocumentId, PdfDocument<'static>> = HashMap::new();
    let mut queue = RenderQueue::new();
    // Text extraction is strictly background work, behind every raster.
    let mut analyses: VecDeque<(DocumentId, PageIndex)> = VecDeque::new();

    loop {
        // Block only when there is nothing to render; otherwise take whatever
        // has arrived and get back to work.
        let first = if queue.is_empty() && analyses.is_empty() {
            match commands.recv() {
                Ok(command) => Some(command),
                Err(_) => break,
            }
        } else {
            None
        };

        let mut stop = false;
        for command in first.into_iter().chain(commands.try_iter()) {
            if handle_command(
                command,
                pdfium,
                &mut documents,
                &mut queue,
                &mut analyses,
                events,
            ) {
                stop = true;
                break;
            }
        }
        if stop {
            break;
        }

        if let Some(request) = queue.pop() {
            // The request may have gone stale while it waited.
            if !queue.is_current(&request) {
                continue;
            }
            render_one(&request, &documents, events);
        } else if let Some((doc, page)) = analyses.pop_front() {
            analyze_one(doc, page, &documents, events);
        }
    }

    // Documents borrow from the leaked Pdfium; drop them before the thread ends.
    documents.clear();
    tracing::debug!("render thread stopped");
}

/// Returns true when the thread should stop.
fn handle_command(
    command: Command,
    pdfium: &'static Pdfium,
    documents: &mut HashMap<DocumentId, PdfDocument<'static>>,
    queue: &mut RenderQueue,
    analyses: &mut VecDeque<(DocumentId, PageIndex)>,
    events: &Sender<RenderEvent>,
) -> bool {
    match command {
        Command::Shutdown => return true,
        Command::Open {
            doc,
            path,
            password,
        } => match open_document(pdfium, &path, password.as_deref()) {
            Ok((document, info)) => {
                documents.insert(doc, document);
                emit(
                    events,
                    RenderEvent::Opened {
                        doc,
                        info: Box::new(info),
                    },
                );
            }
            Err(error) => emit(
                events,
                RenderEvent::Failed {
                    doc,
                    page: None,
                    error,
                },
            ),
        },
        Command::Reopen { doc, bytes } => {
            // Drop the old document first: its objects are about to be
            // replaced, and any queued work refers to pages that may no longer
            // exist.
            documents.remove(&doc);
            queue.drop_document(doc);
            analyses.retain(|(d, _)| *d != doc);

            match open_bytes(pdfium, bytes) {
                Ok((document, info)) => {
                    documents.insert(doc, document);
                    emit(
                        events,
                        RenderEvent::Opened {
                            doc,
                            info: Box::new(info),
                        },
                    );
                }
                Err(error) => emit(
                    events,
                    RenderEvent::Failed {
                        doc,
                        page: None,
                        error,
                    },
                ),
            }
        }
        Command::Close { doc } => {
            documents.remove(&doc);
            queue.drop_document(doc);
            analyses.retain(|(d, _)| *d != doc);
            emit(events, RenderEvent::Closed { doc });
        }
        Command::Render(request) => queue.push(request),
        Command::Analyze { doc, page } => {
            // Scrolling re-asks constantly; extracting a page twice is waste.
            if !analyses.contains(&(doc, page)) {
                analyses.push_back((doc, page));
            }
        }
        Command::Invalidate { doc, generation } => queue.observe_generation(doc, generation),
    }
    false
}

/// Open an edited document from memory.
fn open_bytes(
    pdfium: &'static Pdfium,
    bytes: Vec<u8>,
) -> Result<(PdfDocument<'static>, DocumentInfo)> {
    let document = pdfium
        .load_pdf_from_byte_vec(bytes, None)
        .map_err(|e| from_pdfium(&e, None))?;
    let info = measure(&document, PathBuf::new());
    Ok((document, info))
}

fn open_document(
    pdfium: &'static Pdfium,
    path: &PathBuf,
    password: Option<&str>,
) -> Result<(PdfDocument<'static>, DocumentInfo)> {
    if !path.exists() {
        return Err(Error::io(
            path,
            std::io::Error::from(std::io::ErrorKind::NotFound),
        ));
    }

    let document = pdfium
        .load_pdf_from_file(path, password)
        .map_err(|e| match (&e, password) {
            // PDFium cannot distinguish "needs a password" from "wrong
            // password"; if we supplied none, it is the former.
            (_, None) if is_password_error(&e) => Error::PasswordRequired.with_path(path),
            _ => from_pdfium(&e, Some(path)),
        })?;

    let info = measure(&document, path.clone());
    tracing::info!(path = %path.display(), pages = info.page_count, "document opened");
    Ok((document, info))
}

/// Read a document's structure: page count and page sizes.
fn measure(document: &PdfDocument<'static>, path: PathBuf) -> DocumentInfo {
    let pages = document.pages();
    let page_count = pages.len();
    let mut page_sizes_pt = Vec::with_capacity(usize::try_from(page_count).unwrap_or(0));
    for index in 0..page_count {
        match pages.get(index) {
            Ok(page) => page_sizes_pt.push((page.width().value, page.height().value)),
            // One unreadable page must not stop the document from opening; the
            // viewer shows a placeholder and diagnostics will say why.
            Err(e) => {
                tracing::warn!(page = index, "could not measure page: {e:?}");
                page_sizes_pt.push((0.0, 0.0));
            }
        }
    }

    DocumentInfo {
        path,
        page_count,
        page_sizes_pt,
    }
}

fn is_password_error(e: &pdfium_render::prelude::PdfiumError) -> bool {
    use pdfium_render::prelude::{PdfiumError, PdfiumInternalError};
    matches!(
        e,
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError)
    )
}

fn render_one(
    request: &RenderRequest,
    documents: &HashMap<DocumentId, PdfDocument<'static>>,
    events: &Sender<RenderEvent>,
) {
    let Some(document) = documents.get(&request.doc) else {
        // Closed between queueing and rendering. Not an error.
        return;
    };

    let started = std::time::Instant::now();
    match raster(document, request) {
        Ok(page) => {
            tracing::trace!(
                page = request.page,
                width = page.width,
                height = page.height,
                ms = started.elapsed().as_millis(),
                "page rendered"
            );
            emit(events, RenderEvent::Page(Box::new(page)));
        }
        Err(error) => emit(
            events,
            RenderEvent::Failed {
                doc: request.doc,
                page: Some(request.page),
                error,
            },
        ),
    }
}

fn raster(document: &PdfDocument<'static>, request: &RenderRequest) -> Result<RenderedPage> {
    let pages = document.pages();
    let count = pages.len();
    if request.page < 0 || request.page >= count {
        return Err(Error::PageOutOfRange {
            requested: u32::try_from(request.page.saturating_add(1)).unwrap_or(u32::MAX),
            pages: u32::try_from(count).unwrap_or(u32::MAX),
        });
    }

    let page = pages.get(request.page).map_err(|e| from_pdfium(&e, None))?;

    // The canvas is sized here rather than by `set_target_width`, because that
    // derives the height from the *unrotated* page and a quarter turn would
    // then squeeze the content into a portrait canvas.
    let (page_w, page_h) = (page.width().value, page.height().value);
    let (page_w, page_h) = if request.rotation.is_quarter() {
        (page_h, page_w)
    } else {
        (page_w, page_h)
    };
    if page_w <= 0.0 || page_h <= 0.0 {
        return Err(Error::parse(ParseFailure::Other {
            detail: format!("Page {} has no usable dimensions.", request.page + 1),
        }));
    }

    let width = request.target_width.clamp(1, MAX_TARGET_WIDTH);
    #[expect(clippy::cast_precision_loss, reason = "pixel dimensions are small")]
    let height_f = (width as f32) * (page_h / page_w);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped above 1"
    )]
    let height = height_f.round().clamp(1.0, MAX_TARGET_WIDTH as f32) as u32;

    let config = PdfRenderConfig::new()
        .set_target_size(
            Pixels::try_from(width).unwrap_or(Pixels::MAX),
            Pixels::try_from(height).unwrap_or(Pixels::MAX),
        )
        .rotate(rotation(request.rotation), false);

    let bitmap = page
        .render_with_config(&config)
        .map_err(|e| from_pdfium(&e, None))?;
    let (w, h) = (bitmap.width(), bitmap.height());
    let rgba = bitmap.as_rgba_bytes();

    Ok(RenderedPage {
        doc: request.doc,
        page: request.page,
        generation: request.generation,
        width: u32::try_from(w).unwrap_or(0),
        height: u32::try_from(h).unwrap_or(0),
        rotation: request.rotation,
        rgba,
    })
}

const fn rotation(turns: QuarterTurns) -> PdfPageRenderRotation {
    match turns.normalized() {
        1 => PdfPageRenderRotation::Degrees90,
        2 => PdfPageRenderRotation::Degrees180,
        3 => PdfPageRenderRotation::Degrees270,
        _ => PdfPageRenderRotation::None,
    }
}

fn analyze_one(
    doc: DocumentId,
    page: PageIndex,
    documents: &HashMap<DocumentId, PdfDocument<'static>>,
    events: &Sender<RenderEvent>,
) {
    let Some(document) = documents.get(&doc) else {
        return;
    };

    let started = std::time::Instant::now();
    match analyze(document, page) {
        Ok(analysis) => {
            tracing::debug!(
                page,
                chars = analysis.text.chars.len(),
                links = analysis.links.len(),
                ms = started.elapsed().as_millis(),
                "page analyzed"
            );
            emit(
                events,
                RenderEvent::Analyzed {
                    doc,
                    analysis: Box::new(analysis),
                },
            );
        }
        Err(error) => emit(
            events,
            RenderEvent::Failed {
                doc,
                page: Some(page),
                error,
            },
        ),
    }
}

fn analyze(document: &PdfDocument<'static>, index: PageIndex) -> Result<PageAnalysis> {
    let pages = document.pages();
    let count = pages.len();
    if index < 0 || index >= count {
        return Err(Error::PageOutOfRange {
            requested: u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX),
            pages: u32::try_from(count).unwrap_or(u32::MAX),
        });
    }
    let page = pages.get(index).map_err(|e| from_pdfium(&e, None))?;

    let mut chars = Vec::new();
    let mut fonts: Vec<FontFace> = Vec::new();
    if let Ok(text) = page.text() {
        for c in text.chars().iter() {
            // A character with no box cannot be highlighted or hit-tested;
            // skipping it here keeps text and boxes aligned, which selection,
            // search highlighting, and copy all depend on.
            let (Some(ch), Ok(bounds)) = (c.unicode_char(), c.loose_bounds()) else {
                continue;
            };
            let face = FontFace {
                name: c.font_name(),
                weight: font_weight(c.font_weight()),
                italic: c.font_is_italic(),
                serif: c.font_is_serif(),
                fixed_pitch: c.font_is_fixed_pitch(),
            };
            chars.push(CharBox {
                ch,
                rect: rect_pt(&bounds),
                size: c.scaled_font_size().value,
                font: intern_font(&mut fonts, face),
            });
        }
    }

    let mut links = Vec::new();
    for link in page.links().iter() {
        let Ok(rect) = link.rect() else { continue };
        links.push(PageLink {
            rect: rect_pt(&rect),
            target: link_target(&link),
        });
    }

    Ok(PageAnalysis {
        page: index,
        text: PageText { chars, fonts },
        links,
    })
}

/// Add `face` to the page's table if it is new, and return its index.
///
/// A linear scan rather than a map: a page draws in a handful of faces, and
/// first-seen order is worth more than lookup speed at that size.
fn intern_font(fonts: &mut Vec<FontFace>, face: FontFace) -> u16 {
    if let Some(found) = fonts.iter().position(|f| *f == face) {
        return u16::try_from(found).unwrap_or(0);
    }
    let next = u16::try_from(fonts.len()).unwrap_or(0);
    fonts.push(face);
    next
}

/// PDFium's font weight as a plain number.
fn font_weight(weight: Option<PdfFontWeight>) -> Option<u32> {
    use PdfFontWeight::{
        Custom, Weight100, Weight200, Weight300, Weight400Normal, Weight500, Weight600,
        Weight700Bold, Weight800, Weight900,
    };
    Some(match weight? {
        Weight100 => 100,
        Weight200 => 200,
        Weight300 => 300,
        Weight400Normal => 400,
        Weight500 => 500,
        Weight600 => 600,
        Weight700Bold => 700,
        Weight800 => 800,
        Weight900 => 900,
        Custom(other) => other,
    })
}

fn rect_pt(rect: &pdfium_render::prelude::PdfRect) -> RectPt {
    RectPt {
        left: rect.left().value,
        bottom: rect.bottom().value,
        right: rect.right().value,
        top: rect.top().value,
    }
}

fn link_target(link: &pdfium_render::prelude::PdfLink) -> LinkTarget {
    use pdfium_render::prelude::PdfAction;

    let Some(action) = link.action() else {
        return link
            .destination()
            .and_then(|d| d.page_index().ok())
            .map_or(LinkTarget::Unsupported("no action"), LinkTarget::Page);
    };

    match action {
        PdfAction::Uri(uri) => uri
            .uri()
            .map_or(LinkTarget::Unsupported("URI"), LinkTarget::Url),
        PdfAction::LocalDestination(_) => link
            .destination()
            .and_then(|d| d.page_index().ok())
            .map_or(LinkTarget::Unsupported("destination"), LinkTarget::Page),
        // Never followed. A launch action runs a program; yPDF reports it
        // (spec section 20) rather than offering it as something to click.
        PdfAction::Launch(_) => LinkTarget::Unsupported("launch action"),
        PdfAction::RemoteDestination(_) => LinkTarget::Unsupported("remote document"),
        PdfAction::EmbeddedDestination(_) => LinkTarget::Unsupported("embedded document"),
        PdfAction::Unsupported(_) => LinkTarget::Unsupported("unsupported action"),
    }
}

fn emit(events: &Sender<RenderEvent>, event: RenderEvent) {
    if events.send(event).is_err() {
        tracing::debug!("event receiver is gone");
    }
}

/// Convenience: the priority a page should be rendered at, given how far it is
/// from the viewport.
#[must_use]
pub const fn priority_for_distance(pages_from_viewport: i32) -> Priority {
    match pages_from_viewport {
        0 => Priority::Visible,
        -2..=2 => Priority::Nearby,
        _ => Priority::Thumbnail,
    }
}

/// The page indices worth having ready around `current`, nearest first.
#[must_use]
pub fn prefetch_window(current: PageIndex, count: PageIndex, radius: PageIndex) -> Vec<PageIndex> {
    let mut pages = Vec::new();
    for offset in 0..=radius {
        for page in [current + offset, current - offset] {
            if page >= 0 && page < count && !pages.contains(&page) {
                pages.push(page);
            }
        }
    }
    pages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefetch_starts_at_the_current_page_and_spreads_outward() {
        assert_eq!(prefetch_window(5, 100, 2), vec![5, 6, 4, 7, 3]);
    }

    #[test]
    fn prefetch_stays_inside_the_document() {
        assert_eq!(prefetch_window(0, 2, 3), vec![0, 1]);
    }

    #[test]
    fn distance_maps_to_priority() {
        assert_eq!(priority_for_distance(0), Priority::Visible);
        assert_eq!(priority_for_distance(-2), Priority::Nearby);
        assert_eq!(priority_for_distance(9), Priority::Thumbnail);
    }

    #[test]
    fn rotations_wrap() {
        assert_eq!(rotation(QuarterTurns(4)), PdfPageRenderRotation::None);
        assert_eq!(rotation(QuarterTurns(5)), PdfPageRenderRotation::Degrees90);
    }
}
