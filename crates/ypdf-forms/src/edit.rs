//! Filling, clearing, and flattening (spec §16).
//!
//! Filling a field is two jobs, and doing only the first is the classic bug: the
//! **value** goes in `/V`, and the **appearance** — the stream a reader draws —
//! has to agree with it. A file with the right value and a stale appearance
//! looks empty in one reader and filled in another, and prints blank.
//!
//! So a filled text field gets a freshly built appearance stream, and
//! `/NeedAppearances` is set as well, which asks conforming readers to redo the
//! work with their own typography. Checkboxes and radios need no drawing: their
//! appearances already exist, one per state, and filling one means pointing
//! `/AS` at the right one.

use std::collections::BTreeMap;

use lopdf::{Dictionary, Object, ObjectId, Stream, dictionary};
use ypdf_core::Result;
use ypdf_doc::{Pdf, winansi};

use crate::model::{self, Kind, Located};

/// What a fill or clear did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Fields whose value changed.
    pub filled: usize,
    /// Fields cleared.
    pub cleared: usize,
    /// Fields flattened into the page.
    pub flattened: usize,
    /// Names given that the document does not have.
    ///
    /// Reported rather than ignored: a spelling mistake in a field name would
    /// otherwise look exactly like a successful fill.
    pub unknown: Vec<String>,
    /// Names that exist but could not be written, and why.
    pub refused: Vec<(String, &'static str)>,
}

impl Report {
    /// Did everything asked for happen?
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.unknown.is_empty() && self.refused.is_empty()
    }

    /// A summary for a person.
    #[must_use]
    pub fn to_human(&self) -> String {
        let mut out = String::new();
        if self.filled > 0 {
            out.push_str(&format!("{} field(s) filled\n", self.filled));
        }
        if self.cleared > 0 {
            out.push_str(&format!("{} field(s) cleared\n", self.cleared));
        }
        if self.flattened > 0 {
            out.push_str(&format!("{} field(s) flattened\n", self.flattened));
        }
        if !self.unknown.is_empty() {
            out.push_str(&format!("\nNo such field: {}\n", self.unknown.join(", ")));
        }
        for (name, reason) in &self.refused {
            out.push_str(&format!("Left alone: {name} ({reason})\n"));
        }
        if out.is_empty() {
            out.push_str("Nothing to do.\n");
        }
        out
    }
}

/// Fill fields by name.
///
/// Every name is checked before anything is written, so a typo does not leave a
/// form half filled.
pub fn fill(pdf: &mut Pdf, values: &BTreeMap<String, String>) -> Result<Report> {
    let located = model::locate(pdf);
    let mut report = Report::default();

    for name in values.keys() {
        if !located.iter().any(|found| found.field.name == *name) {
            report.unknown.push(name.clone());
        }
    }

    for found in &located {
        let Some(value) = values.get(&found.field.name) else {
            continue;
        };

        match refusal(found) {
            Some(reason) => report.refused.push((found.field.name.clone(), reason)),
            None => {
                set_value(pdf, found, value)?;
                report.filled += 1;
            }
        }
    }

    if report.filled > 0 {
        need_appearances(pdf, true);
        tracing::info!(filled = report.filled, "form filled");
    }
    Ok(report)
}

/// Why a field cannot be written.
fn refusal(found: &Located) -> Option<&'static str> {
    match found.field.kind {
        Kind::Signature => Some("a signature field is not something to type into"),
        Kind::Button => Some("a push button holds no value"),
        _ if found.field.read_only => Some("marked read-only by whoever built the form"),
        _ => None,
    }
}

/// Clear every value.
pub fn clear(pdf: &mut Pdf) -> Result<Report> {
    let located = model::locate(pdf);
    let mut report = Report::default();

    for found in &located {
        if !found.field.kind.is_fillable() || found.field.value.is_empty() {
            continue;
        }
        set_value(pdf, found, "")?;
        report.cleared += 1;
    }

    if report.cleared > 0 {
        need_appearances(pdf, true);
        tracing::info!(cleared = report.cleared, "form cleared");
    }
    Ok(report)
}

/// Write one field's value and make its appearance agree.
fn set_value(pdf: &mut Pdf, found: &Located, value: &str) -> Result<()> {
    match found.field.kind {
        Kind::Checkbox | Kind::Radio => set_toggle(pdf, found, value),
        Kind::Text | Kind::Choice => set_text(pdf, found, value),
        // Refused earlier; nothing to do.
        Kind::Button | Kind::Signature => Ok(()),
    }
}

