//! Filling a real form, and checking the file agrees with itself afterwards.
//!
//! The fixture is built to catch the mistakes that are easy to make: its
//! checkbox turns on with `/On` rather than the `/Yes` everyone assumes, its
//! radio group is one field with two widgets, and it has a signature field that
//! must come through untouched.

// Integration tests compile as their own crate, so `cfg(test)` is not set and
// the clippy.toml allowance for tests does not reach here.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use lopdf::Object;
use ypdf_doc::Pdf;
use ypdf_forms::{Kind, clear, fill, flatten, read};

fn form() -> Pdf {
    let path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/form.pdf");
    Pdf::open(path).expect("the fixture opens")
}

fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

/// Re-open through bytes, so every assertion is about what was written.
fn round_trip(pdf: &mut Pdf) -> Pdf {
    let bytes = pdf.to_bytes().expect("serializes");
    Pdf::from_bytes(&bytes).expect("re-opens")
}

#[test]
fn every_kind_of_field_is_found_with_its_type() {
    let fields = read(&form());

    let by_name: BTreeMap<&str, &ypdf_forms::Field> = fields
        .iter()
        .map(|field| (field.name.as_str(), field))
        .collect();

    assert_eq!(by_name["name"].kind, Kind::Text);
    assert_eq!(by_name["subscribe"].kind, Kind::Checkbox);
    assert_eq!(
        by_name["plan"].kind,
        Kind::Radio,
        "a radio flag makes it one"
    );
    assert_eq!(by_name["country"].kind, Kind::Choice);
    assert_eq!(by_name["sign here"].kind, Kind::Signature);
    assert_eq!(fields.len(), 5);
}

#[test]
fn a_dropdowns_choices_are_listed() {
    let fields = read(&form());
    let country = fields
        .iter()
        .find(|field| field.name == "country")
        .expect("the field");
    assert_eq!(country.options, vec!["Malaysia", "Singapore", "Ireland"]);
}

#[test]
fn a_radio_groups_options_come_from_its_buttons() {
    let fields = read(&form());
    let plan = fields
        .iter()
        .find(|field| field.name == "plan")
        .expect("the field");
    assert_eq!(plan.options, vec!["basic", "pro"]);
    assert_eq!(plan.pages, vec![1]);
}

#[test]
fn a_filled_text_field_carries_both_a_value_and_an_appearance() {
    // Writing /V without rebuilding the appearance produces a file that looks
    // filled in one reader, empty in another, and prints blank.
    let mut pdf = form();
    let report = fill(&mut pdf, &values(&[("name", "Ada Lovelace")])).expect("fills");
    assert!(report.is_complete(), "{report:?}");
    assert_eq!(report.filled, 1);

    let reopened = round_trip(&mut pdf);
    let field = read(&reopened)
        .into_iter()
        .find(|field| field.name == "name")
        .expect("the field");
    assert_eq!(field.value, "Ada Lovelace");

    // The appearance stream has to contain the text a reader will draw.
    let drawn = appearance_text(&reopened, "name");
    assert!(drawn.contains("Ada Lovelace"), "{drawn}");
}

/// The appearance stream of a field's first widget, as text.
fn appearance_text(pdf: &Pdf, name: &str) -> String {
    let mut out = String::new();
    for object in pdf.raw().objects.values() {
        let Ok(dict) = object.as_dict() else { continue };
        let matches = dict
            .get(b"T")
            .ok()
            .and_then(|value| match value {
                Object::String(bytes, _) => Some(ypdf_doc::decode_text(bytes)),
                _ => None,
            })
            .is_some_and(|found| found == name);
        if !matches {
            continue;
        }

        let Ok(appearance) = dict.get(b"AP").and_then(Object::as_dict) else {
            continue;
        };
        let Ok(stream_id) = appearance.get(b"N").and_then(Object::as_reference) else {
            continue;
        };
        if let Ok(stream) = pdf.raw().get_object(stream_id).and_then(Object::as_stream) {
            out.push_str(&String::from_utf8_lossy(&stream.content));
        }
    }
    out
}

#[test]
fn a_checkbox_is_turned_on_with_the_state_its_own_appearance_names() {
    // The fixture's on-state is `/On`. Writing `/Yes` would give a box that is
    // checked in the data and blank on the page.
    let mut pdf = form();
    fill(&mut pdf, &values(&[("subscribe", "yes")])).expect("fills");

    let reopened = round_trip(&mut pdf);
    let field = read(&reopened)
        .into_iter()
        .find(|field| field.name == "subscribe")
        .expect("the field");
    assert_eq!(field.value, "On");

    let widget = widget_of(&reopened, "subscribe");
    assert_eq!(widget, Some("On".to_string()), "/AS must agree with /V");
}

/// The `/AS` of a field's widget.
fn widget_of(pdf: &Pdf, name: &str) -> Option<String> {
    for object in pdf.raw().objects.values() {
        let Ok(dict) = object.as_dict() else { continue };
        let matches = dict
            .get(b"T")
            .ok()
            .and_then(|value| match value {
                Object::String(bytes, _) => Some(ypdf_doc::decode_text(bytes)),
                _ => None,
            })
            .is_some_and(|found| found == name);
        if matches && let Ok(state) = dict.get(b"AS").and_then(Object::as_name) {
            return Some(String::from_utf8_lossy(state).into_owned());
        }
    }
    None
}

