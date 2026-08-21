//! Getting at every object in a document, once each.
//!
//! PDF/A requirements are about *everything* in the file, not about the pages
//! a reader happens to draw. A forbidden filter on an unreferenced stream still
//! makes the file non-conforming, so these walks go over the object table
//! rather than following the page tree.

use lopdf::{Dictionary, Document, Object, Stream};

/// Every dictionary in the file, including the ones nested inside others.
///
/// An action sits inside an annotation, which sits inside a page's `/Annots`
/// array; a walk that only visited indirect objects would miss it.
pub fn dictionaries(doc: &Document) -> Vec<&Dictionary> {
    let mut found = Vec::new();
    for object in doc.objects.values() {
        collect(object, &mut found);
    }
    found
}

fn collect<'a>(object: &'a Object, out: &mut Vec<&'a Dictionary>) {
    match object {
        Object::Dictionary(dict) => {
            out.push(dict);
            for (_, value) in dict.iter() {
                collect(value, out);
            }
        }
        Object::Stream(stream) => {
            out.push(&stream.dict);
            for (_, value) in stream.dict.iter() {
                collect(value, out);
            }
        }
        Object::Array(items) => {
            for item in items {
                collect(item, out);
            }
        }
        _ => {}
    }
}

/// Every stream in the file.
pub fn streams(doc: &Document) -> Vec<&Stream> {
    doc.objects
        .values()
        .filter_map(|object| object.as_stream().ok())
        .collect()
}

/// Follow a reference, if that is what this is.
pub fn resolve<'a>(doc: &'a Document, object: &'a Object) -> Option<&'a Object> {
    match object {
        Object::Reference(id) => doc.get_object(*id).ok(),
        other => Some(other),
    }
}

/// The dictionary an entry names, whether it is inline or a reference.
pub fn dict_at<'a>(doc: &'a Document, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Dictionary> {
    let value = dict.get(key).ok()?;
    match resolve(doc, value)? {
        Object::Dictionary(d) => Some(d),
        Object::Stream(stream) => Some(&stream.dict),
        _ => None,
    }
}

/// The array an entry names, whether it is inline or a reference.
pub fn array_at<'a>(
    doc: &'a Document,
    dict: &'a Dictionary,
    key: &[u8],
) -> Option<&'a Vec<Object>> {
    let value = dict.get(key).ok()?;
    match resolve(doc, value)? {
        Object::Array(items) => Some(items),
        _ => None,
    }
}

/// A `/Name` entry as a string, whether it is inline or a reference.
pub fn name_at(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<String> {
    let value = dict.get(key).ok()?;
    let name = resolve(doc, value)?.as_name().ok()?;
    Some(String::from_utf8_lossy(name).into_owned())
}

/// Is this dictionary's entry the name given?
pub fn is_named(dict: &Dictionary, key: &[u8], name: &[u8]) -> bool {
    dict.get(key)
        .and_then(lopdf::Object::as_name)
        .is_ok_and(|value| value == name)
}

/// Is this dictionary's `/Type` the one named?
pub fn is_type(dict: &Dictionary, name: &[u8]) -> bool {
    is_named(dict, b"Type", name)
}

/// The names in a `/Filter` entry, which may be one name or an array of them.
pub fn filters(doc: &Document, dict: &Dictionary) -> Vec<String> {
    let Ok(value) = dict.get(b"Filter") else {
        return Vec::new();
    };
    let Some(value) = resolve(doc, value) else {
        return Vec::new();
    };
    match value {
        Object::Name(name) => vec![String::from_utf8_lossy(name).into_owned()],
        Object::Array(items) => items
            .iter()
            .filter_map(|item| item.as_name().ok())
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect(),
        _ => Vec::new(),
    }
}

/// A number entry, however it was written.
pub fn number_at(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<f32> {
    let value = dict.get(key).ok()?;
    match resolve(doc, value)? {
        Object::Integer(n) => Some(*n as f32),
        Object::Real(n) => Some(*n),
        _ => None,
    }
}

/// A boolean entry, however it was written.
pub fn bool_at(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<bool> {
    let value = dict.get(key).ok()?;
    match resolve(doc, value)? {
        Object::Boolean(b) => Some(*b),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use lopdf::dictionary;

    use super::*;

    #[test]
    fn a_nested_dictionary_is_still_walked() {
        let mut doc = Document::with_version("1.7");
        doc.add_object(dictionary! {
            "Type" => "Page",
            "Annots" => vec![Object::Dictionary(dictionary! {
                "Subtype" => "Link",
                "A" => Object::Dictionary(dictionary! { "S" => "Launch" }),
            })],
        });

        let found = dictionaries(&doc);
        assert!(
            found.iter().any(|dict| is_named(dict, b"S", b"Launch")),
            "an action nested two levels down has to be visible to the checks"
        );
    }

    #[test]
    fn filters_read_as_one_name_or_a_list() {
        let doc = Document::with_version("1.7");
        let single = dictionary! { "Filter" => "LZWDecode" };
        assert_eq!(filters(&doc, &single), vec!["LZWDecode".to_string()]);

        let several = dictionary! { "Filter" => vec!["ASCII85Decode".into(), "LZWDecode".into()] };
        assert_eq!(
            filters(&doc, &several),
            vec!["ASCII85Decode".to_string(), "LZWDecode".to_string()]
        );
    }
}
