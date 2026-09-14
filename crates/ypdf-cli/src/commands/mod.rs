//! One module per family of commands.

pub mod annotate;
pub mod convert;
pub mod forms;
pub mod inspect;
pub mod navigate;
pub mod ocr;
pub mod optimize;
pub mod pages;
pub mod pdfa;
pub mod protect;
pub mod redact;
pub mod signatures;
pub mod watermark;

use std::path::Path;

use ypdf_core::Result;
use ypdf_doc::Pdf;

/// Open an input, with a password if one was given.
///
/// One place, so that every command reads an encrypted document the same way
/// and none of them can accidentally be the one that does not.
pub fn open_input(path: &Path, password: Option<&str>) -> Result<Pdf> {
    match password {
        Some(password) => Pdf::open_with_password(path, password),
        None => Pdf::open(path),
    }
}
