//! What a form field is, and how one is read out of a document (spec §16).
//!
//! A field is not one object. Its name, type, flags, and value can each come
//! from itself or from any ancestor in the field tree, and the rectangle it
//! occupies belongs to a *widget* — which may be the same object, or several
//! separate ones for a radio group. Reading it correctly means walking the tree
//! carrying inherited attributes down, which is the same shape of problem as
//! the page tree in `ypdf-doc`, and wrong in the same ways when skipped.

use lopdf::{Dictionary, Object, ObjectId};
use serde::{Deserialize, Serialize};
use ypdf_doc::Pdf;

/// How deep a field tree may nest before this stops following it.
const MAX_DEPTH: usize = 32;

/// What kind of field it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A line or box of text.
    #[default]
    Text,
    /// A single on/off box.
    Checkbox,
    /// One of a group; the group shares a name.
    Radio,
    /// A dropdown or list.
    Choice,
    /// A push button. Carries no value.
    Button,
    /// A place for a signature.
    ///
    /// Listed, never filled: putting a value in one would produce a document
    /// claiming to be signed when it is not.
    Signature,
}

impl Kind {
    /// Can this kind hold a value a person types or picks?
    #[must_use]
    pub const fn is_fillable(self) -> bool {
        matches!(
            self,
            Self::Text | Self::Checkbox | Self::Radio | Self::Choice
        )
    }

    /// The word used in reports and JSON.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Checkbox => "checkbox",
            Self::Radio => "radio",
            Self::Choice => "choice",
            Self::Button => "button",
            Self::Signature => "signature",
        }
    }
}

/// One form field.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// Its fully qualified name, as `parent.child`.
    pub name: String,
    /// What kind it is.
    pub kind: Kind,
    /// Its current value, as text.
    ///
    /// A checkbox reads as its on-state name — usually `Yes` — or an empty
    /// string when it is off.
    #[serde(default)]
    pub value: String,
    /// The choices, for a dropdown, list, or radio group.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// Whether a reader should refuse to submit without it.
    #[serde(default)]
    pub required: bool,
    /// Whether a reader should refuse to change it.
    #[serde(default)]
    pub read_only: bool,
    /// The 1-based pages its widgets are on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<u32>,
}

impl Field {
    /// Can this field be filled in?
    ///
    /// A read-only field is listed but not written: whoever built the form
    /// marked it as not for editing, and quietly overriding that produces a
    /// document that disagrees with itself.
    #[must_use]
    pub const fn is_writable(&self) -> bool {
        self.kind.is_fillable() && !self.read_only
    }
}

/// Field flags, from `/Ff`.
const READ_ONLY: u32 = 1;
const REQUIRED: u32 = 1 << 1;
const RADIO: u32 = 1 << 15;
const PUSH_BUTTON: u32 = 1 << 16;

/// Everything needed to edit one field, gathered from the tree.
#[derive(Clone, Debug)]
pub struct Located {
    /// The field itself.
    pub field: Field,
    /// The object carrying `/V`, which is the field, not its widgets.
    pub id: ObjectId,
    /// The widget annotations that draw it.
    pub widgets: Vec<ObjectId>,
}

/// Does this document have a form at all?
#[must_use]
pub fn has_form(pdf: &Pdf) -> bool {
    acroform(pdf).is_some()
}

/// The `/AcroForm` dictionary.
#[must_use]
pub fn acroform(pdf: &Pdf) -> Option<Dictionary> {
    let catalog = pdf.raw().catalog().ok()?;
    match catalog.get(b"AcroForm").ok()? {
        Object::Reference(id) => pdf.raw().get_dictionary(*id).ok().cloned(),
        Object::Dictionary(dict) => Some(dict.clone()),
        _ => None,
    }
}

/// The object id of `/AcroForm`, when it is an indirect one.
#[must_use]
pub fn acroform_id(pdf: &Pdf) -> Option<ObjectId> {
    pdf.raw()
        .catalog()
        .ok()?
        .get(b"AcroForm")
        .ok()?
        .as_reference()
        .ok()
}

/// Read every field, with the objects needed to edit it.
#[must_use]
pub fn locate(pdf: &Pdf) -> Vec<Located> {
    let Some(form) = acroform(pdf) else {
        return Vec::new();
    };
    let Some(roots) = array_of(pdf, form.get(b"Fields").ok()) else {
        return Vec::new();
    };

    let pages = page_numbers(pdf);
    let mut out = Vec::new();
    for root in roots {
        walk(pdf, root, &Inherited::default(), "", &pages, 0, &mut out);
    }
    out
}

/// Read every field.
#[must_use]
pub fn read(pdf: &Pdf) -> Vec<Field> {
    locate(pdf).into_iter().map(|found| found.field).collect()
}

