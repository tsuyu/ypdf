//! The question redaction exists to answer: **is it gone?**
//!
//! Not "is it hidden", not "does the viewer stop showing it". Gone — absent
//! from the bytes of the saved file, and from every stream inside it. Every
//! redaction scandal of the last twenty years has been a document that passed
//! the first two tests and failed this one, so this is the one that is checked
//! first and hardest.

// Integration tests compile as their own crate, so `cfg(test)` is not set and
// the clippy.toml allowance for tests does not reach here.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use lopdf::{Document, Object, Stream, dictionary};
use ypdf_doc::Pdf;
use ypdf_redact::{Rect, Redaction, Settings, redact};

/// A one-page document reading `PUBLIC SECRET123 PUBLIC` at a known position.
fn document() -> Pdf {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });

    // Three lines, so there is text above and below the redacted one too.
    let content = b"BT /F1 12 Tf 100 700 Td (PUBLIC HEADER) Tj ET\n\
                    BT /F1 12 Tf 100 650 Td (PUBLIC SECRET123 PUBLIC) Tj ET\n\
                    BT /F1 12 Tf 100 600 Td (PUBLIC FOOTER) Tj ET"
        .to_vec();
    let content_id = doc.add_object(Stream::new(dictionary! {}, content));

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        },
        "Contents" => content_id,
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
    Pdf::from_bytes(&bytes).expect("re-opens")
}

/// Does `needle` appear anywhere in the file — raw, or inside any stream?
///
/// A compressed content stream would hide the text from a plain search while
/// leaving it perfectly recoverable, which is the failure this is looking for.
fn appears_anywhere(bytes: &[u8], needle: &str) -> bool {
    if contains(bytes, needle.as_bytes()) {
        return true;
    }

    let document = Document::load_mem(bytes).expect("re-opens");
    document.objects.values().any(|object| {
        object.as_stream().is_ok_and(|stream| {
            if contains(&stream.content, needle.as_bytes()) {
                return true;
            }
            stream
                .decompressed_content()
                .is_ok_and(|content| contains(&content, needle.as_bytes()))
        })
    })
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// The line sits at y=650 with 12pt text; "PUBLIC " is about 45pt wide, so
/// "SECRET123" runs from roughly x=145 to x=210.
fn secret_rect() -> Rect {
    Rect::new(144.0, 644.0, 212.0, 662.0)
}

#[test]
fn the_redacted_text_is_not_in_the_file_at_all() {
    let mut pdf = document();
    assert!(
        appears_anywhere(&pdf.to_bytes().expect("serializes"), "SECRET123"),
        "the fixture should contain the secret before redaction"
    );

    let report = redact(
        &mut pdf,
        &[Redaction {
            page: 1,
            rect: secret_rect(),
        }],
        &Settings::default(),
    )
    .expect("redacts");

    assert!(report.glyphs >= 9, "{report:?}");

    let bytes = pdf.to_bytes().expect("serializes");
    assert!(
        !appears_anywhere(&bytes, "SECRET123"),
        "the secret survived somewhere in the file"
    );
}

#[test]
fn the_rest_of_the_document_is_left_alone() {
    let mut pdf = document();
    redact(
        &mut pdf,
        &[Redaction {
            page: 1,
            rect: secret_rect(),
        }],
        &Settings::default(),
    )
    .expect("redacts");

    let bytes = pdf.to_bytes().expect("serializes");
    let text = Document::load_mem(&bytes)
        .expect("re-opens")
        .extract_text(&[1])
        .expect("extracts");

    assert!(text.contains("HEADER"), "the line above went too: {text:?}");
    assert!(text.contains("FOOTER"), "the line below went too: {text:?}");
    assert!(!text.contains("SECRET123"), "{text:?}");
    // The words on the redacted line either side of the secret survive.
    assert!(text.contains("PUBLIC"), "{text:?}");
}

#[test]
fn extraction_and_search_can_no_longer_find_it() {
    // The user-visible half of the promise: copy and find come up empty.
    let mut pdf = document();
    redact(
        &mut pdf,
        &[Redaction {
            page: 1,
            rect: secret_rect(),
        }],
        &Settings::default(),
    )
    .expect("redacts");

    let bytes = pdf.to_bytes().expect("serializes");
    let text = Document::load_mem(&bytes)
        .expect("re-opens")
        .extract_text(&[1])
        .expect("extracts");
    assert!(!text.to_uppercase().contains("SECRET"), "{text:?}");
}

#[test]
fn the_document_still_opens_and_keeps_its_pages() {
    let mut pdf = document();
    redact(
        &mut pdf,
        &[Redaction {
            page: 1,
            rect: secret_rect(),
        }],
        &Settings::default(),
    )
    .expect("redacts");

    let bytes = pdf.to_bytes().expect("serializes");
    let reopened = Pdf::from_bytes(&bytes).expect("re-opens");
    assert_eq!(reopened.page_count(), 1);
}

#[test]
fn turning_the_cover_off_still_removes_the_text() {
    // The black box is not the redaction. Without it, the text must still be
    // gone — if this test ever fails, the crate has become the thing spec §11
    // forbids.
    let mut pdf = document();
    redact(
        &mut pdf,
        &[Redaction {
            page: 1,
            rect: secret_rect(),
        }],
        &Settings {
            draw_cover: false,
            ..Settings::default()
        },
    )
    .expect("redacts");

    let bytes = pdf.to_bytes().expect("serializes");
    assert!(!appears_anywhere(&bytes, "SECRET123"));
}

#[test]
fn a_redaction_that_covers_nothing_removes_nothing() {
    let mut pdf = document();
    let report = redact(
        &mut pdf,
        &[Redaction {
            page: 1,
            rect: Rect::new(10.0, 10.0, 50.0, 50.0),
        }],
        &Settings::default(),
    )
    .expect("runs");

    assert_eq!(report.glyphs, 0);
    let bytes = pdf.to_bytes().expect("serializes");
    assert!(
        appears_anywhere(&bytes, "SECRET123"),
        "nothing was asked for"
    );
}

#[test]
fn an_annotation_over_the_area_is_deleted_with_it() {
    // A link carries its own text and its own target, neither of which is in
    // the content stream.
    let mut pdf = Pdf::open(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/linked.pdf"),
    )
    .expect("opens");

    let page_id = pdf.page_ids()[0];
    let before = pdf
        .raw()
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Annots").ok())
        .and_then(|value| value.as_array().ok())
        .map_or(0, Vec::len);
    assert!(before > 0, "the fixture is expected to have a link");

    let report = redact(
        &mut pdf,
        &[Redaction {
            page: 1,
            // The whole page: every annotation on it overlaps.
            rect: Rect::new(0.0, 0.0, 612.0, 792.0),
        }],
        &Settings::default(),
    )
    .expect("redacts");

    assert_eq!(report.annotations, before);
}

#[test]
fn redacting_a_page_leaves_the_other_pages_untouched() {
    let mut pdf = Pdf::open(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/many-pages.pdf"),
    )
    .expect("opens");

    let page_two_before = pdf.raw().get_page_content(pdf.page_ids()[1]);

    redact(
        &mut pdf,
        &[Redaction {
            page: 1,
            rect: Rect::new(0.0, 0.0, 612.0, 792.0),
        }],
        &Settings::default(),
    )
    .expect("redacts");

    assert_eq!(
        pdf.raw().get_page_content(pdf.page_ids()[1]),
        page_two_before,
        "page two changed"
    );
}
