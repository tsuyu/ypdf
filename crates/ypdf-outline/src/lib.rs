//! Bookmarks and links: how a document is navigated (spec §17, §18).
//!
//! Both are structure rather than content — they say where to go, not what the
//! page shows — so they are read and written here together, and neither touches
//! a content stream.
//!
//! ```no_run
//! use ypdf_doc::Pdf;
//! use ypdf_outline::{Bookmark, bookmarks};
//!
//! # fn main() -> ypdf_core::Result<()> {
//! let mut pdf = Pdf::open("report.pdf")?;
//! let mut tree = bookmarks::read(&pdf);
//! tree.push(Bookmark::new("Appendix", pdf.page_count()));
//! bookmarks::write(&mut pdf, &tree)?;
//! pdf.save("report-with-bookmarks.pdf")?;
//! # Ok(())
//! # }
//! ```

pub mod bookmarks;
pub mod links;

pub use bookmarks::Bookmark;
pub use links::{Link, Target};