/// Attributes a field inherits from its ancestors.
#[derive(Clone, Debug, Default)]
struct Inherited {
    kind: Option<Kind>,
    flags: u32,
    options: Vec<String>,
}

fn walk(
    pdf: &Pdf,
    id: ObjectId,
    inherited: &Inherited,
    prefix: &str,
    pages: &std::collections::HashMap<ObjectId, u32>,
    depth: usize,
    out: &mut Vec<Located>,
) {
    if depth >= MAX_DEPTH {
        return;
    }
    let Ok(dict) = pdf.raw().get_dictionary(id) else {
        return;
    };

    let partial = dict
        .get(b"T")
        .ok()
        .and_then(|value| match value {
            Object::String(bytes, _) => Some(ypdf_doc::decode_text(bytes)),
            _ => None,
        })
        .unwrap_or_default();
    let name = match (prefix.is_empty(), partial.is_empty()) {
        (_, true) => prefix.to_string(),
        (true, false) => partial.clone(),
        (false, false) => format!("{prefix}.{partial}"),
    };

    let mut state = Inherited {
        kind: field_kind(dict).or(inherited.kind),
        flags: dict
            .get(b"Ff")
            .and_then(Object::as_i64)
            .ok()
            .and_then(|flags| u32::try_from(flags).ok())
            .unwrap_or(inherited.flags),
        options: options_of(pdf, dict).unwrap_or_else(|| inherited.options.clone()),
    };

    // Children are the field tree; widgets are how it is drawn. An object can
    // be both, which is the common case for a single-widget field.
    let kids = array_of(pdf, dict.get(b"Kids").ok()).unwrap_or_default();
    let (widgets, children): (Vec<ObjectId>, Vec<ObjectId>) = kids
        .into_iter()
        .partition(|kid| is_widget(pdf, *kid) && !is_field(pdf, *kid));

    if !children.is_empty() {
        for child in children {
            walk(pdf, child, &state, &name, pages, depth + 1, out);
        }
        return;
    }

    let Some(kind) = state.kind else {
        // No type anywhere up the tree: not a field, just a node.
        return;
    };

    let kind = match kind {
        Kind::Checkbox if state.flags & RADIO != 0 => Kind::Radio,
        Kind::Checkbox if state.flags & PUSH_BUTTON != 0 => Kind::Button,
        other => other,
    };

    let mut widget_ids = widgets;
    if widget_ids.is_empty() && is_widget(pdf, id) {
        widget_ids.push(id);
    }

    // A radio group's options are the on-states of its widgets.
    if kind == Kind::Radio && state.options.is_empty() {
        state.options = widget_ids
            .iter()
            .filter_map(|widget| on_state(pdf, *widget))
            .collect();
    }

    let mut on_pages: Vec<u32> = widget_ids
        .iter()
        .filter_map(|widget| page_of(pdf, *widget, pages))
        .collect();
    on_pages.sort_unstable();
    on_pages.dedup();

    out.push(Located {
        field: Field {
            name,
            kind,
            value: value_of(pdf, dict),
            options: state.options,
            required: state.flags & REQUIRED != 0,
            read_only: state.flags & READ_ONLY != 0,
            pages: on_pages,
        },
        id,
        widgets: widget_ids,
    });
}

fn field_kind(dict: &Dictionary) -> Option<Kind> {
    let name = dict.get(b"FT").and_then(Object::as_name).ok()?;
    Some(match name {
        b"Tx" => Kind::Text,
        b"Btn" => Kind::Checkbox,
        b"Ch" => Kind::Choice,
        b"Sig" => Kind::Signature,
        _ => return None,
    })
}

/// The value of a field, as text.
pub(crate) fn value_of(pdf: &Pdf, dict: &Dictionary) -> String {
    let Ok(value) = dict.get(b"V") else {
        return String::new();
    };
    value_text(pdf, value)
}