/// A checkbox or radio: point `/V` and `/AS` at a state that exists.
fn set_toggle(pdf: &mut Pdf, found: &Located, value: &str) -> Result<()> {
    let wanted = value.trim();
    let off = wanted.is_empty()
        || wanted.eq_ignore_ascii_case("off")
        || wanted.eq_ignore_ascii_case("false")
        || wanted == "0";

    // The on-state is whatever the widget's appearance dictionary calls it —
    // usually `/Yes`, but a producer may use anything. Writing `/Yes` into a
    // widget whose state is `/On` gives a box that is checked in the data and
    // blank on the page.
    let states: Vec<(ObjectId, String)> = found
        .widgets
        .iter()
        .filter_map(|widget| Some((*widget, model::on_state(pdf, *widget)?)))
        .collect();

    let chosen = if off {
        None
    } else if found.field.kind == Kind::Radio {
        states
            .iter()
            .find(|(_, state)| state.eq_ignore_ascii_case(wanted))
            .map(|(_, state)| state.clone())
            // A radio group filled with something that is not one of its
            // buttons: take the first, rather than write a value no widget can
            // display.
            .or_else(|| states.first().map(|(_, state)| state.clone()))
    } else {
        states
            .first()
            .map(|(_, state)| state.clone())
            .or_else(|| Some("Yes".to_string()))
    };

    let name = chosen.clone().unwrap_or_else(|| "Off".to_string());
    if let Ok(field) = pdf.raw_mut().get_dictionary_mut(found.id) {
        field.set("V", Object::Name(name.clone().into_bytes()));
    }

    for (widget, state) in states {
        // Each widget of a radio group shows its own state or `/Off`.
        let shown = match &chosen {
            Some(chosen) if *chosen == state => state.clone(),
            _ => "Off".to_string(),
        };
        if let Ok(dict) = pdf.raw_mut().get_dictionary_mut(widget) {
            dict.set("AS", Object::Name(shown.into_bytes()));
        }
    }

    Ok(())
}

/// A text or choice field: write the string and redraw it.
fn set_text(pdf: &mut Pdf, found: &Located, value: &str) -> Result<()> {
    if value.is_empty() {
        if let Ok(field) = pdf.raw_mut().get_dictionary_mut(found.id) {
            field.remove(b"V");
        }
    } else if let Ok(field) = pdf.raw_mut().get_dictionary_mut(found.id) {
        field.set(
            "V",
            Object::String(ypdf_doc::encode_text(value), lopdf::StringFormat::Literal),
        );
    }

    let appearance = default_appearance(pdf, found);
    for widget in &found.widgets {
        let Some(rect) = widget_rect(pdf, *widget) else {
            continue;
        };
        let stream = appearance_stream(value, rect, &appearance);
        let stream_id = pdf.raw_mut().add_object(Object::Stream(stream));
        // The font the appearance names has to be reachable from the stream, or
        // a reader draws nothing at all.
        attach_resources(pdf, stream_id, &appearance);

        if let Ok(dict) = pdf.raw_mut().get_dictionary_mut(*widget) {
            dict.set(
                "AP",
                Object::Dictionary(dictionary! { "N" => Object::Reference(stream_id) }),
            );
        }
    }

    Ok(())
}

/// The `/DA` string and the resources it refers to.
#[derive(Clone, Debug)]
struct Appearance {
    /// The default-appearance operators, e.g. `/Helv 12 Tf 0 g`.
    da: String,
    /// The font size it names, or `None` for auto.
    size: Option<f32>,
    /// The document's `/DR`, which holds the font it names.
    resources: Option<Object>,
}

fn default_appearance(pdf: &Pdf, found: &Located) -> Appearance {
    let field_da = pdf
        .raw()
        .get_dictionary(found.id)
        .ok()
        .and_then(|dict| dict.get(b"DA").ok().cloned());

    let form = model::acroform(pdf);
    let form_da = form.as_ref().and_then(|form| form.get(b"DA").ok().cloned());

    let da = field_da
        .or(form_da)
        .and_then(|value| match value {
            Object::String(bytes, _) => Some(String::from_utf8_lossy(&bytes).into_owned()),
            _ => None,
        })
        // Helvetica at auto size, black: what almost every form says anyway.
        .unwrap_or_else(|| "/Helv 0 Tf 0 g".to_string());

    // `/Helv 12 Tf` — the number before `Tf` is the size, and zero means auto.
    let size = da
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|pair| pair[1] == "Tf")
        .and_then(|pair| pair[0].parse::<f32>().ok())
        .filter(|size| *size > 0.0);

    Appearance {
        da,
        size,
        resources: form.and_then(|form| form.get(b"DR").ok().cloned()),
    }
}

