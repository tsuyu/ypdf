//! Page-level document operations (spec §3).
//!
//! Every operation goes through one invariant: after it, the document has a
//! **single-level page tree** whose `/Kids` array *is* the page order. PDF
//! allows an arbitrarily nested tree with attributes inherited down it, and
//! reordering pages inside such a tree while preserving inheritance is where
//! page editors quietly corrupt files. So the tree is flattened once, on load,
//! and inherited attributes are copied onto each page before that happens.
//!
//! Operations are in-place and cheap: they permute a list of object ids. The
//! objects themselves are never rewritten.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lopdf::{Dictionary, Document, LoadOptions, Object, ObjectId, dictionary};
use ypdf_core::{Error, ParseFailure, Result};

/// Attributes a page may inherit from an ancestor `/Pages` node.
///
/// Flattening the tree destroys the ancestors, so these are copied down first.
/// Losing `/MediaBox` in particular leaves a page with no size at all.
const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// An open PDF, ready to be edited.
pub struct Pdf {
    inner: Document,
    /// Page object ids in page order. The single source of truth for order.
    pages: Vec<ObjectId>,
    /// Where it was loaded from, if anywhere.
    path: Option<PathBuf>,
    /// The protection the file carried when it was opened, if any.
    encryption: Option<Encryption>,
}

/// The encryption a document carried when it was loaded.
///
/// Held here rather than in the trailer. Once the objects have been decrypted
/// in memory, a trailer still pointing at `/Encrypt` describes a file that no
/// longer exists, and saving it would hand a reader plaintext objects it would
/// try to decrypt. Keeping the state to one side means the protection can still
/// be *reported* without the document lying about what it is.
///
/// Interpreting it — which cipher, which restrictions — belongs to
/// `ypdf-crypt`; this crate only remembers it.
pub type Encryption = lopdf::EncryptionState;

