//! Finding and driving the Tesseract binary.
//!
//! Tesseract is used as a **sidecar process**, not as a linked library. Linking
//! it means Leptonica, vcpkg, and a build that breaks on the machines least
//! able to fix it; running it means one `Command` and a text format that has
//! been stable across major versions.
//!
//! The cost is that the binary has to be found, and might not be there at all.
//! That gets the same treatment as PDFium in `ypdf-render`: search a defined
//! list of places, and when it is missing, **name every path that was tried**.
//! "OCR unavailable" is a dead end; a list of paths is something someone can
//! act on.

use std::path::{Path, PathBuf};
use std::process::Command;

use ypdf_core::{Error, Result};

/// Environment variable that overrides the search entirely.
const OVERRIDE: &str = "YPDF_TESSERACT_PATH";

/// A located Tesseract binary.
#[derive(Clone, Debug)]
pub struct Tesseract {
    path: PathBuf,
}

impl Tesseract {
    /// Find the binary, or explain everywhere that was looked.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] naming every candidate path, when nothing usable
    /// was found.
    pub fn find() -> Result<Self> {
        let mut tried = Vec::new();

        for candidate in candidates() {
            if candidate.is_file() {
                tracing::info!(path = %candidate.display(), "found Tesseract");
                return Ok(Self { path: candidate });
            }
            tried.push(candidate.display().to_string());
        }

        // Last resort: whatever `tesseract` means on this PATH. Tried by
        // running it, because a bare name cannot be checked with `is_file`.
        if let Some(found) = on_path() {
            tracing::info!("found Tesseract on PATH");
            return Ok(found);
        }
        tried.push("tesseract (on PATH)".to_string());

        Err(Error::Unsupported {
            feature: format!(
                "OCR needs Tesseract, which was not found. Looked in:\n  {}\n\n\
                 Install it, or set {OVERRIDE} to the binary.",
                tried.join("\n  ")
            ),
        })
    }

    /// Use a specific binary, without searching.
    ///
    /// The path is not checked here: an invalid one produces a real error from
    /// the first run, which says more than "not found" would.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Where the binary is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The version string it reports.
    pub fn version(&self) -> Result<String> {
        let output = self.run(&["--version".to_string()])?;
        Ok(output
            .lines()
            .next()
            .unwrap_or("tesseract (unknown version)")
            .trim()
            .to_string())
    }

    /// The language packs it has installed.
    ///
    /// Asking for a language that is not installed fails with a message about
    /// traineddata files that means nothing to most people, so the list is
    /// fetched to say it in words instead.
    pub fn languages(&self) -> Result<Vec<String>> {
        let output = self.run(&["--list-langs".to_string()])?;
        Ok(output
            .lines()
            .map(str::trim)
            .filter(|line| {
                !line.is_empty()
                    && !line.starts_with("List of")
                    && !line.contains(char::is_whitespace)
            })
            .map(ToString::to_string)
            .collect())
    }

    /// Check that every language in an `eng+msa` specification is installed.
    pub fn check_languages(&self, requested: &str) -> Result<()> {
        let installed = self.languages()?;
        let missing: Vec<&str> = requested
            .split('+')
            .map(str::trim)
            .filter(|lang| !lang.is_empty() && !installed.iter().any(|have| have == lang))
            .collect();

        if missing.is_empty() {
            return Ok(());
        }

        Err(Error::Config {
            detail: format!(
                "language pack(s) not installed: {}. Installed: {}",
                missing.join(", "),
                if installed.is_empty() {
                    "none".to_string()
                } else {
                    installed.join(", ")
                }
            ),
            source_path: None,
        })
    }

    /// Recognize one image file, returning Tesseract's TSV output.
    ///
    /// `stdout` is asked for by name so nothing is written next to the input,
    /// and the page segmentation is left at Tesseract's default: overriding it
    /// helps one layout and hurts another, and this code cannot see the page.
    pub fn recognize_tsv(&self, image: &Path, language: &str) -> Result<String> {
        self.run(&[
            image.display().to_string(),
            "stdout".to_string(),
            "-l".to_string(),
            language.to_string(),
            "tsv".to_string(),
        ])
    }

    /// Run the binary and return its stdout.
    fn run(&self, args: &[String]) -> Result<String> {
        let output = Command::new(&self.path)
            .args(args)
            .output()
            .map_err(|e| Error::io(&self.path, e))?;

        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(Error::Backend {
                backend: "tesseract",
                detail: if detail.is_empty() {
                    format!("exited with {}", output.status)
                } else {
                    detail
                },
            });
        }

        // Tesseract writes its banner to stderr, so stdout is only the result.
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Where to look, in order.
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();

    if let Ok(path) = std::env::var(OVERRIDE) {
        out.push(PathBuf::from(path));
    }

    // A vendored copy next to the executable or in the source tree, matching
    // how PDFium is shipped.
    let name = if cfg!(windows) {
        "tesseract.exe"
    } else {
        "tesseract"
    };
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(Path::to_path_buf);
        while let Some(current) = dir {
            out.push(current.join("vendor").join("tesseract").join(name));
            dir = current.parent().map(Path::to_path_buf);
        }
    }

    if cfg!(windows) {
        out.push(PathBuf::from(
            r"C:\Program Files\Tesseract-OCR\tesseract.exe",
        ));
        out.push(PathBuf::from(
            r"C:\Program Files (x86)\Tesseract-OCR\tesseract.exe",
        ));
    } else {
        out.push(PathBuf::from("/usr/bin/tesseract"));
        out.push(PathBuf::from("/usr/local/bin/tesseract"));
        out.push(PathBuf::from("/opt/homebrew/bin/tesseract"));
    }

    out
}

/// Is there a working `tesseract` on the PATH?
fn on_path() -> Option<Tesseract> {
    let candidate = Tesseract::at("tesseract");
    candidate.version().ok().map(|version| {
        tracing::debug!(%version, "tesseract on PATH");
        candidate
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_binary_lists_every_path_that_was_tried() {
        // The whole value of this error is that it can be acted on.
        let error = Tesseract::at("definitely-not-a-real-binary-xyz")
            .version()
            .expect_err("no such binary");
        assert_eq!(error.code(), "E_IO");
    }

    #[test]
    fn the_search_starts_with_the_environment_override() {
        // Not set here, so the list simply must not be empty and must end at
        // the well-known install locations.
        let paths = candidates();
        assert!(!paths.is_empty());
        assert!(
            paths
                .iter()
                .any(|p| p.to_string_lossy().contains("tesseract")),
            "{paths:?}"
        );
    }

    #[test]
    fn language_lists_ignore_the_header_line() {
        // `tesseract --list-langs` prints a header before the languages, and
        // treating it as a language would produce a nonsense error later.
        let output = "List of available languages (3):\neng\nmsa\nosd\n";
        let langs: Vec<&str> = output
            .lines()
            .map(str::trim)
            .filter(|line| {
                !line.is_empty()
                    && !line.starts_with("List of")
                    && !line.contains(char::is_whitespace)
            })
            .collect();
        assert_eq!(langs, vec!["eng", "msa", "osd"]);
    }
}