/// Build the stream a reader draws for a filled text field.
fn appearance_stream(value: &str, rect: [f32; 4], appearance: &Appearance) -> Stream {
    let width = (rect[2] - rect[0]).max(1.0);
    let height = (rect[3] - rect[1]).max(1.0);

    // Auto size: fill about two thirds of the box height, and shrink if the
    // text would not otherwise fit across it.
    let size = appearance.size.unwrap_or_else(|| {
        let by_height = (height * 0.66).clamp(4.0, 24.0);
        #[expect(clippy::cast_precision_loss, reason = "field values are short")]
        let characters = value.chars().count().max(1) as f32;
        // 0.5em is the average advance across the base-14 faces.
        let by_width = width / (characters * 0.5);
        by_height.min(by_width).max(4.0)
    });

    let mut da = appearance.da.clone();
    if appearance.size.is_none() {
        // Replace the `0 Tf` with the size actually used, so what is drawn and
        // what is declared agree.
        da = da.replace("0 Tf", &format!("{size:.2} Tf"));
    }

    // Vertically centred on the box, with a small inset from the left edge.
    let baseline = (height - size * 0.72) / 2.0 + size * 0.15;
    let text = winansi::encode_literal(value).unwrap_or_default();

    let content =
        format!("/Tx BMC\nq\nBT\n{da}\n1 0 0 1 2 {baseline:.2} Tm\n({text}) Tj\nET\nQ\nEMC\n");

    Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            // The stream draws in its own space, starting at the origin.
            "BBox" => Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(width),
                Object::Real(height),
            ]),
        },
        content.into_bytes(),
    )
}

fn attach_resources(pdf: &mut Pdf, stream_id: ObjectId, appearance: &Appearance) {
    let Some(resources) = appearance.resources.clone() else {
        return;
    };
    if let Ok(Object::Stream(stream)) = pdf.raw_mut().get_object_mut(stream_id) {
        stream.dict.set("Resources", resources);
    }
}

fn widget_rect(pdf: &Pdf, widget: ObjectId) -> Option<[f32; 4]> {
    let values = pdf
        .raw()
        .get_dictionary(widget)
        .ok()?
        .get(b"Rect")
        .ok()?
        .as_array()
        .ok()?;
    let numbers: Vec<f32> = values
        .iter()
        .filter_map(|value| value.as_float().ok())
        .collect();
    match numbers[..] {
        [x0, y0, x1, y1] => Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)]),
        _ => None,
    }
}

/// Ask readers to rebuild appearances themselves as well.
fn need_appearances(pdf: &mut Pdf, value: bool) {
    let Some(id) = model::acroform_id(pdf) else {
        return;
    };
    if let Ok(form) = pdf.raw_mut().get_dictionary_mut(id) {
        form.set("NeedAppearances", Object::Boolean(value));
    }
}

/// Draw every field into its page and remove the form.
///
/// After this the document has no fields at all: what was typed is part of the
/// page, and nobody can change it by opening the file in a reader. That is the
/// point — but it is one-way, which is why it is a separate operation rather
/// than something filling does on the way past.
pub fn flatten(pdf: &mut Pdf) -> Result<Report> {
    let located = model::locate(pdf);
    if located.is_empty() {
        return Ok(Report::default());
    }

    let mut report = Report::default();
    let page_ids = pdf.page_ids().to_vec();

    for found in &located {
        for widget in &found.widgets {
            let Some(rect) = widget_rect(pdf, *widget) else {
                continue;
            };
            let Some(appearance) = current_appearance(pdf, *widget) else {
                continue;
            };
            let Some(page_id) = widget_page(pdf, *widget, &page_ids) else {
                continue;
            };

            draw_into_page(pdf, page_id, appearance, rect);
        }
        report.flattened += 1;
    }

    // Every widget goes, then the form itself: a document with an /AcroForm and
    // no fields makes some readers show an empty form bar.
    let widgets: Vec<ObjectId> = located
        .iter()
        .flat_map(|found| found.widgets.clone())
        .collect();
    remove_widgets(pdf, &page_ids, &widgets);

    if let Ok(catalog) = pdf.raw_mut().catalog_mut() {
        catalog.remove(b"AcroForm");
    }

    tracing::info!(fields = report.flattened, "form flattened");
    Ok(report)
}

/// The appearance stream a widget currently shows.
fn current_appearance(pdf: &Pdf, widget: ObjectId) -> Option<ObjectId> {
    let dict = pdf.raw().get_dictionary(widget).ok()?;
    let appearance = match dict.get(b"AP").ok()? {
        Object::Reference(id) => pdf.raw().get_dictionary(*id).ok()?.clone(),
        Object::Dictionary(dict) => dict.clone(),
        _ => return None,
    };

    match appearance.get(b"N").ok()? {
        // One stream: that is what it draws.
        Object::Reference(id) => Some(*id),
        // Several, keyed by state: the one `/AS` selects.
        Object::Dictionary(states) => {
            let state = dict.get(b"AS").and_then(Object::as_name).ok()?;
            states.get(state).ok()?.as_reference().ok()
        }
        _ => None,
    }
}

