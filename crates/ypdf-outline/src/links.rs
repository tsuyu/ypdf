//! Links: the clickable rectangles on a page (spec §18).
//!
//! A link is a `/Link` annotation with either a destination inside the document
//! or an action pointing outward. Both are read here; only the kinds that can
//! be described honestly are written.
//!
//! One thing this deliberately will not create: a `/Launch` action. It runs a
//! program on the reader's machine, and a tool that offers "add a link" with
//! that in the list is a tool for building a trap. `ypdf-security` reports them
//! when a file already contains one; nothing here can add one.

use lopdf::{Dictionary, Object, ObjectId, dictionary};
use serde::{Deserialize, Serialize};
use ypdf_core::{Error, Result};
use ypdf_doc::Pdf;

/// Where a link goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    /// A web address.
    Url {
        /// The address, as written in the file.
        url: String,
    },
    /// An email address, from a `mailto:` link.
    Email {
        /// The address.
        address: String,
    },
    /// Another page of this document, 1-based.
    Page {
        /// The page.
        page: u32,
    },
    /// A page of another document.
    External {
        /// The file, as written in the link.
        path: String,
        /// The page inside it, if the link names one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page: Option<u32>,
    },
    /// Something this crate can see but not describe: a named destination, a
    /// launch action, embedded JavaScript.
    ///
    /// Reported rather than dropped, so a listing is a true account of what the
    /// page contains.
    Other {
        /// What kind of action it is, as the file names it.
        action: String,
    },
}

impl Target {
    /// A short description for a listing.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Url { url } => url.clone(),
            Self::Email { address } => format!("mailto:{address}"),
            Self::Page { page } => format!("page {page}"),
            Self::External { path, page: None } => path.clone(),
            Self::External {
                path,
                page: Some(page),
            } => format!("{path}, page {page}"),
            Self::Other { action } => format!("{action} (not shown)"),
        }
    }

    /// Is this something a reader could be sent outside the document by?
    #[must_use]
    pub const fn leaves_the_document(&self) -> bool {
        matches!(
            self,
            Self::Url { .. } | Self::Email { .. } | Self::External { .. }
        )
    }
}

/// A link on a page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Link {
    /// 1-based page it is on.
    pub page: u32,
    /// Where it sits, in points: left, bottom, right, top.
    pub rect: [f32; 4],
    /// Where it goes.
    pub target: Target,
}

/// Every link in the document, in page order.
#[must_use]
pub fn list(pdf: &Pdf) -> Vec<Link> {
    let mut out = Vec::new();

    for (index, page_id) in pdf.page_ids().iter().enumerate() {
        let Ok(page) = u32::try_from(index + 1) else {
            continue;
        };
        for (_, annotation) in annotations(pdf, *page_id) {
            let is_link = annotation
                .get(b"Subtype")
                .and_then(Object::as_name)
                .is_ok_and(|name| name == b"Link");
            if !is_link {
                continue;
            }
            let Some(rect) = rect_of(&annotation) else {
                continue;
            };
            let Some(target) = target_of(pdf, &annotation) else {
                continue;
            };
            out.push(Link { page, rect, target });
        }
    }

    out
}

/// Add a link to a page.
///
/// # Errors
///
/// Refuses a page or destination outside the document, and a rectangle with no
/// area — a link nobody can click is not a link.
pub fn add(pdf: &mut Pdf, link: &Link) -> Result<()> {
    let count = pdf.page_count();
    if link.page == 0 || link.page > count {
        return Err(Error::PageOutOfRange {
            requested: link.page,
            pages: count,
        });
    }
    if (link.rect[2] - link.rect[0]).abs() < f32::EPSILON
        || (link.rect[3] - link.rect[1]).abs() < f32::EPSILON
    {
        return Err(Error::Config {
            detail: "a link with no area could never be clicked".into(),
            source_path: None,
        });
    }
    if let Target::Page { page } = &link.target
        && (*page == 0 || *page > count)
    {
        return Err(Error::PageOutOfRange {
            requested: *page,
            pages: count,
        });
    }

    let page_ids = pdf.page_ids().to_vec();
    let Some(page_id) = page_ids.get((link.page - 1) as usize).copied() else {
        return Ok(());
    };

    let action = build_target(pdf, &link.target, &page_ids)?;
    let mut annotation = dictionary! {
        "Type" => "Annot",
        "Subtype" => "Link",
        "Rect" => Object::Array(link.rect.iter().map(|v| Object::Real(*v)).collect()),
        // No visible frame: a border drawn round a link is a 1996 look, and
        // readers that honour it draw it differently from each other.
        "Border" => Object::Array(vec![0.into(), 0.into(), 0.into()]),
    };
    match action {
        Built::Destination(destination) => annotation.set("Dest", destination),
        Built::Action(action) => annotation.set("A", action),
    }

    let annotation_id = pdf.raw_mut().add_object(Object::Dictionary(annotation));
    append_annotation(pdf, page_id, annotation_id);
    Ok(())
}

