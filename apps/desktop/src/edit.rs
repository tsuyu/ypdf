//! Page editing with undo (spec §3, §28).
//!
//! Undo is an **operation log replayed from the original file**, not a stack of
//! saved document states. A PDF state is megabytes; an operation is a few
//! bytes, and replaying twelve of them onto a freshly opened document takes
//! milliseconds. It also means undo can never desynchronize from what the file
//! actually says — there is only ever one source of truth, and it is on disk.
//!
//! The cost is that undo is unavailable once the document has been saved over
//! its own original, which is honest: at that point the earlier state is gone.

use std::path::{Path, PathBuf};

use ypdf_core::{Error, Result};
use ypdf_doc::Pdf;

/// One editing operation, recorded so it can be replayed.
///
/// Page numbers are 1-based, matching what the user selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Remove pages.
    Delete(Vec<u32>),
    /// Duplicate pages in place.
    Duplicate(Vec<u32>),
    /// Rotate pages by quarter turns clockwise.
    Rotate(Vec<u32>, i32),
    /// Move one page to another position.
    Move { from: u32, to: u32 },
    /// Reverse the whole document.
    Reverse,
    /// Keep only these pages, in this order.
    Extract(Vec<u32>),
    /// Append another document's pages at a position.
    Insert { path: PathBuf, at: u32 },
    /// Replace the outline.
    ///
    /// An outline edit is recorded like a page edit so it survives a replay:
    /// otherwise adding a bookmark and then rotating a page would quietly drop
    /// the bookmark, because every replay starts from the original file.
    SetOutline(Vec<ypdf_outline::Bookmark>),
}

impl Op {
    /// A short description for the undo tooltip and the log.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Delete(pages) => format!("Delete {}", count(pages)),
            Self::Duplicate(pages) => format!("Duplicate {}", count(pages)),
            Self::Rotate(pages, turns) => {
                format!("Rotate {} by {}°", count(pages), turns.rem_euclid(4) * 90)
            }
            Self::Move { from, to } => format!("Move page {from} to {to}"),
            Self::Reverse => "Reverse pages".to_string(),
            Self::Extract(pages) => format!("Keep {}", count(pages)),
            Self::Insert { path, at } => {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                format!("Insert {name} at page {at}")
            }
            Self::SetOutline(tree) => match tree.len() {
                0 => "Remove the outline".to_string(),
                n => format!("Set {n} bookmark(s)"),
            },
        }
    }

    fn apply(&self, pdf: &mut Pdf) -> Result<()> {
        match self {
            Self::Delete(pages) => pdf.delete(pages),
            Self::Duplicate(pages) => pdf.duplicate(pages),
            Self::Rotate(pages, turns) => pdf.rotate(pages, *turns),
            Self::Move { from, to } => pdf.move_page(*from, *to),
            Self::Reverse => {
                pdf.reverse();
                Ok(())
            }
            Self::Extract(pages) => pdf.extract(pages),
            Self::Insert { path, at } => {
                let other = Pdf::open(path)?;
                pdf.insert_from(&other, *at)
            }
            Self::SetOutline(tree) => ypdf_outline::bookmarks::write(pdf, tree).map(|_| ()),
        }
    }
}

fn count(pages: &[u32]) -> String {
    match pages.len() {
        1 => format!("page {}", pages[0]),
        n => format!("{n} pages"),
    }
}

/// An editable document: the file it came from, plus the edits made to it.
pub struct EditSession {
    source: PathBuf,
    /// Password the source needs, when it is protected.
    ///
    /// Every replay re-opens the original file, so a protected document needs
    /// its password kept for as long as it is open. It lives only here, in
    /// memory, and is never written anywhere.
    password: Option<String>,
    ops: Vec<Op>,
    /// True once an edit has been made and not yet saved.
    dirty: bool,
}

impl EditSession {
    /// Start editing the document at `path`.
    #[must_use]
    pub fn new(source: PathBuf) -> Self {
        Self {
            source,
            password: None,
            ops: Vec::new(),
            dirty: false,
        }
    }

    /// Remember the password the source file needs.
    pub fn set_password(&mut self, password: impl Into<String>) {
        self.password = Some(password.into());
    }

