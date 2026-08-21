//! Finding the signature fields, and what each one says about itself.

use std::collections::BTreeMap;

use lopdf::{Dictionary, Document, Object, ObjectId};
use ypdf_doc::decode_text;

/// A signature field as the file holds it.
#[derive(Clone, Debug)]
pub struct Found {
    /// Fully-qualified field name.
    pub name: String,
    /// The page its widget sits on, counting from 1.
    pub page: Option<u32>,
    /// The signature dictionary, when the field has been signed.
    pub value: Option<Dictionary>,
}

/// Every signature field in the document, signed or not.
///
/// Walks the field tree rather than the pages: a signature field can exist
/// with no widget at all, and one that has been signed but never placed still
/// signs the document.
pub fn signature_fields(doc: &Document) -> Vec<Found> {
    let pages = page_numbers(doc);
    let mut found = Vec::new();

    let Ok(catalog) = doc.catalog() else {
        return found;
    };
    let Some(acroform) = dict_of(doc, catalog.get(b"AcroForm").ok()) else {
        return found;
    };
    let Some(fields) = array_of(doc, acroform.get(b"Fields").ok()) else {
        return found;
    };

    for field in fields {
        walk(doc, &field, None, &pages, &mut found, 0);
    }
    found
}

/// Depth limit: a field tree is shallow, and a cycle in a hostile file must
/// not become an infinite walk.
const MAX_DEPTH: usize = 32;

fn walk(
    doc: &Document,
    object: &Object,
    inherited_name: Option<&str>,
    pages: &BTreeMap<ObjectId, u32>,
    out: &mut Vec<Found>,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let Some(dict) = dict_of(doc, Some(object)) else {
        return;
    };

    let partial = dict
        .get(b"T")
        .ok()
        .and_then(|value| value.as_str().ok())
        .map(decode_text);
    let name = match (inherited_name, partial.as_deref()) {
        (Some(parent), Some(part)) => format!("{parent}.{part}"),
        (Some(parent), None) => parent.to_string(),
        (None, Some(part)) => part.to_string(),
        (None, None) => String::new(),
    };

    if let Some(kids) = array_of(doc, dict.get(b"Kids").ok()) {
        // A field with kids may still be a field: a signature field with one
        // widget is often written as a parent with a single child.
        let is_field = kids
            .iter()
            .filter_map(|kid| dict_of(doc, Some(kid)))
            .all(|kid| kid.get(b"T").is_err());
        if !is_field {
            for kid in kids {
                walk(doc, &kid, Some(&name), pages, out, depth + 1);
            }
            return;
        }
    }

    // `/FT` is inheritable, so a field that says nothing is only a signature
    // field if an ancestor said so — which is what the recursion passes down
    // through the name, and what this re-reads from the parent chain.
    if !is_signature_field(doc, &dict, 0) {
        return;
    }

    let value = dict
        .get(b"V")
        .ok()
        .and_then(|value| dict_of(doc, Some(value)));

    out.push(Found {
        name: if name.is_empty() {
            "(unnamed)".to_string()
        } else {
            name
        },
        page: widget_page(doc, &dict, pages),
        value,
    });
}

fn is_signature_field(doc: &Document, dict: &Dictionary, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    match dict.get(b"FT").and_then(Object::as_name) {
        Ok(name) => name == b"Sig",
        Err(_) => dict
            .get(b"Parent")
            .ok()
            .and_then(|parent| dict_of(doc, Some(parent)))
            .is_some_and(|parent| is_signature_field(doc, &parent, depth + 1)),
    }
}

/// The page a field's widget is on.
fn widget_page(doc: &Document, dict: &Dictionary, pages: &BTreeMap<ObjectId, u32>) -> Option<u32> {
    if let Ok(Object::Reference(id)) = dict.get(b"P") {
        return pages.get(id).copied();
    }
    // The widget may be a kid rather than the field itself.
    let kids = array_of(doc, dict.get(b"Kids").ok())?;
    kids.iter().find_map(|kid| {
        let widget = dict_of(doc, Some(kid))?;
        match widget.get(b"P") {
            Ok(Object::Reference(id)) => pages.get(id).copied(),
            _ => None,
        }
    })
}