/// Remove every link matching `predicate`, returning how many went.
pub fn remove_where(pdf: &mut Pdf, mut predicate: impl FnMut(&Link) -> bool) -> usize {
    let mut removed = 0;

    for (index, page_id) in pdf.page_ids().to_vec().into_iter().enumerate() {
        let Ok(page) = u32::try_from(index + 1) else {
            continue;
        };

        let mut keep: Vec<Object> = Vec::new();
        let mut changed = false;

        for (reference, annotation) in annotations(pdf, page_id) {
            let matched = annotation
                .get(b"Subtype")
                .and_then(Object::as_name)
                .is_ok_and(|name| name == b"Link")
                && rect_of(&annotation).is_some_and(|rect| {
                    target_of(pdf, &annotation)
                        .is_some_and(|target| predicate(&Link { page, rect, target }))
                });

            if matched {
                removed += 1;
                changed = true;
            } else {
                keep.push(reference);
            }
        }

        if changed && let Ok(page) = pdf.raw_mut().get_dictionary_mut(page_id) {
            page.set("Annots", Object::Array(keep));
        }
    }

    if removed > 0 {
        tracing::info!(removed, "links removed");
    }
    removed
}

/// What `add` builds: either a destination array or an action dictionary.
enum Built {
    Destination(Object),
    Action(Object),
}

fn build_target(pdf: &mut Pdf, target: &Target, page_ids: &[ObjectId]) -> Result<Built> {
    Ok(match target {
        Target::Url { url } => Built::Action(Object::Dictionary(dictionary! {
            "Type" => "Action",
            "S" => "URI",
            "URI" => Object::string_literal(url.as_str()),
        })),
        Target::Email { address } => Built::Action(Object::Dictionary(dictionary! {
            "Type" => "Action",
            "S" => "URI",
            "URI" => Object::string_literal(format!("mailto:{address}")),
        })),
        Target::Page { page } => {
            let page_id = page_ids
                .get((page.max(&1) - 1) as usize)
                .copied()
                .unwrap_or_default();
            Built::Destination(Object::Array(vec![
                Object::Reference(page_id),
                Object::Name(b"XYZ".to_vec()),
                Object::Null,
                Object::Null,
                Object::Null,
            ]))
        }
        Target::External { path, page } => {
            let mut action = dictionary! {
                "Type" => "Action",
                "S" => "GoToR",
                "F" => Object::string_literal(path.as_str()),
            };
            if let Some(page) = page {
                // A remote destination is by page index, since the other
                // document's objects are not ours to reference.
                action.set(
                    "D",
                    Object::Array(vec![
                        Object::Integer(i64::from(page.saturating_sub(1))),
                        Object::Name(b"XYZ".to_vec()),
                        Object::Null,
                        Object::Null,
                        Object::Null,
                    ]),
                );
            }
            let _ = pdf;
            Built::Action(Object::Dictionary(action))
        }
        Target::Other { action } => {
            // Nothing here knows what to build for one of these, and guessing
            // would produce a link that goes somewhere nobody chose.
            return Err(Error::Unsupported {
                feature: format!("creating a {action} link"),
            });
        }
    })
}

/// The annotations on a page, as `(reference, dictionary)`.
fn annotations(pdf: &Pdf, page_id: ObjectId) -> Vec<(Object, Dictionary)> {
    let list = pdf
        .raw()
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Annots").ok())
        .and_then(|value| match value {
            Object::Array(items) => Some(items.clone()),
            Object::Reference(id) => pdf
                .raw()
                .get_object(*id)
                .ok()
                .and_then(|object| object.as_array().ok())
                .cloned(),
            _ => None,
        })
        .unwrap_or_default();

    list.into_iter()
        .filter_map(|item| {
            let dict = match &item {
                Object::Reference(id) => pdf.raw().get_dictionary(*id).ok().cloned(),
                Object::Dictionary(dict) => Some(dict.clone()),
                _ => None,
            }?;
            Some((item, dict))
        })
        .collect()
}

fn rect_of(annotation: &Dictionary) -> Option<[f32; 4]> {
    let values = annotation.get(b"Rect").ok()?.as_array().ok()?;
    let numbers: Vec<f32> = values
        .iter()
        .filter_map(|value| value.as_float().ok())
        .collect();
    match numbers[..] {
        [x0, y0, x1, y1] => Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)]),
        _ => None,
    }
}