impl Pdf {
    /// Load a document from disk.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let inner = Document::load(path).map_err(|e| from_lopdf(&e, Some(path)))?;
        Self::finish(inner, Some(path.to_path_buf()))
    }

    /// Load a document from memory.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let inner = Document::load_mem(bytes).map_err(|e| from_lopdf(&e, None))?;
        Self::finish(inner, None)
    }

    /// Load an encrypted document from disk.
    ///
    /// Either password opens it: the user password gives the document, the
    /// owner password gives the document and the right to change its
    /// protection.
    pub fn open_with_password(path: impl AsRef<Path>, password: &str) -> Result<Self> {
        let path = path.as_ref();
        let inner = Document::load_with_options(path, LoadOptions::with_password(password))
            .map_err(|e| from_lopdf(&e, Some(path)))?;
        Self::finish(inner, Some(path.to_path_buf()))
    }

    /// Load an encrypted document from memory.
    pub fn from_encrypted_bytes(bytes: &[u8], password: &str) -> Result<Self> {
        let inner = Document::load_mem_with_options(bytes, LoadOptions::with_password(password))
            .map_err(|e| from_lopdf(&e, None))?;
        Self::finish(inner, None)
    }

    /// Shared tail of every loader.
    ///
    /// A document that is still encrypted at this point is one no password
    /// unlocked; reading its pages would produce ciphertext dressed up as
    /// content, so it is reported as needing a password instead.
    fn finish(mut inner: Document, path: Option<PathBuf>) -> Result<Self> {
        // Still encrypted here means no password unlocked it: the objects are
        // ciphertext, and reading pages out of them would produce nonsense
        // dressed as content.
        if inner.is_encrypted() && !inner.was_encrypted() {
            let error = Error::PasswordRequired;
            return Err(match path {
                Some(path) => error.with_path(path),
                None => error,
            });
        }

        let encryption = inner.encryption_state.clone();
        // Whatever survived of the old protection must not reach the writer.
        inner.trailer.remove(b"Encrypt");
        inner.encryption_state = None;

        let mut pdf = Self {
            inner,
            pages: Vec::new(),
            path,
            encryption,
        };
        pdf.normalize()?;
        Ok(pdf)
    }

    /// The protection this document carried when it was opened.
    ///
    /// `None` for a file that was not encrypted. Note that saving **never**
    /// carries protection over: the objects were decrypted to be worked on, and
    /// re-protecting them is a deliberate act, not a side effect of saving.
    #[must_use]
    pub const fn encryption(&self) -> Option<&Encryption> {
        self.encryption.as_ref()
    }

    /// Was this document encrypted when it was opened?
    #[must_use]
    pub const fn was_encrypted(&self) -> bool {
        self.encryption.is_some()
    }

    /// Write the document out.
    pub fn save(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        self.commit()?;
        self.inner
            .save(path)
            .map(|_| ())
            .map_err(|e| Error::io(path, e))
    }

    /// Serialize the document to memory.
    pub fn to_bytes(&mut self) -> Result<Vec<u8>> {
        self.commit()?;
        let mut buffer = Vec::new();
        self.inner.save_to(&mut buffer).map_err(|e| Error::Io {
            path: None,
            source: e,
        })?;
        Ok(buffer)
    }

    /// The underlying lopdf document.
    ///
    /// Exposed so inspection crates (`ypdf-security`, diagnostics) can walk the
    /// object graph without this crate having to proxy every accessor. Editing
    /// through it bypasses the flattened page list, so treat it as read-only
    /// unless you are inside `ypdf-doc`.
    #[must_use]
    pub fn raw(&self) -> &lopdf::Document {
        &self.inner
    }

    /// Mutable access to the underlying document.
    ///
    /// For crates that rewrite objects in place — `ypdf-optimize` recompressing
    /// images, metadata edits here. Adding or removing **pages** through this
    /// bypasses the flattened page list and will be undone on the next save;
    /// use the page operations for that.
    pub fn raw_mut(&mut self) -> &mut lopdf::Document {
        &mut self.inner
    }

    /// Page object ids, in page order.
    #[must_use]
    pub fn page_ids(&self) -> &[ObjectId] {
        &self.pages
    }

    /// Where this document came from, if it was loaded from a file.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Number of pages.
    #[must_use]
    pub fn page_count(&self) -> u32 {
        u32::try_from(self.pages.len()).unwrap_or(u32::MAX)
    }

    /// Is the document empty?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    // --- page operations (spec §3.3) ------------------------------------

    /// Keep only the given pages, in the order given (spec §3.3, extract).
    ///
    /// Repeats duplicate a page, so `extract(&[1, 1, 2])` is a legitimate way
    /// to produce a three-page document.
    pub fn extract(&mut self, pages: &[u32]) -> Result<()> {
        let ids = self.ids_for(pages)?;
        self.pages = ids;
        Ok(())
    }

    /// Remove the given pages.
    ///
    /// Refuses to delete every page: a PDF with no pages is not a document,
    /// and silently producing one turns a mistake into a corrupt file.
    pub fn delete(&mut self, pages: &[u32]) -> Result<()> {
        let doomed = self.indices_for(pages)?;
        if doomed.len() == self.pages.len() && !self.pages.is_empty() {
            return Err(Error::InvalidPageRange {
                spec: "every page: a document must keep at least one page".into(),
            });
        }
        let doomed: BTreeSet<usize> = doomed.into_iter().collect();
        let mut index = 0;
        self.pages.retain(|_| {
            let keep = !doomed.contains(&index);
            index += 1;
            keep
        });
        Ok(())
    }

    /// Duplicate the given pages, each copy placed directly after its original.
    pub fn duplicate(&mut self, pages: &[u32]) -> Result<()> {
        let targets: BTreeSet<usize> = self.indices_for(pages)?.into_iter().collect();
        let mut next = Vec::with_capacity(self.pages.len() + targets.len());
        for (index, id) in self.pages.iter().enumerate() {
            next.push(*id);
            if targets.contains(&index) {
                // The same object id twice: PDF permits a page object to appear
                // in /Kids more than once, and nothing needs to be copied.
                next.push(*id);
            }
        }
        self.pages = next;
        Ok(())
    }

    /// Reorder the whole document. `order` must be a permutation of every page.
    pub fn reorder(&mut self, order: &[u32]) -> Result<()> {
        let mut seen: Vec<u32> = order.to_vec();
        seen.sort_unstable();
        seen.dedup();
        if seen.len() != self.pages.len() || order.len() != self.pages.len() {
            return Err(Error::InvalidPageRange {
                spec: format!(
                    "a reordering must list each of the {} pages exactly once",
                    self.pages.len()
                ),
            });
        }
        self.pages = self.ids_for(order)?;
        Ok(())
    }

    /// Move one page to a new position, both 1-based.
    ///
    /// This is what a drag in the thumbnail sidebar means.
    pub fn move_page(&mut self, from: u32, to: u32) -> Result<()> {
        let from = self.index_of(from)?;
        let to = self.index_of(to)?;
        let id = self.pages.remove(from);
        self.pages.insert(to, id);
        Ok(())
    }

    /// Reverse the page order.
    pub fn reverse(&mut self) {
        self.pages.reverse();
    }

    /// Rotate the given pages by a number of quarter turns clockwise.
    ///
    /// Rotation is relative and accumulates with any rotation the page already
    /// carries, which is what "rotate this page" means to a user.
    pub fn rotate(&mut self, pages: &[u32], quarter_turns: i32) -> Result<()> {
        let ids = self.ids_for(pages)?;
        let delta = quarter_turns.rem_euclid(4) * 90;
        for id in ids {
            let current = self
                .inner
                .get_dictionary(id)
                .ok()
                .and_then(|d| d.get(b"Rotate").ok())
                .and_then(|o| o.as_i64().ok())
                .unwrap_or(0);
            let rotated = (i64::from(delta) + current).rem_euclid(360);
            let dict = self
                .inner
                .get_object_mut(id)
                .and_then(Object::as_dict_mut)
                .map_err(|e| from_lopdf(&e, None))?;
            dict.set("Rotate", rotated);
        }
        Ok(())
    }

    /// Insert every page of `other` at a 1-based position.
    ///
    /// `at` may be `page_count() + 1`, meaning "append".
    pub fn insert_from(&mut self, other: &Self, at: u32) -> Result<()> {
        let at = usize::try_from(at.saturating_sub(1))
            .unwrap_or(usize::MAX)
            .min(self.pages.len());
        let imported = self.import(other);
        self.pages.splice(at..at, imported);
        Ok(())
    }

    /// Replace a run of pages with every page of `other`.
    pub fn replace(&mut self, pages: &[u32], other: &Self) -> Result<()> {
        let doomed = self.indices_for(pages)?;
        let at = doomed.iter().copied().min().unwrap_or(0);
        let doomed: BTreeSet<usize> = doomed.into_iter().collect();

        let mut index = 0;
        self.pages.retain(|_| {
            let keep = !doomed.contains(&index);
            index += 1;
            keep
        });

        let at = at.min(self.pages.len());
        let imported = self.import(other);
        self.pages.splice(at..at, imported);
        Ok(())
    }

    /// Append every page of each document, in order (spec §3.1).
    pub fn merge(documents: &[Self]) -> Result<Self> {
        let mut iter = documents.iter();
        let Some(first) = iter.next() else {
            return Err(Error::InvalidPageRange {
                spec: "merge needs at least one document".into(),
            });
        };

        let mut merged = first.clone_document()?;
        for other in iter {
            let at = merged.page_count() + 1;
            merged.insert_from(other, at)?;
        }
        Ok(merged)
    }

    /// A copy of this document holding only the given pages.
    pub fn extracted(&self, pages: &[u32]) -> Result<Self> {
        let mut copy = self.clone_document()?;
        copy.extract(pages)?;
        Ok(copy)
    }

    // --- internals -------------------------------------------------------

    /// Copy every object of `other` into this document under fresh ids, and
    /// return its page ids as they now exist here.
    fn import(&mut self, other: &Self) -> Vec<ObjectId> {
        // Renumbering `other` onto ids above ours is what keeps the two object
        // spaces from colliding; it rewrites references inside `other` too.
        let mut source = other.inner.clone();
        let offset = self.inner.max_id;
        source.renumber_objects_with(offset + 1);

        let shift = |id: ObjectId| (id.0 + offset, id.1);
        let pages: Vec<ObjectId> = other.pages.iter().map(|id| shift(*id)).collect();

        self.inner.max_id = source.max_id.max(self.inner.max_id);
        self.inner.objects.extend(source.objects);
        pages
    }

    /// Deep copy, keeping the flattened page list.
    fn clone_document(&self) -> Result<Self> {
        Ok(Self {
            inner: self.inner.clone(),
            pages: self.pages.clone(),
            path: self.path.clone(),
            encryption: self.encryption.clone(),
        })
    }

    /// Flatten the page tree and record the page order.
    ///
    /// Called once on load. Everything after it works on [`Self::pages`].
    fn normalize(&mut self) -> Result<()> {
        let pages = self.inner.get_pages();
        if pages.is_empty() {
            return Err(Error::parse(ParseFailure::Other {
                detail: "The document has no pages.".into(),
            }));
        }

        self.pages = pages.values().copied().collect();
        self.materialize_inherited();
        Ok(())
    }

    /// Copy inherited attributes from ancestors onto each page.
    fn materialize_inherited(&mut self) {
        for page_id in self.pages.clone() {
            for key in INHERITABLE {
                if self.inner.get_dictionary(page_id).is_ok_and(|d| d.has(key)) {
                    continue;
                }
                let Some(value) = self.inherited(page_id, key) else {
                    continue;
                };
                if let Ok(dict) = self
                    .inner
                    .get_object_mut(page_id)
                    .and_then(Object::as_dict_mut)
                {
                    dict.set(key.to_vec(), value);
                }
            }
        }
    }

    /// Walk `/Parent` looking for an attribute.
    fn inherited(&self, page_id: ObjectId, key: &[u8]) -> Option<Object> {
        let mut current = page_id;
        // Bounded: a malformed file can point /Parent at itself, and this must
        // not become an infinite loop.
        for _ in 0..32 {
            let dict = self.inner.get_dictionary(current).ok()?;
            if let Ok(value) = dict.get(key) {
                return Some(value.clone());
            }
            current = dict.get(b"Parent").ok()?.as_reference().ok()?;
        }
        None
    }

    /// Write [`Self::pages`] back into the document as a single-level tree.
    fn commit(&mut self) -> Result<()> {
        let root_id = self.pages_root()?;

        let kids: Vec<Object> = self.pages.iter().map(|id| Object::Reference(*id)).collect();
        let count = i64::try_from(self.pages.len()).unwrap_or(i64::MAX);

        for page_id in self.pages.clone() {
            if let Ok(dict) = self
                .inner
                .get_object_mut(page_id)
                .and_then(Object::as_dict_mut)
            {
                dict.set("Parent", Object::Reference(root_id));
            }
        }

        let root = self
            .inner
            .get_object_mut(root_id)
            .and_then(Object::as_dict_mut)
            .map_err(|e| from_lopdf(&e, None))?;
        root.set("Type", Object::Name(b"Pages".to_vec()));
        root.set("Kids", Object::Array(kids));
        root.set("Count", count);
        // Attributes now live on the pages themselves; leaving stale inherited
        // copies on the root would silently override a page that has none.
        for key in INHERITABLE {
            root.remove(key);
        }

        // Intermediate nodes and pages dropped by an edit are now unreachable.
        self.inner.prune_objects();
        Ok(())
    }

    fn pages_root(&mut self) -> Result<ObjectId> {
        if let Ok(catalog) = self.inner.catalog()
            && let Ok(reference) = catalog.get(b"Pages").and_then(Object::as_reference)
        {
            return Ok(reference);
        }

        // No usable catalog: build one rather than refusing to save an
        // otherwise recoverable document.
        let root_id = self.inner.add_object(Dictionary::new());
        let catalog_id = self.inner.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(root_id),
        });
        self.inner
            .trailer
            .set("Root", Object::Reference(catalog_id));
        Ok(root_id)
    }

    /// 1-based page numbers to positions in [`Self::pages`].
    fn indices_for(&self, pages: &[u32]) -> Result<Vec<usize>> {
        pages.iter().map(|page| self.index_of(*page)).collect()
    }

    /// 1-based page numbers to object ids, in the order given.
    fn ids_for(&self, pages: &[u32]) -> Result<Vec<ObjectId>> {
        self.indices_for(pages)?
            .into_iter()
            .map(|i| Ok(self.pages[i]))
            .collect()
    }

    fn index_of(&self, page: u32) -> Result<usize> {
        let count = self.page_count();
        if page == 0 || page > count {
            return Err(Error::PageOutOfRange {
                requested: page,
                pages: count,
            });
        }
        Ok(usize::try_from(page - 1).unwrap_or(0))
    }
}