fn page_numbers(doc: &Document) -> BTreeMap<ObjectId, u32> {
    doc.get_pages()
        .into_iter()
        .map(|(number, id)| (id, number))
        .collect()
}

fn dict_of(doc: &Document, object: Option<&Object>) -> Option<Dictionary> {
    match object? {
        Object::Dictionary(dict) => Some(dict.clone()),
        Object::Reference(id) => doc.get_dictionary(*id).ok().cloned(),
        _ => None,
    }
}

fn array_of(doc: &Document, object: Option<&Object>) -> Option<Vec<Object>> {
    match object? {
        Object::Array(items) => Some(items.clone()),
        Object::Reference(id) => doc.get_object(*id).ok()?.as_array().ok().cloned(),
        _ => None,
    }
}

/// A string entry from a signature dictionary, decoded.
pub fn text(dict: &Dictionary, key: &[u8]) -> Option<String> {
    let value = dict.get(key).ok()?.as_str().ok()?;
    let text = decode_text(value);
    (!text.trim().is_empty()).then_some(text)
}

/// Does this signature certify the document rather than approve it?
///
/// A certification signature is the one that says what may be changed
/// afterwards; there can only be one, and it is the first.
pub fn is_certification(doc: &Document, signature: &Dictionary) -> bool {
    if signature
        .get(b"Reference")
        .ok()
        .and_then(|value| array_of(doc, Some(value)))
        .is_some_and(|references| {
            references.iter().any(|reference| {
                dict_of(doc, Some(reference)).is_some_and(|dict| {
                    dict.get(b"TransformMethod")
                        .and_then(Object::as_name)
                        .is_ok_and(|method| method == b"DocMDP")
                })
            })
        })
    {
        return true;
    }

    // The catalog's /Perms points at the certification signature.
    doc.catalog()
        .ok()
        .and_then(|catalog| dict_of(doc, catalog.get(b"Perms").ok()))
        .and_then(|perms| dict_of(doc, perms.get(b"DocMDP").ok()))
        .is_some_and(
            |doc_mdp| match (doc_mdp.get(b"Contents"), signature.get(b"Contents")) {
                (Ok(theirs), Ok(ours)) => theirs == ours,
                _ => false,
            },
        )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use lopdf::dictionary;

    use super::*;

    fn document_with(fields: Vec<Object>) -> Document {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
            "AcroForm" => dictionary! { "Fields" => fields },
        });
        doc.trailer.set("Root", catalog);
        doc
    }

    #[test]
    fn an_unsigned_signature_field_is_still_found() {
        let doc = document_with(vec![Object::Dictionary(dictionary! {
            "FT" => "Sig",
            "T" => Object::string_literal("Signature1"),
        })]);
        let found = signature_fields(&doc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Signature1");
        assert!(found[0].value.is_none(), "nobody has signed it");
    }

    #[test]
    fn a_text_field_is_not_a_signature_field() {
        let doc = document_with(vec![Object::Dictionary(dictionary! {
            "FT" => "Tx",
            "T" => Object::string_literal("Name"),
        })]);
        assert!(signature_fields(&doc).is_empty());
    }

    #[test]
    fn a_field_type_inherited_from_a_parent_still_counts() {
        // /FT is inheritable. A walker that only reads the child's own
        // dictionary misses signatures in forms built this way.
        let mut doc = document_with(Vec::new());
        let parent_id = doc.add_object(dictionary! {
            "FT" => "Sig",
            "T" => Object::string_literal("Approvals"),
        });
        let child_id = doc.add_object(dictionary! {
            "Parent" => Object::Reference(parent_id),
            "T" => Object::string_literal("Second"),
        });
        if let Ok(parent) = doc.get_dictionary_mut(parent_id) {
            parent.set("Kids", vec![Object::Reference(child_id)]);
        }
        let catalog_id = doc
            .trailer
            .get(b"Root")
            .expect("root")
            .as_reference()
            .expect("reference");
        if let Ok(catalog) = doc.get_dictionary_mut(catalog_id) {
            catalog.set(
                "AcroForm",
                dictionary! { "Fields" => vec![Object::Reference(parent_id)] },
            );
        }

        let found = signature_fields(&doc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Approvals.Second");
    }
}
