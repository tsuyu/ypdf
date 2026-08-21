//! Building the invisible text layer (spec §7).
//!
//! The scan itself is never touched. What OCR adds is a second content stream
//! drawing the recognized words in **text render mode 3**, which draws nothing
//! at all: the page looks exactly as it did, and the words are there to be
//! searched, selected, and copied.
//!
//! That is the whole contract, and it is why the layer is appended rather than
//! substituted. If the recognition is wrong, the document still shows what it
//! always showed — a wrong text layer costs a bad search hit, not a lost page.
//!
//! Two honest limitations, both reported rather than hidden:
//!
//! * The font is Helvetica with WinAnsi encoding, so a word containing
//!   characters that encoding cannot represent — Chinese, Arabic, Devanagari —
//!   is **skipped and counted**. Placing it in a font that cannot encode it
//!   would produce a text layer that extracts as mojibake, which is worse than
//!   an absence, because it looks like data.
//! * Word width is matched by horizontal scaling against an approximate
//!   Helvetica advance. The glyphs are invisible, so this only affects where a
//!   selection rectangle lands, and being a few percent out there is invisible
//!   too.

use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};

use ypdf_doc::winansi;

use crate::tsv::Word;

/// Name given to the font this layer adds.
///
/// Deliberately unlikely to collide with a name the page already uses.
const FONT_NAME: &[u8] = b"YPDFOCR";

/// Rough average advance of Helvetica, in ems.
///
/// Only used to pick a horizontal scale, and the text is invisible.
const AVERAGE_ADVANCE: f32 = 0.5;

/// What happened when a page's text layer was written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayerReport {
    /// Words placed in the layer.
    pub placed: usize,
    /// Words dropped because the font cannot encode them.
    pub unencodable: usize,
}

/// Add an invisible text layer for `words` to `page`.
///
/// `image_size_px` is the raster the words were recognized on, and
/// `page_size_pt` is the page they belong to; the two together are the only
/// thing that maps pixels onto the page.
pub fn add_text_layer(
    document: &mut Document,
    page_id: ObjectId,
    words: &[Word],
    image_size_px: (u32, u32),
    page_size_pt: (f32, f32),
) -> LayerReport {
    let mut report = LayerReport::default();
    if words.is_empty() || image_size_px.0 == 0 || image_size_px.1 == 0 {
        return report;
    }

    let scale_x = page_size_pt.0 / f32::from_u32(image_size_px.0);
    let scale_y = page_size_pt.1 / f32::from_u32(image_size_px.1);

    let mut content = String::from("q\nBT\n3 Tr\n");

    for word in words {
        let Some(encoded) = winansi::encode_literal(&word.text) else {
            report.unencodable += 1;
            continue;
        };

        let width_pt = f32::from_i32(word.width) * scale_x;
        let height_pt = f32::from_i32(word.height) * scale_y;
        let x = f32::from_i32(word.left) * scale_x;
        // PDF user space runs from the bottom; OCR boxes run from the top.
        let y = page_size_pt.1 - (f32::from_i32(word.top + word.height) * scale_y);

        if width_pt <= 0.0 || height_pt <= 0.0 {
            continue;
        }

        // Stretch the glyphs to the width the scan actually shows, so a
        // selection over the layer lands on the ink underneath it.
        #[expect(clippy::cast_precision_loss, reason = "word lengths are small")]
        let characters = word.text.chars().count().max(1) as f32;
        let natural = AVERAGE_ADVANCE * height_pt * characters;
        let horizontal_scale = if natural > 0.0 {
            (width_pt / natural * 100.0).clamp(1.0, 1000.0)
        } else {
            100.0
        };

        content.push_str(&format!(
            "/{} {height_pt:.2} Tf\n{horizontal_scale:.2} Tz\n1 0 0 1 {x:.2} {y:.2} Tm\n({encoded}) Tj\n",
            String::from_utf8_lossy(FONT_NAME)
        ));
        report.placed += 1;
    }

    content.push_str("ET\nQ\n");

    if report.placed == 0 {
        return report;
    }

    let font_id = ensure_font(document);
    add_font_to_resources(document, page_id, font_id);

    let stream_id = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));
    append_content(document, page_id, stream_id);

    report
}