impl std::fmt::Debug for Pdf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pdf")
            .field("path", &self.path)
            .field("pages", &self.pages.len())
            .field("objects", &self.inner.objects.len())
            .finish()
    }
}

/// Map a lopdf failure onto the engine's error vocabulary.
pub(crate) fn from_lopdf(e: &lopdf::Error, path: Option<&Path>) -> Error {
    use lopdf::Error as L;

    let error = match e {
        L::IO(io) => Error::Io {
            path: None,
            source: std::io::Error::new(io.kind(), io.to_string()),
        },
        L::Xref(_) | L::MissingXrefEntry => Error::parse(ParseFailure::InvalidXref),
        L::ObjectNotFound(id) => Error::parse(ParseFailure::MissingObject {
            object: id.0,
            generation: id.1,
        }),
        L::Decompress(_) | L::InvalidStream(_) | L::InvalidObjectStream(_) => {
            Error::parse(ParseFailure::CorruptStream { object: 0 })
        }
        L::InvalidPassword => Error::WrongPassword,
        L::Unimplemented(what) => Error::Unsupported {
            feature: (*what).to_string(),
        },
        other => Error::parse(ParseFailure::Other {
            detail: other.to_string(),
        }),
    };

    match path {
        Some(path) => error.with_path(path),
        None => error,
    }
}