fn target_of(pdf: &Pdf, annotation: &Dictionary) -> Option<Target> {
    if let Ok(destination) = annotation.get(b"Dest") {
        return Some(
            destination_target(pdf, destination).unwrap_or(Target::Other {
                action: "named destination".to_string(),
            }),
        );
    }

    let action = match annotation.get(b"A").ok()? {
        Object::Reference(id) => pdf.raw().get_dictionary(*id).ok()?.clone(),
        Object::Dictionary(dict) => dict.clone(),
        _ => return None,
    };

    let kind = action
        .get(b"S")
        .and_then(Object::as_name)
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .unwrap_or_default();

    match kind.as_str() {
        "URI" => {
            let uri = action.get(b"URI").ok().and_then(|value| match value {
                Object::String(bytes, _) => Some(String::from_utf8_lossy(bytes).into_owned()),
                _ => None,
            })?;
            Some(match uri.strip_prefix("mailto:") {
                Some(address) => Target::Email {
                    address: address.to_string(),
                },
                None => Target::Url { url: uri },
            })
        }
        "GoTo" => {
            let destination = action.get(b"D").ok()?;
            Some(
                destination_target(pdf, destination).unwrap_or(Target::Other {
                    action: "named destination".to_string(),
                }),
            )
        }
        "GoToR" => {
            let path = action.get(b"F").ok().and_then(|value| match value {
                Object::String(bytes, _) => Some(String::from_utf8_lossy(bytes).into_owned()),
                Object::Dictionary(dict) => dict.get(b"F").ok().and_then(|f| match f {
                    Object::String(bytes, _) => Some(String::from_utf8_lossy(bytes).into_owned()),
                    _ => None,
                }),
                _ => None,
            })?;
            let page = action
                .get(b"D")
                .and_then(Object::as_array)
                .ok()
                .and_then(|array| array.first()?.as_i64().ok())
                .and_then(|index| u32::try_from(index + 1).ok());
            Some(Target::External { path, page })
        }
        // Launch, JavaScript, SubmitForm and the rest: named, never built.
        other => Some(Target::Other {
            action: if other.is_empty() {
                "unknown action".to_string()
            } else {
                other.to_string()
            },
        }),
    }
}

fn destination_target(pdf: &Pdf, destination: &Object) -> Option<Target> {
    let array = match destination {
        Object::Array(items) => items.clone(),
        Object::Reference(id) => pdf.raw().get_object(*id).ok()?.as_array().ok()?.clone(),
        _ => return None,
    };
    let page_id = array.first()?.as_reference().ok()?;
    let index = pdf.page_ids().iter().position(|id| *id == page_id)?;
    Some(Target::Page {
        page: u32::try_from(index + 1).ok()?,
    })
}

