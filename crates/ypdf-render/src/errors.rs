//! Translating PDFium failures into the engine's error vocabulary.

use std::path::Path;

use pdfium_render::prelude::{PdfiumError, PdfiumInternalError};
use ypdf_core::{Error, ParseFailure};

/// Map a PDFium failure onto [`Error`], preserving the distinctions the user
/// can act on: a wrong password is not a corrupt file, and neither is a bug.
pub fn from_pdfium(e: &PdfiumError, path: Option<&Path>) -> Error {
    let error = match e {
        PdfiumError::PdfiumLibraryInternalError(internal) => match internal {
            PdfiumInternalError::PasswordError => Error::WrongPassword,
            PdfiumInternalError::SecurityError => Error::PermissionDenied {
                action: "this operation",
            },
            PdfiumInternalError::FormatError => Error::parse(ParseFailure::Other {
                detail: "PDFium could not parse the document structure.".into(),
            }),
            PdfiumInternalError::FileError => Error::parse(ParseFailure::Other {
                detail: "The file could not be read as a PDF.".into(),
            }),
            PdfiumInternalError::PageError => Error::Backend {
                backend: "pdfium",
                detail: "The page could not be loaded.".into(),
            },
            PdfiumInternalError::Unknown => Error::Backend {
                backend: "pdfium",
                detail: "Unknown PDFium error.".into(),
            },
        },
        PdfiumError::IoError(io) => Error::Io {
            path: None,
            source: clone_io(io),
        },
        PdfiumError::PageIndexOutOfBounds => Error::PageOutOfRange {
            requested: 0,
            pages: 0,
        },
        other => Error::Backend {
            backend: "pdfium",
            detail: format!("{other:?}"),
        },
    };

    match path {
        Some(path) => error.with_path(path),
        None => error,
    }
}

/// `std::io::Error` is not `Clone`; rebuild an equivalent one.
fn clone_io(source: &std::io::Error) -> std::io::Error {
    std::io::Error::new(source.kind(), source.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrong_password_is_not_a_parse_failure() {
        let e = from_pdfium(
            &PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError),
            None,
        );
        assert_eq!(e.code(), "E_PASSWORD_WRONG");
    }

    #[test]
    fn a_format_error_is_a_parse_failure_carrying_the_path() {
        let e = from_pdfium(
            &PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::FormatError),
            Some(Path::new("broken.pdf")),
        );
        assert_eq!(e.code(), "E_PARSE");
        assert_eq!(e.path(), Some(Path::new("broken.pdf")));
    }
}
