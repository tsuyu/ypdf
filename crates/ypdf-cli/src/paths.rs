//! Deciding where output goes, and refusing to destroy anything on the way.
//!
//! The overwrite guard is the important part. A batch run is exactly the
//! situation where writing over an existing file is both easy to do by accident
//! and impossible to undo, so every write goes through [`guard`] and every
//! command needs an explicit `--overwrite` to replace anything.

use std::path::{Path, PathBuf};

use ypdf_core::{Error, Result};

/// Refuse to write over an existing file unless told to.
pub fn guard(path: &Path, overwrite: bool) -> Result<()> {
    if overwrite || !path.exists() {
        return Ok(());
    }
    Err(Error::OutputExists {
        path: path.to_path_buf(),
    })
}

/// Create a directory, and everything above it, if it is not there.
pub fn ensure_dir(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))
}

/// Where one input's output belongs.
///
/// With several inputs the destination is always a directory: writing every
/// file of a batch to one path would silently leave only the last one.
pub fn output_for(destination: &Path, input: &Path, many: bool, suffix: &str) -> Result<PathBuf> {
    let treat_as_dir = many || destination.is_dir() || ends_with_separator(destination);

    if !treat_as_dir {
        return Ok(destination.to_path_buf());
    }

    ensure_dir(destination)?;
    let stem = input.file_stem().map_or_else(
        || "document".to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    Ok(destination.join(format!("{stem}{suffix}.pdf")))
}

/// Does the path end in a separator, i.e. did the user write `out/`?
fn ends_with_separator(path: &Path) -> bool {
    path.to_string_lossy().ends_with(['/', '\\'])
}

/// The name to use for a per-input subdirectory.
#[must_use]
pub fn stem_of(input: &Path) -> String {
    input.file_stem().map_or_else(
        || "document".to_string(),
        |s| s.to_string_lossy().into_owned(),
    )
}

/// Write bytes, honouring the overwrite guard.
pub fn write(path: &Path, bytes: &[u8], overwrite: bool) -> Result<()> {
    guard(path, overwrite)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        ensure_dir(parent)?;
    }
    std::fs::write(path, bytes).map_err(|e| Error::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_file_is_refused_without_permission() {
        let existing = std::env::current_exe().expect("the test binary exists");
        assert!(guard(&existing, false).is_err());
        assert!(guard(&existing, true).is_ok(), "--overwrite allows it");
    }

    #[test]
    fn a_missing_file_is_always_fine() {
        assert!(guard(Path::new("nothing-here-at-all.pdf"), false).is_ok());
    }

    #[test]
    fn several_inputs_always_land_in_a_directory() {
        let out = std::env::temp_dir().join("ypdf-cli-test-out");
        let path =
            output_for(&out, Path::new("a/report.pdf"), true, "-compressed").expect("resolves");
        assert_eq!(path, out.join("report-compressed.pdf"));
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn one_input_writes_to_the_file_it_was_given() {
        let path =
            output_for(Path::new("out.pdf"), Path::new("in.pdf"), false, "-x").expect("resolves");
        assert_eq!(path, PathBuf::from("out.pdf"));
    }

    #[test]
    fn a_trailing_separator_means_a_directory_even_for_one_input() {
        let out = std::env::temp_dir().join("ypdf-cli-test-dir");
        let with_slash = format!("{}/", out.display());
        let path =
            output_for(Path::new(&with_slash), Path::new("in.pdf"), false, "").expect("resolves");
        assert_eq!(
            path.file_name().map(|n| n.to_string_lossy().into_owned()),
            Some("in.pdf".to_string())
        );
        let _ = std::fs::remove_dir_all(&out);
    }
}
