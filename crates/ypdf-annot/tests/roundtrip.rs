//! Annotations written, read back, and removed again.
//!
//! The property that matters most is the last one: marking a document up must
//! be undoable by deletion, because that is what makes it safe to send a file
//! for review and get a clean copy back.

// Integration tests compile as their own crate, so `cfg(test)` is not set and
// the clippy.toml allowance for tests does not reach here.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use ypdf_annot::{Annotation, Colour, Shape, add, list, remove_where};
use ypdf_doc::Pdf;

fn document() -> Pdf {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/many-pages.pdf");
    Pdf::open(path).expect("the fixture opens")
}

fn round_trip(pdf: &mut Pdf) -> Pdf {
    let bytes = pdf.to_bytes().expect("serializes");
    Pdf::from_bytes(&bytes).expect("re-opens")
}

fn one_of_each() -> Vec<Annotation> {
    vec![
        Annotation::new(
            1,
            Shape::Highlight {
                quads: vec![[72.0, 700.0, 300.0, 714.0]],
            },
        )
        .with_contents("check this figure")
        .with_author("Ada"),
        Annotation::new(
            1,
            Shape::Underline {
                quads: vec![[72.0, 680.0, 260.0, 694.0]],
            },
        ),
        Annotation::new(
            1,
            Shape::StrikeOut {
                quads: vec![[72.0, 660.0, 260.0, 674.0]],
            },
        ),
        Annotation::new(2, Shape::Note { at: [100.0, 700.0] }).with_contents("a comment"),
        Annotation::new(
            2,
            Shape::TextBox {
                rect: [100.0, 600.0, 320.0, 650.0],
                size: None,
            },
        )
        .with_contents("a box of text"),
        Annotation::new(
            3,
            Shape::Ink {
                strokes: vec![vec![[100.0, 500.0], [140.0, 540.0], [180.0, 500.0]]],
            },
        ),
        Annotation::new(
            3,
            Shape::Rectangle {
                rect: [100.0, 300.0, 300.0, 400.0],
                fill: None,
            },
        ),
        Annotation::new(
            3,
            Shape::Ellipse {
                rect: [320.0, 300.0, 500.0, 400.0],
                fill: Some(Colour(0.9, 0.95, 1.0)),
            },
        ),
        Annotation::new(
            4,
            Shape::Line {
                from: [100.0, 200.0],
                to: [300.0, 260.0],
                arrow: true,
            },
        ),
        Annotation::new(
            4,
            Shape::Stamp {
                rect: [100.0, 100.0, 260.0, 150.0],
                text: "APPROVED".into(),
            },
        ),
    ]
}

#[test]
fn every_kind_is_written_and_read_back_on_the_right_page() {
    let mut pdf = document();
    for annotation in one_of_each() {
        add(&mut pdf, &annotation).expect("adds");
    }

    let reopened = round_trip(&mut pdf);
    let found = list(&reopened);

    assert_eq!(found.len(), 10, "{found:#?}");
    let labels: Vec<&str> = found.iter().map(|a| a.shape.label()).collect();
    for expected in [
        "highlight",
        "underline",
        "strike-through",
        "note",
        "text box",
        "drawing",
        "rectangle",
        "ellipse",
        "line",
        "stamp",
    ] {
        assert!(
            labels.contains(&expected),
            "{expected} is missing: {labels:?}"
        );
    }

    assert_eq!(found.iter().filter(|a| a.page == 1).count(), 3);
    assert_eq!(found.iter().filter(|a| a.page == 4).count(), 2);
}

#[test]
fn a_comment_and_its_author_survive() {
    let mut pdf = document();
    add(
        &mut pdf,
        &Annotation::new(
            1,
            Shape::Highlight {
                quads: vec![[72.0, 700.0, 300.0, 714.0]],
            },
        )
        .with_contents("check this figure")
        .with_author("Ada Lovelace"),
    )
    .expect("adds");

    let reopened = round_trip(&mut pdf);
    let found = &list(&reopened)[0];
    assert_eq!(found.contents, "check this figure");
    assert_eq!(found.author, "Ada Lovelace");
    assert!(found.modified.is_some(), "a timestamp should be written");
}

#[test]
fn a_comment_in_a_script_the_font_cannot_draw_is_still_attached() {
    // The drawing falls back to nothing; the text itself is what a reader shows
    // in its comment panel, and that must survive.
    let mut pdf = document();
    add(
        &mut pdf,
        &Annotation::new(1, Shape::Note { at: [100.0, 700.0] }).with_contents("機密事項"),
    )
    .expect("adds");

    let reopened = round_trip(&mut pdf);
    assert_eq!(list(&reopened)[0].contents, "機密事項");
}

#[test]
fn colour_and_opacity_come_back() {
    let mut pdf = document();
    add(
        &mut pdf,
        &Annotation::new(
            1,
            Shape::Rectangle {
                rect: [100.0, 300.0, 300.0, 400.0],
                fill: None,
            },
        )
        .with_colour(Colour::parse("#ff0000").expect("a colour"))
        .with_opacity(0.4)
        .with_width(3.0),
    )
    .expect("adds");

    let reopened = round_trip(&mut pdf);
    let found = &list(&reopened)[0];
    assert_eq!(found.colour.to_hex(), "#ff0000");
    assert!((found.opacity - 0.4).abs() < 0.01, "{}", found.opacity);
    assert!((found.width - 3.0).abs() < 0.01, "{}", found.width);
}