impl Pdf {
    /// The 1-based page numbers that top-level bookmarks point at (spec §3.2).
    ///
    /// A document with no outline yields nothing rather than an error: "split
    /// by bookmarks" on an unbookmarked file should produce one piece, not a
    /// failure.
    pub fn bookmark_pages(&self) -> Result<Vec<u32>> {
        let Ok(catalog) = self.inner.catalog() else {
            return Ok(Vec::new());
        };
        let Ok(outlines) = catalog.get(b"Outlines").and_then(Object::as_reference) else {
            return Ok(Vec::new());
        };
        let Ok(outlines) = self.inner.get_dictionary(outlines) else {
            return Ok(Vec::new());
        };

        let mut pages = Vec::new();
        let mut current = outlines
            .get(b"First")
            .ok()
            .and_then(|o| o.as_reference().ok());

        // Bounded so a malformed /Next cycle cannot hang the process.
        for _ in 0..4096 {
            let Some(id) = current else { break };
            let Ok(item) = self.inner.get_dictionary(id) else {
                break;
            };

            if let Some(page_id) = self.destination_page(item)
                && let Some(index) = self.pages.iter().position(|p| *p == page_id)
            {
                pages.push(u32::try_from(index + 1).unwrap_or(u32::MAX));
            }

            current = item.get(b"Next").ok().and_then(|o| o.as_reference().ok());
        }

        Ok(pages)
    }

    /// The page an outline item points at, via `/Dest` or a `/GoTo` action.
    fn destination_page(&self, item: &Dictionary) -> Option<ObjectId> {
        let destination = match item.get(b"Dest") {
            Ok(dest) => dest.clone(),
            Err(_) => {
                let action = item.get(b"A").ok()?;
                let action = self.resolve(action)?;
                action.as_dict().ok()?.get(b"D").ok()?.clone()
            }
        };

        let destination = self.resolve(&destination)?;
        // A destination array starts with the page reference; a named
        // destination would need the name tree, which is not followed here.
        destination.as_array().ok()?.first()?.as_reference().ok()
    }

    fn resolve(&self, object: &Object) -> Option<Object> {
        match object {
            Object::Reference(id) => self.inner.get_object(*id).ok().cloned(),
            other => Some(other.clone()),
        }
    }
}
