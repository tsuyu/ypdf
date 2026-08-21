//! The recent-files list (spec §28).
//!
//! Stored beside the user's configuration as plain JSON. It is a convenience,
//! never a dependency: every failure here is logged and swallowed, because a
//! corrupt list must not stop the application from opening.
//!
//! Paths are user data and stay local — this file is the only place they are
//! written, and nothing uploads it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How many entries to keep.
const MAX_ENTRIES: usize = 12;

/// The most recently opened files, newest first.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Recent {
    #[serde(default)]
    paths: Vec<PathBuf>,
}

impl Recent {
    /// Read the list, or an empty one if it is missing or unreadable.
    #[must_use]
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), "recent files list is unreadable: {e}");
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::warn!(path = %path.display(), "could not read recent files: {e}");
                Self::default()
            }
        }
    }

    /// Record a file as most recently opened and save.
    pub fn push(&mut self, path: &Path) {
        self.remember(path);
        self.save();
    }

    /// The list update on its own, without touching the disk.
    fn remember(&mut self, path: &Path) {
        self.paths.retain(|p| p != path);
        self.paths.insert(0, path.to_path_buf());
        self.paths.truncate(MAX_ENTRIES);
    }

    /// Forget every entry that no longer exists on disk.
    pub fn prune(&mut self) {
        let before = self.paths.len();
        self.paths.retain(|p| p.exists());
        if self.paths.len() != before {
            self.save();
        }
    }

    /// The entries, newest first.
    #[must_use]
    pub fn entries(&self) -> &[PathBuf] {
        &self.paths
    }

    fn save(&self) {
        let Some(path) = Self::path() else { return };
        let Some(parent) = path.parent() else { return };
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(path = %parent.display(), "could not create config directory: {e}");
            return;
        }
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    tracing::warn!(path = %path.display(), "could not save recent files: {e}");
                }
            }
            Err(e) => tracing::warn!("could not serialize recent files: {e}"),
        }
    }

    fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("dev", "ypdf", "ypdf")
            .map(|d| d.config_dir().join("recent.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `remember` rather than `push`: these tests must not write to the real
    // user profile.

    #[test]
    fn most_recent_comes_first_and_repeats_do_not_duplicate() {
        let mut recent = Recent::default();
        recent.remember(Path::new("a.pdf"));
        recent.remember(Path::new("b.pdf"));
        recent.remember(Path::new("a.pdf"));

        assert_eq!(
            recent.entries(),
            [PathBuf::from("a.pdf"), PathBuf::from("b.pdf")]
        );
    }

    #[test]
    fn the_list_is_bounded() {
        let mut recent = Recent::default();
        for i in 0..100 {
            recent.remember(Path::new(&format!("{i}.pdf")));
        }
        assert_eq!(recent.entries().len(), MAX_ENTRIES);
        // And keeps the newest.
        assert_eq!(recent.entries()[0], PathBuf::from("99.pdf"));
    }
}