#[test]
fn every_annotation_carries_an_appearance_and_prints() {
    // Without `/AP` a reader draws it however it likes; without the print flag
    // it vanishes on paper, which is how review comments get lost.
    let mut pdf = document();
    for annotation in one_of_each() {
        add(&mut pdf, &annotation).expect("adds");
    }

    let reopened = round_trip(&mut pdf);
    let mut checked = 0;

    for page_id in reopened.page_ids() {
        let Ok(page) = reopened.raw().get_dictionary(*page_id) else {
            continue;
        };
        let Ok(annots) = page.get(b"Annots").and_then(lopdf::Object::as_array) else {
            continue;
        };
        for item in annots {
            let id = item.as_reference().expect("indirect");
            let dict = reopened.raw().get_dictionary(id).expect("a dictionary");

            assert!(dict.get(b"AP").is_ok(), "no appearance: {dict:?}");
            let flags = dict
                .get(b"F")
                .and_then(lopdf::Object::as_i64)
                .expect("flags");
            assert_eq!(flags & 4, 4, "not marked printable: {dict:?}");
            checked += 1;
        }
    }

    assert_eq!(checked, 10);
}

#[test]
fn a_highlights_quad_points_are_written_in_the_order_readers_expect() {
    // Top-left, top-right, bottom-left, bottom-right. Getting this wrong draws
    // an hourglass, or nothing.
    let mut pdf = document();
    add(
        &mut pdf,
        &Annotation::new(
            1,
            Shape::Highlight {
                quads: vec![[72.0, 700.0, 300.0, 714.0]],
            },
        ),
    )
    .expect("adds");

    let reopened = round_trip(&mut pdf);
    let page_id = reopened.page_ids()[0];
    let annots = reopened
        .raw()
        .get_dictionary(page_id)
        .expect("page")
        .get(b"Annots")
        .and_then(lopdf::Object::as_array)
        .expect("annotations");
    let dict = reopened
        .raw()
        .get_dictionary(annots[0].as_reference().expect("indirect"))
        .expect("a dictionary");

    let points: Vec<f32> = dict
        .get(b"QuadPoints")
        .and_then(lopdf::Object::as_array)
        .expect("quad points")
        .iter()
        .filter_map(|value| value.as_float().ok())
        .collect();

    assert_eq!(
        points,
        vec![72.0, 714.0, 300.0, 714.0, 72.0, 700.0, 300.0, 700.0]
    );
}

#[test]
fn marking_up_a_document_can_be_undone_by_deleting() {
    // The property that makes it safe to send a file out for review.
    let mut pdf = document();
    let before = pdf.to_bytes().expect("serializes").len();

    for annotation in one_of_each() {
        add(&mut pdf, &annotation).expect("adds");
    }
    assert_eq!(list(&pdf).len(), 10);

    let removed = remove_where(&mut pdf, |_| true);
    assert_eq!(removed, 10);

    let reopened = round_trip(&mut pdf);
    assert!(list(&reopened).is_empty());
    assert_eq!(reopened.page_count(), 12);

    // And the file is back to roughly what it was: the objects are unreferenced
    // and pruned on save.
    let size = {
        let mut copy = round_trip(&mut pdf);
        copy.to_bytes().expect("serializes").len()
    };
    assert!(
        size < before + 1024,
        "removal left {size} bytes against an original {before}"
    );
}

#[test]
fn annotations_can_be_removed_by_author() {
    // What a reviewer's comments being cleared out actually looks like.
    let mut pdf = document();
    add(
        &mut pdf,
        &Annotation::new(1, Shape::Note { at: [100.0, 700.0] }).with_author("Ada"),
    )
    .expect("adds");
    add(
        &mut pdf,
        &Annotation::new(1, Shape::Note { at: [140.0, 700.0] }).with_author("Grace"),
    )
    .expect("adds");

    let removed = remove_where(&mut pdf, |annotation| annotation.author == "Ada");
    assert_eq!(removed, 1);

    let reopened = round_trip(&mut pdf);
    let found = list(&reopened);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].author, "Grace");
}

#[test]
fn form_fields_and_links_are_not_listed_as_annotations() {
    // A widget is a form field and a link is navigation; neither is a comment,
    // and listing them here would make "clear all annotations" delete the form.
    let form = Pdf::open(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/form.pdf"),
    )
    .expect("opens");
    assert!(list(&form).is_empty());

    let linked = Pdf::open(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/linked.pdf"),
    )
    .expect("opens");
    assert!(list(&linked).is_empty());
}

#[test]
fn clearing_annotations_leaves_a_form_alone() {
    let mut form = Pdf::open(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/form.pdf"),
    )
    .expect("opens");

    add(
        &mut form,
        &Annotation::new(1, Shape::Note { at: [400.0, 700.0] }).with_contents("looks fine"),
    )
    .expect("adds");

    assert_eq!(remove_where(&mut form, |_| true), 1);

    let reopened = round_trip(&mut form);
    assert_eq!(
        ypdf_forms::read(&reopened).len(),
        5,
        "the form fields must still be there"
    );
}
