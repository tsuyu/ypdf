//! PDF rendering, backed by PDFium on a dedicated thread.
//!
//! PDFium is not thread-safe, so this crate owns the only thread allowed to
//! touch it. Callers hold a [`RenderHandle`], send [`RenderRequest`]s, and
//! collect [`RenderEvent`]s once per frame. Nothing blocks the caller.
//!
//! ```no_run
//! use ypdf_render::{Priority, QuarterTurns, RenderEvent, RenderHandle, RenderRequest};
//!
//! # fn main() -> ypdf_core::Result<()> {
//! let render = RenderHandle::spawn()?;
//! let doc = render.open("document.pdf", None);
//!
//! while let Some(event) = render.recv_event() {
//!     match event {
//!         RenderEvent::Opened { info, .. } => {
//!             render.request(RenderRequest {
//!                 doc,
//!                 page: 0,
//!                 target_width: 1200,
//!                 rotation: QuarterTurns(0),
//!                 priority: Priority::Visible,
//!                 generation: 1,
//!             });
//!             println!("{} pages", info.page_count);
//!         }
//!         RenderEvent::Page(page) => {
//!             println!("{}x{} pixels", page.width, page.height);
//!             break;
//!         }
//!         RenderEvent::Failed { error, .. } => return Err(error),
//!         RenderEvent::Analyzed { .. } | RenderEvent::Closed { .. } => break,
//!     }
//! }
//! # Ok(())
//! # }
//! ```

mod errors;
mod library;
mod queue;
mod text;
mod thread;
mod types;

pub use queue::RenderQueue;
pub use text::{
    CharBox, FontFace, LinkTarget, PageAnalysis, PageLink, PageText, RectPt, SearchOptions,
    TextMatch, find_matches,
};
pub use thread::{RenderHandle, prefetch_window, priority_for_distance};
pub use types::{
    DocumentId, DocumentInfo, PageIndex, Priority, QuarterTurns, RenderEvent, RenderRequest,
    RenderedPage,
};