/// Add the Helvetica font object, reusing one if this document already has ours.
fn ensure_font(document: &mut Document) -> ObjectId {
    let existing = document.objects.iter().find_map(|(id, object)| {
        let dict = object.as_dict().ok()?;
        (dict.get(b"Type").and_then(Object::as_name).ok() == Some(b"Font")
            && dict.get(b"BaseFont").and_then(Object::as_name).ok() == Some(b"Helvetica")
            && dict.get(b"Name").and_then(Object::as_name).ok() == Some(FONT_NAME))
        .then_some(*id)
    });

    existing.unwrap_or_else(|| {
        document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
            "Name" => Object::Name(FONT_NAME.to_vec()),
        })
    })
}

/// Put the font in the page's resource dictionary.
fn add_font_to_resources(document: &mut Document, page_id: ObjectId, font_id: ObjectId) {
    // Resources can be inline or indirect, and editing the wrong one leaves the
    // font invisible to the page that needs it.
    let resources_ref = document
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Resources").ok())
        .and_then(|value| value.as_reference().ok());

    if let Some(resources_id) = resources_ref {
        if let Ok(resources) = document.get_dictionary_mut(resources_id) {
            set_font(resources, font_id);
        }
        return;
    }

    if let Ok(page) = document.get_dictionary_mut(page_id) {
        match page.get_mut(b"Resources") {
            Ok(Object::Dictionary(resources)) => set_font(resources, font_id),
            _ => {
                let mut resources = Dictionary::new();
                set_font(&mut resources, font_id);
                page.set("Resources", Object::Dictionary(resources));
            }
        }
    }
}

fn set_font(resources: &mut Dictionary, font_id: ObjectId) {
    match resources.get_mut(b"Font") {
        Ok(Object::Dictionary(fonts)) => {
            fonts.set(
                String::from_utf8_lossy(FONT_NAME).into_owned(),
                Object::Reference(font_id),
            );
        }
        _ => {
            let mut fonts = Dictionary::new();
            fonts.set(
                String::from_utf8_lossy(FONT_NAME).into_owned(),
                Object::Reference(font_id),
            );
            resources.set("Font", Object::Dictionary(fonts));
        }
    }
}

/// Append our stream after whatever the page already draws.
fn append_content(document: &mut Document, page_id: ObjectId, stream_id: ObjectId) {
    let Ok(page) = document.get_dictionary_mut(page_id) else {
        return;
    };

    match page.get(b"Contents") {
        Ok(Object::Array(existing)) => {
            let mut contents = existing.clone();
            contents.push(Object::Reference(stream_id));
            page.set("Contents", Object::Array(contents));
        }
        Ok(Object::Reference(id)) => {
            let id = *id;
            page.set(
                "Contents",
                Object::Array(vec![Object::Reference(id), Object::Reference(stream_id)]),
            );
        }
        _ => page.set("Contents", Object::Reference(stream_id)),
    }
}

/// Small conversion helpers, kept explicit so the casts are all in one place.
trait FromNumber {
    fn from_u32(value: u32) -> f32;
    fn from_i32(value: i32) -> f32;
}

impl FromNumber for f32 {
    #[expect(clippy::cast_precision_loss, reason = "pixel counts fit in f32")]
    fn from_u32(value: u32) -> Self {
        value as Self
    }

    #[expect(clippy::cast_precision_loss, reason = "pixel counts fit in f32")]
    fn from_i32(value: i32) -> Self {
        value as Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, left: i32, top: i32, width: i32, height: i32) -> Word {
        Word {
            text: text.to_string(),
            left,
            top,
            width,
            height,
            confidence: 95.0,
            line: (1, 1, 1),
        }
    }