/// Append a widget's appearance to the page's content.
fn draw_into_page(pdf: &mut Pdf, page_id: ObjectId, appearance: ObjectId, rect: [f32; 4]) {
    // A name unlikely to collide with anything the page already uses.
    let name = format!("YPDFFF{}_{}", appearance.0, appearance.1);

    let content = format!(
        "q\n1 0 0 1 {:.2} {:.2} cm\n/{name} Do\nQ\n",
        rect[0], rect[1]
    );
    let stream_id = pdf
        .raw_mut()
        .add_object(Stream::new(dictionary! {}, content.into_bytes()));

    add_xobject(pdf, page_id, &name, appearance);
    append_content(pdf, page_id, stream_id);
}

fn add_xobject(pdf: &mut Pdf, page_id: ObjectId, name: &str, target: ObjectId) {
    let resources_ref = pdf
        .raw()
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Resources").ok())
        .and_then(|value| value.as_reference().ok());

    let install = |resources: &mut Dictionary| {
        if !matches!(resources.get(b"XObject"), Ok(Object::Dictionary(_))) {
            resources.set("XObject", Object::Dictionary(Dictionary::new()));
        }
        if let Ok(Object::Dictionary(xobjects)) = resources.get_mut(b"XObject") {
            xobjects.set(name.to_string(), Object::Reference(target));
        }
    };

    if let Some(id) = resources_ref {
        if let Ok(resources) = pdf.raw_mut().get_dictionary_mut(id) {
            install(resources);
        }
        return;
    }

    if let Ok(page) = pdf.raw_mut().get_dictionary_mut(page_id) {
        match page.get_mut(b"Resources") {
            Ok(Object::Dictionary(resources)) => install(resources),
            _ => {
                let mut resources = Dictionary::new();
                install(&mut resources);
                page.set("Resources", Object::Dictionary(resources));
            }
        }
    }
}

fn append_content(pdf: &mut Pdf, page_id: ObjectId, stream_id: ObjectId) {
    let existing = pdf
        .raw()
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Contents").ok())
        .cloned();

    let mut contents = match existing {
        Some(Object::Array(items)) => items,
        Some(Object::Reference(id)) => vec![Object::Reference(id)],
        _ => Vec::new(),
    };
    contents.push(Object::Reference(stream_id));

    if let Ok(page) = pdf.raw_mut().get_dictionary_mut(page_id) {
        page.set("Contents", Object::Array(contents));
    }
}

fn widget_page(pdf: &Pdf, widget: ObjectId, page_ids: &[ObjectId]) -> Option<ObjectId> {
    if let Ok(page) = pdf
        .raw()
        .get_dictionary(widget)
        .ok()?
        .get(b"P")
        .and_then(Object::as_reference)
        && page_ids.contains(&page)
    {
        return Some(page);
    }

    page_ids.iter().copied().find(|page_id| {
        pdf.raw()
            .get_dictionary(*page_id)
            .ok()
            .and_then(|page| page.get(b"Annots").ok())
            .and_then(|value| value.as_array().ok())
            .is_some_and(|annots| {
                annots
                    .iter()
                    .any(|item| item.as_reference().ok() == Some(widget))
            })
    })
}

fn remove_widgets(pdf: &mut Pdf, page_ids: &[ObjectId], widgets: &[ObjectId]) {
    for page_id in page_ids {
        let annots = pdf
            .raw()
            .get_dictionary(*page_id)
            .ok()
            .and_then(|page| page.get(b"Annots").ok())
            .and_then(|value| value.as_array().ok())
            .cloned();

        let Some(annots) = annots else { continue };
        let kept: Vec<Object> = annots
            .into_iter()
            .filter(|item| {
                item.as_reference()
                    .ok()
                    .is_none_or(|id| !widgets.contains(&id))
            })
            .collect();

        if let Ok(page) = pdf.raw_mut().get_dictionary_mut(*page_id) {
            if kept.is_empty() {
                page.remove(b"Annots");
            } else {
                page.set("Annots", Object::Array(kept));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_says_when_a_name_did_not_exist() {
        // A spelling mistake in a field name would otherwise look exactly like
        // a successful fill.
        let report = Report {
            filled: 2,
            unknown: vec!["adress".into()],
            ..Report::default()
        };
        assert!(!report.is_complete());
        assert!(report.to_human().contains("No such field: adress"));
    }

    #[test]
    fn a_report_says_which_fields_it_would_not_write() {
        let report = Report {
            refused: vec![(
                "signature".into(),
                "a signature field is not something to type into",
            )],
            ..Report::default()
        };
        assert!(report.to_human().contains("Left alone: signature"));
    }

    #[test]
    fn nothing_to_do_says_so_rather_than_printing_nothing() {
        assert_eq!(Report::default().to_human(), "Nothing to do.\n");
    }
}