    /// The password, if the source needs one.
    #[must_use]
    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    /// The file the log replays onto.
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Are there unsaved edits?
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Number of operations in the log.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Is anything undoable?
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.ops.is_empty()
    }

    /// What undoing would reverse.
    #[must_use]
    pub fn undo_description(&self) -> Option<String> {
        self.ops.last().map(Op::describe)
    }

    /// Record and apply an operation, returning the edited document.
    ///
    /// The operation is validated by actually performing it: if it fails, the
    /// log is left exactly as it was, so a rejected edit cannot poison every
    /// later replay.
    pub fn apply(&mut self, op: Op) -> Result<Pdf> {
        self.ops.push(op);
        match self.replay() {
            Ok(pdf) => {
                self.dirty = true;
                Ok(pdf)
            }
            Err(e) => {
                self.ops.pop();
                Err(e)
            }
        }
    }

    /// Undo the most recent operation, returning the document as it now stands.
    pub fn undo(&mut self) -> Result<Option<Pdf>> {
        if self.ops.pop().is_none() {
            return Ok(None);
        }
        let pdf = self.replay()?;
        self.dirty = !self.ops.is_empty();
        Ok(Some(pdf))
    }

    /// Rebuild the current document by replaying every operation from the
    /// original file.
    pub fn replay(&self) -> Result<Pdf> {
        let mut pdf = match &self.password {
            Some(password) => Pdf::open_with_password(&self.source, password)?,
            None => Pdf::open(&self.source)?,
        };
        for (index, op) in self.ops.iter().enumerate() {
            op.apply(&mut pdf).inspect_err(|_| {
                tracing::warn!(step = index, op = %op.describe(), "replay failed");
            })?;
        }
        Ok(pdf)
    }

    /// Write the edited document to `path`.
    ///
    /// Saving over the source file clears the log: the original those
    /// operations replayed onto no longer exists, so keeping them would let
    /// undo replay them a second time onto the already-edited file.
    pub fn save_as(&mut self, path: &Path) -> Result<()> {
        let mut pdf = self.replay()?;
        pdf.save(path)?;

        if same_file(path, &self.source) {
            self.ops.clear();
        } else {
            self.source = path.to_path_buf();
            self.ops.clear();
        }
        self.dirty = false;
        Ok(())
    }
}

/// Compare paths, falling back to a textual comparison when either cannot be
/// canonicalized — a file that does not exist yet cannot be the same file.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

impl std::fmt::Debug for EditSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditSession")
            .field("source", &self.source)
            .field("ops", &self.ops.len())
            .field("dirty", &self.dirty)
            .finish()
    }
}

/// A failure that leaves the document untouched, for the status bar.
#[must_use]
pub fn describe_failure(op: &Op, error: &Error) -> String {
    format!("{} failed: {}", op.describe(), error.message())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    fn session() -> EditSession {
        EditSession::new(fixture("many-pages.pdf"))
    }

    #[test]
    fn operations_describe_themselves_for_the_undo_tooltip() {
        assert_eq!(Op::Delete(vec![3]).describe(), "Delete page 3");
        assert_eq!(Op::Delete(vec![3, 4]).describe(), "Delete 2 pages");
        assert_eq!(Op::Rotate(vec![1], 1).describe(), "Rotate page 1 by 90°");
        assert_eq!(Op::Reverse.describe(), "Reverse pages");
    }

    #[test]
    fn a_fresh_session_is_clean_and_has_nothing_to_undo() {
        let session = session();
        assert!(!session.is_dirty());
        assert!(!session.can_undo());
        assert!(session.undo_description().is_none());
        assert_eq!(session.replay().expect("replays").page_count(), 12);
    }

    #[test]
    fn applying_an_operation_edits_the_document_and_marks_it_dirty() {
        let mut session = session();
        let pdf = session.apply(Op::Delete(vec![1, 2])).expect("deletes");
        assert_eq!(pdf.page_count(), 10);
        assert!(session.is_dirty());
        assert!(session.can_undo());
    }

    #[test]
    fn undo_replays_the_log_from_the_original_file() {
        let mut session = session();
        session.apply(Op::Delete(vec![1])).expect("delete");
        session.apply(Op::Delete(vec![1])).expect("delete again");
        assert_eq!(session.replay().expect("replays").page_count(), 10);

        let pdf = session.undo().expect("undoes").expect("a document");
        assert_eq!(pdf.page_count(), 11);

        let pdf = session.undo().expect("undoes").expect("a document");
        assert_eq!(pdf.page_count(), 12);
        assert!(
            !session.is_dirty(),
            "back at the original, nothing is unsaved"
        );
        assert!(
            session.undo().expect("undoes").is_none(),
            "nothing left to undo"
        );
    }

    #[test]
    fn a_rejected_operation_leaves_the_log_untouched() {
        let mut session = session();
        session.apply(Op::Delete(vec![1])).expect("delete");

        let err = session
            .apply(Op::Delete(vec![99]))
            .expect_err("page 99 does not exist");
        assert_eq!(err.code(), "E_PAGE_RANGE");
        assert_eq!(
            session.len(),
            1,
            "the failed operation must not be recorded"
        );
        assert_eq!(session.replay().expect("replays").page_count(), 11);
    }

    #[test]
    fn operations_compose_in_order() {
        let mut session = session();
        session.apply(Op::Delete(vec![1])).expect("delete");
        session.apply(Op::Duplicate(vec![1])).expect("duplicate");
        session.apply(Op::Reverse).expect("reverse");
        assert_eq!(session.replay().expect("replays").page_count(), 12);
        assert_eq!(session.len(), 3);
    }

    #[test]
    fn saving_elsewhere_clears_the_log_and_follows_the_new_file() {
        let dir = std::env::temp_dir().join("ypdf-edit-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let target = dir.join("saved.pdf");

        let mut session = session();
        session.apply(Op::Delete(vec![1, 2])).expect("delete");
        session.save_as(&target).expect("saves");

        assert!(!session.is_dirty());
        assert!(!session.can_undo(), "the saved file is the new baseline");
        assert_eq!(session.source(), target);
        assert_eq!(
            session.replay().expect("replays").page_count(),
            10,
            "the edit is in the saved file"
        );

        std::fs::remove_file(&target).ok();
    }
}
