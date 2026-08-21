//! AcroForm fields: reading, filling, flattening, and interchange (spec §16).
//!
//! Three rules run through the crate.
//!
//! **Value and appearance are one job.** Writing `/V` without rebuilding the
//! appearance stream produces a file that looks filled in one reader, empty in
//! another, and prints blank. Everything here writes both.
//!
//! **A signature field is never filled.** Putting a value in one produces a
//! document that claims to be signed and is not, and no argument about
//! convenience outweighs that.
//!
//! **A name that does not exist is reported, not ignored.** A typo in a field
//! name would otherwise look exactly like a successful fill, which is the worst
//! possible outcome for a form someone is about to send.
//!
//! ```no_run
//! use std::collections::BTreeMap;
//! use ypdf_doc::Pdf;
//! use ypdf_forms::{fill, read};
//!
//! # fn main() -> ypdf_core::Result<()> {
//! let mut pdf = Pdf::open("application.pdf")?;
//! for field in read(&pdf) {
//!     println!("{}: {}", field.name, field.value);
//! }
//!
//! let values = BTreeMap::from([("name".to_string(), "Ada Lovelace".to_string())]);
//! let report = fill(&mut pdf, &values)?;
//! assert!(report.is_complete());
//! pdf.save("application-filled.pdf")?;
//! # Ok(())
//! # }
//! ```

pub mod data;
mod edit;
mod model;

pub use data::{Format, export, import, values_of};
pub use edit::{Report, clear, fill, flatten};
pub use model::{Field, Kind, acroform, has_form, read};
