//! PDF security scanning (spec §20, §26).
//!
//! A PDF is not just paper. It can carry JavaScript, launch external programs,
//! embed executables, phone out to a URL when opened, and hide all of it in
//! places a reader never shows. This crate walks every object and reports what
//! is there, classified by how much it should worry someone.
//!
//! Two rules shape the whole design:
//!
//! * **Nothing found here is ever executed.** The vendored PDFium build has no
//!   JavaScript engine, and this crate only reads bytes. Detecting a launch
//!   action must never become performing one.
//! * **Findings are evidence, not verdicts.** A form that submits to a URL is
//!   normal in an expense claim and alarming in an invoice from a stranger. The
//!   scanner says what is in the file and how unusual it is; the person reading
//!   the report supplies the context.
//!
//! ```no_run
//! use ypdf_doc::Pdf;
//! use ypdf_security::{Severity, scan};
//!
//! # fn main() -> ypdf_core::Result<()> {
//! let pdf = Pdf::open("suspicious.pdf")?;
//! let report = scan(&pdf);
//! if report.worst() >= Some(Severity::High) {
//!     println!("{}", report.to_human());
//! }
//! # Ok(())
//! # }
//! ```

mod scan;
mod urls;

pub use scan::{Finding, ScanReport, scan};
pub use urls::{UrlKind, classify_url};
pub use ypdf_doc::Severity;