#[test]
fn one_button_of_a_radio_group_goes_on_and_the_others_go_off() {
    let mut pdf = form();
    fill(&mut pdf, &values(&[("plan", "pro")])).expect("fills");

    let reopened = round_trip(&mut pdf);
    let field = read(&reopened)
        .into_iter()
        .find(|field| field.name == "plan")
        .expect("the field");
    assert_eq!(field.value, "pro");

    // Exactly one widget shows an on-state.
    let states: Vec<String> = reopened
        .raw()
        .objects
        .values()
        .filter_map(|object| {
            let dict = object.as_dict().ok()?;
            let parent = dict.get(b"Parent").ok()?;
            parent.as_reference().ok()?;
            let state = dict.get(b"AS").and_then(Object::as_name).ok()?;
            Some(String::from_utf8_lossy(state).into_owned())
        })
        .collect();

    assert_eq!(states.iter().filter(|state| *state != "Off").count(), 1);
    assert!(states.contains(&"pro".to_string()), "{states:?}");
}

#[test]
fn a_signature_field_is_refused_rather_than_filled() {
    // Filling one would produce a document that claims to be signed.
    let mut pdf = form();
    let report = fill(&mut pdf, &values(&[("sign here", "Ada")])).expect("runs");

    assert_eq!(report.filled, 0);
    assert!(!report.is_complete());
    assert!(report.to_human().contains("sign here"), "{report:?}");

    let reopened = round_trip(&mut pdf);
    let field = read(&reopened)
        .into_iter()
        .find(|field| field.name == "sign here")
        .expect("the field");
    assert!(field.value.is_empty());
}

#[test]
fn a_name_that_does_not_exist_is_reported() {
    // A typo would otherwise look exactly like a successful fill.
    let mut pdf = form();
    let report = fill(&mut pdf, &values(&[("nmae", "Ada")])).expect("runs");

    assert_eq!(report.filled, 0);
    assert_eq!(report.unknown, vec!["nmae".to_string()]);
    assert!(!report.is_complete());
}

#[test]
fn clearing_empties_every_value_including_the_boxes() {
    let mut pdf = form();
    fill(
        &mut pdf,
        &values(&[("name", "Ada"), ("subscribe", "yes"), ("plan", "basic")]),
    )
    .expect("fills");

    let report = clear(&mut pdf).expect("clears");
    assert_eq!(report.cleared, 3);

    let reopened = round_trip(&mut pdf);
    for field in read(&reopened) {
        assert!(field.value.is_empty(), "{field:?}");
    }
    assert_eq!(widget_of(&reopened, "subscribe").as_deref(), Some("Off"));
}

#[test]
fn flattening_leaves_the_words_on_the_page_and_no_form_at_all() {
    let mut pdf = form();
    fill(&mut pdf, &values(&[("name", "Ada Lovelace")])).expect("fills");
    let report = flatten(&mut pdf).expect("flattens");
    assert_eq!(report.flattened, 5);

    let reopened = round_trip(&mut pdf);

    // No fields, and no form dictionary either: a document with an /AcroForm
    // and nothing in it makes some readers show an empty form bar.
    assert!(read(&reopened).is_empty());
    assert!(!ypdf_forms::has_form(&reopened));

    // The value is now drawn by the page.
    let page_id = reopened.page_ids()[0];
    let content = String::from_utf8_lossy(&reopened.raw().get_page_content(page_id)).into_owned();
    assert!(
        content.contains("Do"),
        "the appearance must be drawn by the page: {content}"
    );
}

#[test]
fn a_flattened_form_no_longer_has_widget_annotations() {
    let mut pdf = form();
    flatten(&mut pdf).expect("flattens");

    let reopened = round_trip(&mut pdf);
    let page_id = reopened.page_ids()[0];
    let annots = reopened
        .raw()
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Annots").ok())
        .and_then(|value| value.as_array().ok())
        .map_or(0, Vec::len);

    assert_eq!(annots, 0, "the fields must not still be editable");
}

#[test]
fn form_data_round_trips_through_every_format() {
    let mut pdf = form();
    fill(
        &mut pdf,
        &values(&[("name", "Ada Lovelace"), ("country", "Ireland")]),
    )
    .expect("fills");

    let exported = ypdf_forms::values_of(&read(&pdf));
    for format in [
        ypdf_forms::Format::Json,
        ypdf_forms::Format::Fdf,
        ypdf_forms::Format::Xfdf,
    ] {
        let text = ypdf_forms::export(&exported, format).expect("writes");
        let back = ypdf_forms::import(&text, format).expect("reads");
        assert_eq!(back, exported, "{format:?}");
    }
}

#[test]
fn importing_into_a_blank_copy_reproduces_the_filled_one() {
    // The point of export: fill one form, and fill a thousand the same way.
    let mut filled = form();
    fill(
        &mut filled,
        &values(&[("name", "Ada Lovelace"), ("subscribe", "yes")]),
    )
    .expect("fills");
    let data = ypdf_forms::values_of(&read(&filled));

    let mut blank = form();
    let report = fill(&mut blank, &data).expect("fills");
    assert!(report.is_complete(), "{report:?}");

    let a = read(&round_trip(&mut filled));
    let b = read(&round_trip(&mut blank));
    assert_eq!(a, b);
}