    /// A one-page document, 612x792, with an existing content stream.
    fn page_document() -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let content_id = doc.add_object(Stream::new(dictionary! {}, b"q Q".to_vec()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
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
        (doc, page_id)
    }

    #[test]
    fn the_layer_is_appended_and_never_replaces_what_the_page_draws() {
        let (mut doc, page_id) = page_document();
        let report = add_text_layer(
            &mut doc,
            page_id,
            &[word("Quarterly", 100, 120, 180, 40)],
            (1224, 1584),
            (612.0, 792.0),
        );

        assert_eq!(report.placed, 1);
        let contents = doc
            .get_dictionary(page_id)
            .expect("page")
            .get(b"Contents")
            .expect("contents")
            .as_array()
            .expect("an array after appending");
        assert_eq!(contents.len(), 2, "the original stream must still be first");
    }

    #[test]
    fn the_text_is_invisible() {
        // Mode 3 is the entire promise: the scan must look untouched.
        let (mut doc, page_id) = page_document();
        add_text_layer(
            &mut doc,
            page_id,
            &[word("Quarterly", 100, 120, 180, 40)],
            (1224, 1584),
            (612.0, 792.0),
        );

        let text = String::from_utf8_lossy(&doc.get_page_content(page_id)).into_owned();
        assert!(text.contains("3 Tr"), "{text}");
    }

    #[test]
    fn words_land_where_the_scan_shows_them() {
        // The image is exactly twice the page in each direction, so a word at
        // (100, 120) in pixels belongs at x = 50 and y = 792 - 80 = 712.
        let (mut doc, page_id) = page_document();
        add_text_layer(
            &mut doc,
            page_id,
            &[word("Quarterly", 100, 120, 180, 40)],
            (1224, 1584),
            (612.0, 792.0),
        );

        let content = String::from_utf8_lossy(&doc.get_page_content(page_id)).into_owned();
        assert!(content.contains("1 0 0 1 50.00 712.00 Tm"), "{content}");
    }

    #[test]
    fn the_text_survives_extraction() {
        // The point of the layer: this is what search and copy will see.
        let (mut doc, page_id) = page_document();
        add_text_layer(
            &mut doc,
            page_id,
            &[
                word("Quarterly", 100, 120, 180, 40),
                word("Report", 300, 120, 120, 40),
            ],
            (1224, 1584),
            (612.0, 792.0),
        );

        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).expect("serializes");
        let reopened = Document::load_mem(&bytes).expect("re-opens");
        let text = reopened.extract_text(&[1]).expect("extracts");

        assert!(text.contains("Quarterly"), "{text:?}");
        assert!(text.contains("Report"), "{text:?}");
    }

    #[test]
    fn a_word_the_font_cannot_encode_is_counted_rather_than_mangled() {
        // Mojibake in a text layer is worse than absence: it looks like data.
        let (mut doc, page_id) = page_document();
        let report = add_text_layer(
            &mut doc,
            page_id,
            &[word("汉字", 100, 120, 80, 40), word("ok", 200, 120, 40, 40)],
            (1224, 1584),
            (612.0, 792.0),
        );

        assert_eq!(report.placed, 1);
        assert_eq!(report.unencodable, 1);
    }

    #[test]
    fn accented_latin_text_is_kept() {
        let (mut doc, page_id) = page_document();
        let report = add_text_layer(
            &mut doc,
            page_id,
            &[word("Café", 100, 120, 80, 40)],
            (1224, 1584),
            (612.0, 792.0),
        );
        assert_eq!(report.placed, 1);
        assert_eq!(report.unencodable, 0);
    }

    #[test]
    fn nothing_is_written_for_a_page_with_no_words() {
        let (mut doc, page_id) = page_document();
        let before = doc.objects.len();
        let report = add_text_layer(&mut doc, page_id, &[], (1224, 1584), (612.0, 792.0));

        assert_eq!(report, LayerReport::default());
        assert_eq!(doc.objects.len(), before, "no font, no stream");
    }

    #[test]
    fn the_font_is_added_once_however_many_pages_are_recognized() {
        let (mut doc, page_id) = page_document();
        add_text_layer(
            &mut doc,
            page_id,
            &[word("one", 10, 10, 40, 20)],
            (612, 792),
            (612.0, 792.0),
        );
        let after_first = doc.objects.len();

        add_text_layer(
            &mut doc,
            page_id,
            &[word("two", 10, 40, 40, 20)],
            (612, 792),
            (612.0, 792.0),
        );

        // One new content stream, and no second copy of the font.
        assert_eq!(doc.objects.len(), after_first + 1);
    }
}
