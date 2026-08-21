//! Bookmarks: the outline tree (spec §17).
//!
//! Reading is a walk down `/First` and along `/Next`, both of which are just
//! pointers a producer can get wrong; every walk here is depth- and
//! count-limited so a cycle in someone's file cannot hang the process.
//!
//! Writing replaces the whole tree. Editing the linked list in place would mean
//! repairing `/Prev`, `/Next`, `/First`, `/Last`, `/Parent`, and `/Count` on
//! every touched node, and a mistake in any of them produces an outline that
//! looks fine in one reader and is empty in another. Rebuilding is simpler and
//! either works or does not.

use lopdf::{Dictionary, Object, ObjectId, dictionary};
use serde::{Deserialize, Serialize};
use ypdf_core::Result;
use ypdf_doc::Pdf;

/// How deep an outline may nest before this stops following it.
const MAX_DEPTH: usize = 32;

/// How many siblings it will follow at one level.
const MAX_SIBLINGS: usize = 8192;

/// One bookmark, and anything nested under it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    /// What it says.
    pub title: String,
    /// The 1-based page it points at.
    ///
    /// `None` for an item that points nowhere this crate could resolve — a
    /// remote destination, or a named one that is missing. Kept rather than
    /// dropped: a heading with no target is still part of the structure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    /// Whether it is drawn expanded.
    #[serde(default)]
    pub open: bool,
    /// Nested bookmarks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Bookmark>,
}

impl Bookmark {
    /// A bookmark pointing at a page.
    #[must_use]
    pub fn new(title: impl Into<String>, page: u32) -> Self {
        Self {
            title: title.into(),
            page: Some(page),
            open: false,
            children: Vec::new(),
        }
    }

    /// This bookmark and everything under it.
    #[must_use]
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(Self::count).sum::<usize>()
    }
}

/// Read the outline tree.
#[must_use]
pub fn read(pdf: &Pdf) -> Vec<Bookmark> {
    let document = pdf.raw();
    let Ok(catalog) = document.catalog() else {
        return Vec::new();
    };
    let Some(outlines) = catalog
        .get(b"Outlines")
        .ok()
        .and_then(|value| resolve_dict(pdf, value))
    else {
        return Vec::new();
    };

    let first = outlines
        .get(b"First")
        .ok()
        .and_then(|value| value.as_reference().ok());
    read_siblings(pdf, first, 0)
}

