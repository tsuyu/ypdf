//! PDF structure operations: pages, merge, split.
//!
//! This is the pure-Rust half of the engine. It never rasterizes anything and
//! never loads PDFium, so its operations are safe to run in parallel across
//! files — which is what makes batch processing (spec §21) cheap.
//!
//! ```no_run
//! use ypdf_doc::{PageSpec, Pdf};
//!
//! # fn main() -> ypdf_core::Result<()> {
//! let mut pdf = Pdf::open("input.pdf")?;
//! let pages = PageSpec::parse("1-10,20")?.resolve(pdf.page_count())?;
//! pdf.extract(&pages)?;
//! pdf.save("output.pdf")?;
//! # Ok(())
//! # }
//! ```

mod diagnostics;
mod metadata;
mod pdf;
mod spec;
mod split;
pub mod winansi;

pub use diagnostics::{Diagnostics, Issue, Severity};
pub use metadata::{Metadata, MetadataEdit, decode_text, encode_text, format_date};
pub use pdf::{Encryption, Pdf};
pub use spec::PageSpec;
pub use split::{Piece, SplitMode, split};