fn append_annotation(pdf: &mut Pdf, page_id: ObjectId, annotation_id: ObjectId) {
    let existing = pdf
        .raw()
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Annots").ok())
        .and_then(|value| match value {
            Object::Array(items) => Some(items.clone()),
            Object::Reference(id) => pdf
                .raw()
                .get_object(*id)
                .ok()
                .and_then(|object| object.as_array().ok())
                .cloned(),
            _ => None,
        })
        .unwrap_or_default();

    let mut annots = existing;
    annots.push(Object::Reference(annotation_id));
    if let Ok(page) = pdf.raw_mut().get_dictionary_mut(page_id) {
        page.set("Annots", Object::Array(annots));
    }
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
    fn the_links_in_a_document_are_listed_with_where_they_go() {
        let pdf = Pdf::open(fixture("linked.pdf")).expect("opens");
        let links = list(&pdf);

        assert!(!links.is_empty(), "the fixture has links");
        assert!(links.iter().any(|link| link.target.leaves_the_document()));
    }

    #[test]
    fn a_document_with_no_links_lists_none() {
        let pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        assert!(list(&pdf).is_empty());
    }

    #[test]
    fn a_link_that_is_added_reads_back() {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        add(
            &mut pdf,
            &Link {
                page: 1,
                rect: [100.0, 700.0, 200.0, 720.0],
                target: Target::Url {
                    url: "https://example.org/report".into(),
                },
            },
        )
        .expect("adds");

        let bytes = pdf.to_bytes().expect("serializes");
        let reopened = Pdf::from_bytes(&bytes).expect("re-opens");
        let links = list(&reopened);

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].page, 1);
        assert_eq!(
            links[0].target,
            Target::Url {
                url: "https://example.org/report".into()
            }
        );
    }

    #[test]
    fn an_internal_link_points_at_a_page_of_this_document() {
        let mut pdf = Pdf::open(fixture("many-pages.pdf")).expect("opens");
        add(
            &mut pdf,
            &Link {
                page: 1,
                rect: [10.0, 10.0, 100.0, 30.0],
                target: Target::Page { page: 5 },
            },
        )
        .expect("adds");

        let bytes = pdf.to_bytes().expect("serializes");
        let reopened = Pdf::from_bytes(&bytes).expect("re-opens");
        assert_eq!(list(&reopened)[0].target, Target::Page { page: 5 });
    }

    #[test]
    fn an_email_link_is_read_back_as_an_email_rather_than_a_url() {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        add(
            &mut pdf,
            &Link {
                page: 1,
                rect: [10.0, 10.0, 100.0, 30.0],
                target: Target::Email {
                    address: "someone@example.org".into(),
                },
            },
        )
        .expect("adds");

        let bytes = pdf.to_bytes().expect("serializes");
        let reopened = Pdf::from_bytes(&bytes).expect("re-opens");
        assert_eq!(
            list(&reopened)[0].target,
            Target::Email {
                address: "someone@example.org".into()
            }
        );
    }

    #[test]
    fn a_link_to_a_page_that_does_not_exist_is_refused() {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        let error = add(
            &mut pdf,
            &Link {
                page: 1,
                rect: [10.0, 10.0, 100.0, 30.0],
                target: Target::Page { page: 99 },
            },
        )
        .expect_err("refused");
        assert_eq!(error.code(), "E_PAGE_RANGE");
    }

    #[test]
    fn a_link_with_no_area_is_refused() {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        assert!(
            add(
                &mut pdf,
                &Link {
                    page: 1,
                    rect: [10.0, 10.0, 10.0, 30.0],
                    target: Target::Url {
                        url: "https://example.org".into()
                    },
                },
            )
            .is_err()
        );
    }

    #[test]
    fn this_crate_will_not_build_a_launch_link() {
        // Detection reports them; nothing here can create one.
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        let error = add(
            &mut pdf,
            &Link {
                page: 1,
                rect: [10.0, 10.0, 100.0, 30.0],
                target: Target::Other {
                    action: "Launch".into(),
                },
            },
        )
        .expect_err("refused");
        assert_eq!(error.code(), "E_UNSUPPORTED");
    }

    #[test]
    fn links_can_be_removed_by_what_they_point_at() {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        for url in ["https://example.org/a", "https://elsewhere.test/b"] {
            add(
                &mut pdf,
                &Link {
                    page: 1,
                    rect: [10.0, 10.0, 100.0, 30.0],
                    target: Target::Url { url: url.into() },
                },
            )
            .expect("adds");
        }

        let removed = remove_where(
            &mut pdf,
            |link| matches!(&link.target, Target::Url { url } if url.contains("elsewhere.test")),
        );
        assert_eq!(removed, 1);

        let bytes = pdf.to_bytes().expect("serializes");
        let reopened = Pdf::from_bytes(&bytes).expect("re-opens");
        let links = list(&reopened);
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].target,
            Target::Url {
                url: "https://example.org/a".into()
            }
        );
    }

    #[test]
    fn a_dangerous_url_is_listed_exactly_as_the_file_writes_it() {
        // A listing that tidied `javascript:` away, or normalized it into
        // something harmless-looking, would be worse than useless: it would be
        // reassuring. What the file says is what gets reported; judging it is
        // `ypdf-security`'s job.
        let pdf = Pdf::open(fixture("hostile.pdf")).expect("opens");
        let links = list(&pdf);

        assert!(
            links.iter().any(|link| matches!(
                &link.target,
                Target::Url { url } if url.starts_with("javascript:")
            )),
            "{links:?}"
        );
        assert!(
            links.iter().any(|link| matches!(
                &link.target,
                Target::Url { url } if url.contains("192.0.2.10")
            )),
            "a bare IP address must be listed too: {links:?}"
        );
    }

    #[test]
    fn an_action_this_crate_does_not_model_is_reported_rather_than_dropped() {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");

        // Built by hand, because nothing in this crate will create one.
        let action = pdf.raw_mut().add_object(lopdf::dictionary! {
            "Type" => "Action",
            "S" => "Launch",
            "F" => Object::string_literal("payload.exe"),
        });
        let annotation = pdf.raw_mut().add_object(lopdf::dictionary! {
            "Type" => "Annot",
            "Subtype" => "Link",
            "Rect" => Object::Array(vec![10.into(), 10.into(), 100.into(), 30.into()]),
            "A" => Object::Reference(action),
        });
        let page_id = pdf.page_ids()[0];
        append_annotation(&mut pdf, page_id, annotation);

        let links = list(&pdf);
        assert_eq!(
            links[0].target,
            Target::Other {
                action: "Launch".into()
            }
        );
        assert!(!links[0].target.leaves_the_document());
    }

    #[test]
    fn a_target_describes_itself_for_a_listing() {
        assert_eq!(Target::Page { page: 4 }.describe(), "page 4");
        assert_eq!(
            Target::Email {
                address: "a@b.test".into()
            }
            .describe(),
            "mailto:a@b.test"
        );
        assert!(
            Target::Other {
                action: "Launch".into()
            }
            .describe()
            .contains("Launch")
        );
    }
}
