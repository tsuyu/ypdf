//! Finding and binding the PDFium shared library.
//!
//! The library is a native binary that ships beside the application rather than
//! inside it, so "where is it?" is a first-class failure mode with an
//! actionable message (spec §35), not a panic.
//!
//! Search order:
//!
//! 1. `YPDF_PDFIUM_PATH` — a directory or a direct path to the library;
//! 2. `vendor/pdfium/<target-triple>/bin/` next to the executable (installed layout);
//! 3. the same path walking up from the executable (a `target/debug` dev build);
//! 4. beside the executable itself;
//! 5. the system library.
//!
//! The vendored build is deliberately the **non-V8** PDFium: yPDF detects
//! JavaScript in a document (spec §20, §26) and must never be able to run it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use pdfium_render::prelude::{Pdfium, PdfiumLibraryBindings};
use ypdf_core::{Error, Result};

/// The target triple this binary was built for, used to pick a vendor subdirectory.
const TARGET_TRIPLE: &str = env!("YPDF_TARGET_TRIPLE");

/// The one PDFium instance this process will ever have.
///
/// PDFium initializes global state exactly once and aborts if asked twice, so
/// the instance is created on first use and kept for the life of the process.
/// It is leaked deliberately: documents borrow from it, and this is what lets
/// the render thread hold them in a plain map instead of a self-referential
/// struct. A restarted render thread reuses the same instance.
///
/// Access is still confined to the render thread — `Pdfium` is not thread-safe,
/// and nothing here makes it so.
pub fn instance() -> Result<&'static Pdfium> {
    static INSTANCE: OnceLock<std::result::Result<&'static Pdfium, String>> = OnceLock::new();

    match INSTANCE.get_or_init(|| match bind() {
        Ok(bindings) => Ok(&*Box::leak(Box::new(Pdfium::new(bindings)))),
        // The error is stored as text because `Error` is not `Clone` and this
        // has to be reportable to every later caller.
        Err(e) => Err(e.report().to_human()),
    }) {
        Ok(pdfium) => Ok(*pdfium),
        Err(detail) => Err(Error::Backend {
            backend: "pdfium",
            detail: detail.clone(),
        }),
    }
}

/// Bind to PDFium, reporting every path that was tried when it cannot be found.
fn bind() -> Result<Box<dyn PdfiumLibraryBindings>> {
    let mut tried = Vec::new();

    for dir in candidate_dirs() {
        let path = Pdfium::pdfium_platform_library_name_at_path(&dir);
        match Pdfium::bind_to_library(&path) {
            Ok(bindings) => {
                tracing::info!(path = %path.display(), "bound to PDFium");
                return Ok(bindings);
            }
            Err(e) => {
                tracing::debug!(path = %path.display(), "PDFium not here: {e}");
                tried.push(path);
            }
        }
    }

    match Pdfium::bind_to_system_library() {
        Ok(bindings) => {
            tracing::info!("bound to the system PDFium");
            Ok(bindings)
        }
        Err(e) => Err(Error::Backend {
            backend: "pdfium",
            detail: format!(
                "Could not load the PDFium library ({e}).\n\nSearched:\n{}\n\nRun \
                 scripts/fetch-pdfium.ps1 (Windows) or scripts/fetch-pdfium.sh, or set \
                 YPDF_PDFIUM_PATH to the directory holding the library.",
                tried
                    .iter()
                    .map(|p| format!("  {}", p.display()))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        }),
    }
}

/// Directories to search, in precedence order.
fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(raw) = std::env::var_os("YPDF_PDFIUM_PATH") {
        let path = PathBuf::from(raw);
        // Accept either the directory or the library file itself.
        if path.is_file() {
            if let Some(parent) = path.parent() {
                dirs.push(parent.to_path_buf());
            }
        } else {
            dirs.push(path);
        }
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        dirs.push(vendor_dir(exe_dir));
        // A dev build lives in target/debug; the vendor directory sits at the
        // workspace root, two levels up.
        for ancestor in exe_dir.ancestors().skip(1).take(3) {
            dirs.push(vendor_dir(ancestor));
        }
        dirs.push(exe_dir.to_path_buf());
    }

    dirs
}

fn vendor_dir(root: &Path) -> PathBuf {
    root.join("vendor")
        .join("pdfium")
        .join(TARGET_TRIPLE)
        .join("bin")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_include_a_vendor_directory() {
        let dirs = candidate_dirs();
        assert!(
            dirs.iter().any(|d| d.to_string_lossy().contains("vendor")),
            "expected a vendor path among {dirs:?}"
        );
    }

    #[test]
    fn vendor_layout_is_triple_scoped() {
        let dir = vendor_dir(Path::new("/root"));
        assert!(dir.ends_with(Path::new(TARGET_TRIPLE).join("bin")));
    }
}