fn read_siblings(pdf: &Pdf, first: Option<ObjectId>, depth: usize) -> Vec<Bookmark> {
    if depth >= MAX_DEPTH {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut current = first;
    let mut seen: Vec<ObjectId> = Vec::new();

    for _ in 0..MAX_SIBLINGS {
        let Some(id) = current else { break };
        // A `/Next` that points back at an earlier item is a cycle; following
        // it would fill memory with the same three bookmarks.
        if seen.contains(&id) {
            break;
        }
        seen.push(id);

        let Ok(item) = pdf.raw().get_dictionary(id) else {
            break;
        };

        let title = item
            .get(b"Title")
            .ok()
            .and_then(|value| match value {
                Object::String(bytes, _) => Some(ypdf_doc::decode_text(bytes)),
                _ => None,
            })
            .unwrap_or_default();

        let children = read_siblings(
            pdf,
            item.get(b"First")
                .ok()
                .and_then(|value| value.as_reference().ok()),
            depth + 1,
        );

        out.push(Bookmark {
            title,
            page: page_of(pdf, item),
            // A positive /Count means the item is drawn expanded.
            open: item
                .get(b"Count")
                .and_then(Object::as_i64)
                .is_ok_and(|count| count > 0),
            children,
        });

        current = item
            .get(b"Next")
            .ok()
            .and_then(|value| value.as_reference().ok());
    }

    out
}

/// The 1-based page an item points at.
fn page_of(pdf: &Pdf, item: &Dictionary) -> Option<u32> {
    let destination = match item.get(b"Dest") {
        Ok(dest) => dest.clone(),
        Err(_) => {
            // A `/GoTo` action is the other way to say the same thing.
            let action = item
                .get(b"A")
                .ok()
                .and_then(|value| resolve_dict(pdf, value))?;
            action.get(b"D").ok()?.clone()
        }
    };

    let array = match destination {
        Object::Array(items) => items,
        Object::Reference(id) => pdf.raw().get_object(id).ok()?.as_array().ok()?.clone(),
        // A named destination needs the name tree, which this does not read.
        _ => return None,
    };

    let page_id = array.first()?.as_reference().ok()?;
    let index = pdf.page_ids().iter().position(|id| *id == page_id)?;
    u32::try_from(index + 1).ok()
}

fn resolve_dict(pdf: &Pdf, value: &Object) -> Option<Dictionary> {
    match value {
        Object::Reference(id) => pdf.raw().get_dictionary(*id).ok().cloned(),
        Object::Dictionary(dict) => Some(dict.clone()),
        _ => None,
    }
}

/// Replace the outline with `tree`.
///
/// Returns how many bookmarks were written. An empty tree removes the outline
/// entirely rather than leaving an empty one, which some readers show as a
/// blank panel.
pub fn write(pdf: &mut Pdf, tree: &[Bookmark]) -> Result<usize> {
    let page_ids: Vec<ObjectId> = pdf.page_ids().to_vec();

    // The old outline objects are simply dropped: nothing points at them once
    // the catalog does not, and `commit` prunes what is unreachable.
    if tree.is_empty() {
        if let Ok(catalog) = pdf.raw_mut().catalog_mut() {
            catalog.remove(b"Outlines");
        }
        tracing::info!("outline removed");
        return Ok(0);
    }

    let outlines_id = pdf.raw_mut().new_object_id();
    let (first, last, count) = write_level(pdf, tree, outlines_id, &page_ids);

    let outlines = dictionary! {
        "Type" => "Outlines",
        "First" => first.map_or(Object::Null, Object::Reference),
        "Last" => last.map_or(Object::Null, Object::Reference),
        // At the root, /Count is the number of *visible* descendants.
        "Count" => i64::try_from(count).unwrap_or(i64::MAX),
    };
    pdf.raw_mut()
        .objects
        .insert(outlines_id, Object::Dictionary(outlines));

    if let Ok(catalog) = pdf.raw_mut().catalog_mut() {
        catalog.set("Outlines", Object::Reference(outlines_id));
    }

    let total = tree.iter().map(Bookmark::count).sum();
    tracing::info!(bookmarks = total, "outline written");
    Ok(total)
}

/// Write one level of siblings, returning its first, last, and visible count.
fn write_level(
    pdf: &mut Pdf,
    level: &[Bookmark],
    parent: ObjectId,
    page_ids: &[ObjectId],
) -> (Option<ObjectId>, Option<ObjectId>, usize) {
    if level.is_empty() {
        return (None, None, 0);
    }

    // Identifiers first, so each item can point at its neighbours.
    let ids: Vec<ObjectId> = level
        .iter()
        .map(|_| pdf.raw_mut().new_object_id())
        .collect();

    let mut visible = level.len();

    for (index, bookmark) in level.iter().enumerate() {
        let Some(id) = ids.get(index).copied() else {
            continue;
        };

        let (first, last, descendants) = write_level(pdf, &bookmark.children, id, page_ids);
        if bookmark.open {
            visible += descendants;
        }

        let mut item = dictionary! {
            "Title" => Object::String(
                ypdf_doc::encode_text(&bookmark.title),
                lopdf::StringFormat::Literal,
            ),
            "Parent" => Object::Reference(parent),
        };

        if index > 0
            && let Some(previous) = ids.get(index - 1)
        {
            item.set("Prev", Object::Reference(*previous));
        }
        if let Some(next) = ids.get(index + 1) {
            item.set("Next", Object::Reference(*next));
        }
        if let Some(first) = first {
            item.set("First", Object::Reference(first));
        }
        if let Some(last) = last {
            item.set("Last", Object::Reference(last));
        }
        if descendants > 0 {
            // Negative means collapsed, positive means expanded. The magnitude
            // is the number of descendants a reader would show.
            let count = i64::try_from(descendants).unwrap_or(i64::MAX);
            item.set("Count", if bookmark.open { count } else { -count });
        }

        if let Some(page) = bookmark.page
            && let Some(page_id) = page_ids.get((page.max(1) - 1) as usize)
        {
            // `/XYZ` with nulls means "this page, keep the current view", which
            // is what a bookmark to a page should do — jumping to a remembered
            // zoom level is disorienting.
            item.set(
                "Dest",
                Object::Array(vec![
                    Object::Reference(*page_id),
                    Object::Name(b"XYZ".to_vec()),
                    Object::Null,
                    Object::Null,
                    Object::Null,
                ]),
            );
        }

        pdf.raw_mut().objects.insert(id, Object::Dictionary(item));
    }

    (ids.first().copied(), ids.last().copied(), visible)
}

/// Flatten a tree into `(depth, bookmark)` pairs, for listing.
#[must_use]
pub fn flatten(tree: &[Bookmark]) -> Vec<(usize, &Bookmark)> {
    fn walk<'a>(level: &'a [Bookmark], depth: usize, out: &mut Vec<(usize, &'a Bookmark)>) {
        for bookmark in level {
            out.push((depth, bookmark));
            walk(&bookmark.children, depth + 1, out);
        }
    }

    let mut out = Vec::new();
    walk(tree, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    #[test]
    fn an_existing_outline_is_read_with_its_pages() {
        let pdf = Pdf::open(fixture("outlined.pdf")).expect("opens");
        let tree = read(&pdf);

        assert!(!tree.is_empty(), "the fixture has bookmarks");
        assert!(tree.iter().all(|bookmark| !bookmark.title.is_empty()));
        assert!(tree.iter().any(|bookmark| bookmark.page.is_some()));
    }

    #[test]
    fn a_document_with_no_outline_reads_as_empty_rather_than_failing() {
        let pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        assert!(read(&pdf).is_empty());
    }

    #[test]
    fn a_written_tree_reads_back_the_same() {
        let mut pdf = Pdf::open(fixture("many-pages.pdf")).expect("opens");
        let tree = vec![
            Bookmark {
                title: "Introduction".into(),
                page: Some(1),
                open: true,
                children: vec![Bookmark::new("Background", 2), Bookmark::new("Scope", 3)],
            },
            Bookmark::new("Results", 7),
        ];

        assert_eq!(write(&mut pdf, &tree).expect("writes"), 4);

        let bytes = pdf.to_bytes().expect("serializes");
        let reopened = Pdf::from_bytes(&bytes).expect("re-opens");
        assert_eq!(read(&reopened), tree);
    }

    #[test]
    fn a_non_latin_title_survives_the_round_trip() {
        // Titles are PDF text strings, so they are written as UTF-16 and are
        // not limited to what a base-14 font can draw.
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        let tree = vec![Bookmark::new("目次 — Übersicht", 1)];
        write(&mut pdf, &tree).expect("writes");

        let bytes = pdf.to_bytes().expect("serializes");
        let reopened = Pdf::from_bytes(&bytes).expect("re-opens");
        assert_eq!(read(&reopened)[0].title, "目次 — Übersicht");
    }

    #[test]
    fn writing_an_empty_tree_removes_the_outline() {
        let mut pdf = Pdf::open(fixture("outlined.pdf")).expect("opens");
        assert!(!read(&pdf).is_empty());

        assert_eq!(write(&mut pdf, &[]).expect("writes"), 0);

        let bytes = pdf.to_bytes().expect("serializes");
        let reopened = Pdf::from_bytes(&bytes).expect("re-opens");
        assert!(read(&reopened).is_empty());
        assert!(
            reopened
                .raw()
                .catalog()
                .is_ok_and(|catalog| catalog.get(b"Outlines").is_err()),
            "an empty outline shows as a blank panel in some readers"
        );
    }

    #[test]
    fn an_expanded_item_is_written_as_expanded() {
        let mut pdf = Pdf::open(fixture("many-pages.pdf")).expect("opens");
        let tree = vec![Bookmark {
            title: "Parent".into(),
            page: Some(1),
            open: true,
            children: vec![Bookmark::new("Child", 2)],
        }];
        write(&mut pdf, &tree).expect("writes");

        let bytes = pdf.to_bytes().expect("serializes");
        let reopened = Pdf::from_bytes(&bytes).expect("re-opens");
        assert!(read(&reopened)[0].open);
    }

    #[test]
    fn a_bookmark_pointing_past_the_end_is_written_without_a_destination() {
        // Rather than pointing at whatever page happens to be last, which would
        // look like it worked.
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        write(&mut pdf, &[Bookmark::new("Nowhere", 99)]).expect("writes");

        let bytes = pdf.to_bytes().expect("serializes");
        let reopened = Pdf::from_bytes(&bytes).expect("re-opens");
        let tree = read(&reopened);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].page, None);
    }

    #[test]
    fn flattening_keeps_reading_order_and_depth() {
        let tree = vec![
            Bookmark {
                title: "One".into(),
                children: vec![Bookmark::new("One.a", 2)],
                ..Bookmark::new("One", 1)
            },
            Bookmark::new("Two", 3),
        ];

        let flat = flatten(&tree);
        let titles: Vec<&str> = flat.iter().map(|(_, b)| b.title.as_str()).collect();
        assert_eq!(titles, vec!["One", "One.a", "Two"]);
        assert_eq!(flat[1].0, 1, "the child is one level down");
    }

    #[test]
    fn a_tree_counts_everything_under_it() {
        let bookmark = Bookmark {
            title: "Root".into(),
            children: vec![Bookmark {
                title: "Child".into(),
                children: vec![Bookmark::new("Grandchild", 3)],
                ..Bookmark::new("Child", 2)
            }],
            ..Bookmark::new("Root", 1)
        };
        assert_eq!(bookmark.count(), 3);
    }
}