fn value_text(pdf: &Pdf, value: &Object) -> String {
    match value {
        Object::String(bytes, _) => ypdf_doc::decode_text(bytes),
        Object::Name(name) => {
            let text = String::from_utf8_lossy(name).into_owned();
            // `/Off` is how a checkbox says "no", and reporting it as the word
            // "Off" would read like a value someone typed.
            if text == "Off" { String::new() } else { text }
        }
        Object::Array(items) => items
            .iter()
            .map(|item| value_text(pdf, item))
            .collect::<Vec<_>>()
            .join(", "),
        Object::Reference(id) => pdf
            .raw()
            .get_object(*id)
            .map(|object| value_text(pdf, object))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn options_of(pdf: &Pdf, dict: &Dictionary) -> Option<Vec<String>> {
    let items = match dict.get(b"Opt").ok()? {
        Object::Array(items) => items.clone(),
        Object::Reference(id) => pdf.raw().get_object(*id).ok()?.as_array().ok()?.clone(),
        _ => return None,
    };

    Some(
        items
            .iter()
            .map(|item| match item {
                // An option can be `value` or `[value, label]`; the label is
                // what a reader shows, and what someone would type back.
                Object::Array(pair) => pair
                    .last()
                    .map(|value| value_text(pdf, value))
                    .unwrap_or_default(),
                other => value_text(pdf, other),
            })
            .collect(),
    )
}

/// The `/AP /N` state that turns this widget on.
pub(crate) fn on_state(pdf: &Pdf, widget: ObjectId) -> Option<String> {
    let dict = pdf.raw().get_dictionary(widget).ok()?;
    let appearance = match dict.get(b"AP").ok()? {
        Object::Reference(id) => pdf.raw().get_dictionary(*id).ok()?.clone(),
        Object::Dictionary(dict) => dict.clone(),
        _ => return None,
    };
    let normal = match appearance.get(b"N").ok()? {
        Object::Reference(id) => pdf.raw().get_dictionary(*id).ok()?.clone(),
        Object::Dictionary(dict) => dict.clone(),
        _ => return None,
    };

    normal
        .iter()
        .map(|(name, _)| String::from_utf8_lossy(name).into_owned())
        .find(|name| name != "Off")
}

fn is_widget(pdf: &Pdf, id: ObjectId) -> bool {
    pdf.raw().get_dictionary(id).is_ok_and(|dict| {
        dict.get(b"Subtype")
            .and_then(Object::as_name)
            .is_ok_and(|subtype| subtype == b"Widget")
    })
}

fn is_field(pdf: &Pdf, id: ObjectId) -> bool {
    pdf.raw()
        .get_dictionary(id)
        .is_ok_and(|dict| dict.get(b"T").is_ok() || dict.get(b"Kids").is_ok())
}

fn array_of(pdf: &Pdf, value: Option<&Object>) -> Option<Vec<ObjectId>> {
    let items = match value? {
        Object::Array(items) => items.clone(),
        Object::Reference(id) => pdf.raw().get_object(*id).ok()?.as_array().ok()?.clone(),
        _ => return None,
    };
    Some(
        items
            .iter()
            .filter_map(|item| item.as_reference().ok())
            .collect(),
    )
}

/// Page object ids to 1-based page numbers.
fn page_numbers(pdf: &Pdf) -> std::collections::HashMap<ObjectId, u32> {
    pdf.page_ids()
        .iter()
        .enumerate()
        .filter_map(|(index, id)| Some((*id, u32::try_from(index + 1).ok()?)))
        .collect()
}

fn page_of(
    pdf: &Pdf,
    widget: ObjectId,
    pages: &std::collections::HashMap<ObjectId, u32>,
) -> Option<u32> {
    let dict = pdf.raw().get_dictionary(widget).ok()?;
    if let Ok(page) = dict.get(b"P").and_then(Object::as_reference) {
        return pages.get(&page).copied();
    }

    // Not every producer writes `/P`; the page that lists the widget is the
    // other way to find out.
    for (page_id, number) in pages {
        let annots = pdf
            .raw()
            .get_dictionary(*page_id)
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

        if annots
            .iter()
            .any(|item| item.as_reference().ok() == Some(widget))
        {
            return Some(*number);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    #[test]
    fn a_signature_field_is_never_fillable() {
        // Putting a value in one would produce a document claiming to be signed
        // when it is not.
        assert!(!Kind::Signature.is_fillable());
        assert!(!Kind::Button.is_fillable());
        assert!(Kind::Text.is_fillable());
    }

    #[test]
    fn a_read_only_field_is_listed_but_not_writable() {
        let field = Field {
            name: "total".into(),
            kind: Kind::Text,
            read_only: true,
            ..Field::default()
        };
        assert!(!field.is_writable());
    }

    #[test]
    fn a_checkbox_that_is_off_reads_as_empty_rather_than_as_the_word_off() {
        let pdf = ypdf_doc::Pdf::from_bytes(&minimal()).expect("opens");
        let dict = dictionary! { "V" => Object::Name(b"Off".to_vec()) };
        assert_eq!(value_of(&pdf, &dict), "");

        let dict = dictionary! { "V" => Object::Name(b"Yes".to_vec()) };
        assert_eq!(value_of(&pdf, &dict), "Yes");
    }

    #[test]
    fn a_document_with_no_form_reads_as_empty() {
        let pdf = ypdf_doc::Pdf::from_bytes(&minimal()).expect("opens");
        assert!(!has_form(&pdf));
        assert!(read(&pdf).is_empty());
    }

    /// The smallest document that opens.
    fn minimal() -> Vec<u8> {
        let mut doc = lopdf::Document::with_version("1.7");
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
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => 1_i64,
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);

        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("serializes");
        bytes
    }
}
