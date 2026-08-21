//! Writing the watermark onto pages.
//!
//! One content stream per page, plus the resources it needs. Everything the
//! page already draws is left exactly as it is: the stream is appended for an
//! overlay, or put first for a background, and the page's own operators are
//! never parsed, rewritten, or re-encoded.
//!
//! The stream is wrapped in `q`/`Q` so nothing it sets — colour, transparency,
//! the transformation matrix — can leak into the page's own drawing. A
//! watermark that changed the fill colour of the document underneath it would
//! be a very hard bug to find.

use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};
use ypdf_core::Result;
use ypdf_doc::{Pdf, winansi};

use crate::metrics::{self, Face};
use crate::{Colour, Content, Placement, Position, Report, Watermark};

/// Names for the resources the stamp adds.
const FONT_NAME: &str = "YPDFWMF";
const IMAGE_NAME: &str = "YPDFWMI";
const GS_NAME: &str = "YPDFWMG";

/// Margin from the page edge for the corner positions, in points.
const MARGIN: f32 = 36.0;

/// How much of the page width a fitted watermark should span.
const FIT_FRACTION: f32 = 0.7;

/// How much of it one tile should span, when the watermark repeats.
const TILE_FRACTION: f32 = 0.25;

/// Stamp every page in `pages`.
pub fn apply(pdf: &mut Pdf, pages: &[u32], watermark: &Watermark) -> Result<Report> {
    let page_ids: Vec<ObjectId> = pages
        .iter()
        .filter_map(|page| pdf.page_ids().get((*page - 1) as usize).copied())
        .collect();

    let document = pdf.raw_mut();

    // Resources are shared across every page: one font, one image, one graphics
    // state, however many pages are stamped.
    let gs_id = add_graphics_state(document, watermark.opacity);
    let font_id = match &watermark.content {
        Content::Text { face, .. } => Some(add_font(document, *face)),
        Content::Image { .. } => None,
    };
    let image = match &watermark.content {
        Content::Image { bytes } => Some(crate::image_xobject::place(document, bytes)?),
        Content::Text { .. } => None,
    };

    let mut report = Report {
        image: image.map(|placed| placed.kind),
        ..Report::default()
    };

    for page_id in page_ids {
        let size = page_size(document, page_id);
        let content = match &watermark.content {
            Content::Text {
                text,
                face,
                size: point_size,
                colour,
            } => {
                let Some(encoded) = winansi::encode_literal(text) else {
                    // Refused rather than mangled: a watermark of question
                    // marks is worse than being told it cannot be drawn.
                    report.unencodable += text.chars().count();
                    continue;
                };
                text_stream(&encoded, text, *face, *point_size, *colour, size, watermark)
            }
            Content::Image { .. } => {
                let Some(placed) = image else { continue };
                image_stream(placed.width, placed.height, size, watermark)
            }
        };

        let stream_id = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        add_resources(document, page_id, gs_id, font_id, image.map(|i| i.id));
        attach(document, page_id, stream_id, watermark.placement);
        report.pages += 1;
    }

    Ok(report)
}

/// The page's size in points.
fn page_size(document: &Document, page_id: ObjectId) -> (f32, f32) {
    let media_box = document
        .get_dictionary(page_id)
        .ok()
        .and_then(|dict| dict.get(b"MediaBox").ok()?.as_array().ok().cloned());

    let Some(values) = media_box else {
        return (612.0, 792.0);
    };
    let number = |index: usize| -> f32 {
        values
            .get(index)
            .and_then(|value| value.as_float().ok())
            .unwrap_or(0.0)
    };

    let width = number(2) - number(0);
    let height = number(3) - number(1);
    if width <= 0.0 || height <= 0.0 {
        (612.0, 792.0)
    } else {
        (width, height)
    }
}

/// The content stream for a text watermark.
fn text_stream(
    encoded: &str,
    raw: &str,
    face: Face,
    point_size: Option<f32>,
    colour: Colour,
    page: (f32, f32),
    watermark: &Watermark,
) -> String {
    // Without an explicit size, fit the text across most of the page. Rotated
    // text spans the diagonal, so it can be larger. A tiled watermark is fitted
    // much smaller: at full width a single tile covers the page, which is a
    // stamp, not a tiling.
    let span = if watermark.position == Position::Tiled {
        page.0 * TILE_FRACTION
    } else if watermark.rotation.abs() > 1.0 {
        (page.0 * page.0 + page.1 * page.1).sqrt() * FIT_FRACTION
    } else {
        page.0 * FIT_FRACTION
    };
    let size = point_size.unwrap_or_else(|| {
        let at_ten = metrics::width_of(raw, 10.0, face);
        if at_ten > 0.0 {
            span / at_ten * 10.0
        } else {
            24.0
        }
    }) * watermark.scale;

    let width = metrics::width_of(raw, size, face);
    let height = size * metrics::CAP_HEIGHT;

    let mut out = String::from("q\n");
    out.push_str(&format!("/{GS_NAME} gs\n"));
    out.push_str(&format!(
        "{:.4} {:.4} {:.4} rg\n",
        colour.0, colour.1, colour.2
    ));

    if watermark.position == Position::Tiled {
        // Spaced by the text's own size, so the pattern scales with it.
        let step_x = (width + size).max(1.0);
        let step_y = (height + size * 2.0).max(1.0);
        let mut y = 0.0;
        while y < page.1 {
            let mut x = 0.0;
            while x < page.0 {
                out.push_str(&draw_text(encoded, face, size, x, y, watermark.rotation));
                x += step_x;
            }
            y += step_y;
        }
    } else {
        let (x, y) = anchor(watermark.position, page, width, height, watermark.rotation);
        out.push_str(&draw_text(encoded, face, size, x, y, watermark.rotation));
    }

    out.push_str("Q\n");
    out
}

/// One `BT`/`ET` block, rotated about its own origin.
fn draw_text(encoded: &str, _face: Face, size: f32, x: f32, y: f32, rotation: f32) -> String {
    let radians = rotation.to_radians();
    let (sin, cos) = radians.sin_cos();
    format!(
        "BT\n/{FONT_NAME} {size:.2} Tf\n{cos:.5} {sin:.5} {:.5} {cos:.5} {x:.2} {y:.2} Tm\n({encoded}) Tj\nET\n",
        -sin
    )
}

/// The content stream for an image watermark.
fn image_stream(
    image_width: u32,
    image_height: u32,
    page: (f32, f32),
    watermark: &Watermark,
) -> String {
    #[expect(clippy::cast_precision_loss, reason = "pixel counts fit in f32")]
    let (native_w, native_h) = (image_width as f32, image_height as f32);

    // Fit inside the page, then apply the scale the caller asked for. A tile
    // is fitted smaller, or the pattern is one image.
    let fraction = if watermark.position == Position::Tiled {
        TILE_FRACTION
    } else {
        FIT_FRACTION
    };
    let fit = (page.0 * fraction / native_w).min(page.1 * fraction / native_h);
    let width = native_w * fit * watermark.scale;
    let height = native_h * fit * watermark.scale;

    let mut out = String::from("q\n");
    out.push_str(&format!("/{GS_NAME} gs\n"));

    if watermark.position == Position::Tiled {
        let step_x = width.max(1.0);
        let step_y = height.max(1.0);
        let mut y = 0.0;
        while y < page.1 {
            let mut x = 0.0;
            while x < page.0 {
                out.push_str(&draw_image(width, height, x, y, watermark.rotation));
                x += step_x;
            }
            y += step_y;
        }
    } else {
        let (x, y) = anchor(watermark.position, page, width, height, watermark.rotation);
        out.push_str(&draw_image(width, height, x, y, watermark.rotation));
    }

    out.push_str("Q\n");
    out
}

/// One image draw, rotated about its own origin.
fn draw_image(width: f32, height: f32, x: f32, y: f32, rotation: f32) -> String {
    let radians = rotation.to_radians();
    let (sin, cos) = radians.sin_cos();
    // The image occupies the unit square, so the matrix carries its size.
    let (a, b) = (width * cos, width * sin);
    let (c, d) = (-height * sin, height * cos);
    format!("q\n{a:.4} {b:.4} {c:.4} {d:.4} {x:.2} {y:.2} cm\n/{IMAGE_NAME} Do\nQ\n")
}

/// Where the drawing starts, for a given position.
///
/// Rotated text is anchored so that its *centre* lands where the position asks,
/// which is what someone means by "centre" for a 45° `CONFIDENTIAL`.
fn anchor(
    position: Position,
    page: (f32, f32),
    width: f32,
    height: f32,
    rotation: f32,
) -> (f32, f32) {
    let radians = rotation.to_radians();
    let (sin, cos) = radians.sin_cos();
    // Half the drawn extent, after rotation.
    let half_x = (width * cos - height * sin) / 2.0;
    let half_y = (width * sin + height * cos) / 2.0;

    let (centre_x, centre_y) = match position {
        Position::Center | Position::Tiled => (page.0 / 2.0, page.1 / 2.0),
        Position::TopLeft => (MARGIN + width / 2.0, page.1 - MARGIN - height / 2.0),
        Position::TopCenter => (page.0 / 2.0, page.1 - MARGIN - height / 2.0),
        Position::TopRight => (
            page.0 - MARGIN - width / 2.0,
            page.1 - MARGIN - height / 2.0,
        ),
        Position::BottomLeft => (MARGIN + width / 2.0, MARGIN + height / 2.0),
        Position::BottomCenter => (page.0 / 2.0, MARGIN + height / 2.0),
        Position::BottomRight => (page.0 - MARGIN - width / 2.0, MARGIN + height / 2.0),
    };

    (centre_x - half_x, centre_y - half_y)
}

/// The transparency state, shared by every stamped page.
fn add_graphics_state(document: &mut Document, opacity: f32) -> ObjectId {
    let alpha = f64::from(opacity.clamp(0.0, 1.0));
    document.add_object(dictionary! {
        "Type" => "ExtGState",
        // Both, so the setting applies to text and images alike.
        "CA" => alpha,
        "ca" => alpha,
    })
}

fn add_font(document: &mut Document, face: Face) -> ObjectId {
    document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => face.base_font(),
        "Encoding" => "WinAnsiEncoding",
    })
}

/// Put the font, image, and graphics state where the page can see them.
fn add_resources(
    document: &mut Document,
    page_id: ObjectId,
    gs_id: ObjectId,
    font_id: Option<ObjectId>,
    image_id: Option<ObjectId>,
) {
    let resources_ref = document
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Resources").ok())
        .and_then(|value| value.as_reference().ok());

    let apply = |resources: &mut Dictionary| {
        sub_dictionary(resources, "ExtGState").set(GS_NAME, Object::Reference(gs_id));
        if let Some(font_id) = font_id {
            sub_dictionary(resources, "Font").set(FONT_NAME, Object::Reference(font_id));
        }
        if let Some(image_id) = image_id {
            sub_dictionary(resources, "XObject").set(IMAGE_NAME, Object::Reference(image_id));
        }
    };

    if let Some(resources_id) = resources_ref {
        if let Ok(resources) = document.get_dictionary_mut(resources_id) {
            apply(resources);
        }
        return;
    }

    if let Ok(page) = document.get_dictionary_mut(page_id) {
        match page.get_mut(b"Resources") {
            Ok(Object::Dictionary(resources)) => apply(resources),
            _ => {
                let mut resources = Dictionary::new();
                apply(&mut resources);
                page.set("Resources", Object::Dictionary(resources));
            }
        }
    }
}

/// Get or create a sub-dictionary of a resource dictionary.
fn sub_dictionary<'a>(resources: &'a mut Dictionary, key: &str) -> &'a mut Dictionary {
    if !matches!(resources.get(key.as_bytes()), Ok(Object::Dictionary(_))) {
        resources.set(key, Object::Dictionary(Dictionary::new()));
    }
    match resources.get_mut(key.as_bytes()) {
        Ok(Object::Dictionary(dict)) => dict,
        // Unreachable: it was just set above. Returning a scratch dictionary
        // loses the watermark rather than panicking on someone's document.
        _ => Box::leak(Box::new(Dictionary::new())),
    }
}

/// Add the stream to the page, over or under what is already there.
fn attach(document: &mut Document, page_id: ObjectId, stream_id: ObjectId, placement: Placement) {
    let Ok(page) = document.get_dictionary_mut(page_id) else {
        return;
    };

    let existing = match page.get(b"Contents") {
        Ok(Object::Array(items)) => items.clone(),
        Ok(Object::Reference(id)) => vec![Object::Reference(*id)],
        _ => Vec::new(),
    };

    let mut contents = Vec::with_capacity(existing.len() + 1);
    match placement {
        Placement::Over => {
            contents.extend(existing);
            contents.push(Object::Reference(stream_id));
        }
        Placement::Under => {
            contents.push(Object::Reference(stream_id));
            contents.extend(existing);
        }
    }

    page.set("Contents", Object::Array(contents));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Watermark;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    fn page_content(pdf: &Pdf, page: usize) -> String {
        let page_id = pdf.page_ids()[page];
        String::from_utf8_lossy(&pdf.raw().get_page_content(page_id)).into_owned()
    }

    #[test]
    fn an_overlay_comes_after_what_the_page_draws() {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        let before = page_content(&pdf, 0);

        apply(&mut pdf, &[1], &Watermark::text("CONFIDENTIAL")).expect("stamps");

        let after = page_content(&pdf, 0);
        assert!(
            after.starts_with(&before),
            "the page's own drawing must be untouched and first"
        );
        assert!(after.contains("CONFIDENTIAL"), "{after}");
    }

    #[test]
    fn a_background_goes_before_it() {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        let before = page_content(&pdf, 0);

        apply(
            &mut pdf,
            &[1],
            &Watermark::text("DRAFT").with_placement(Placement::Under),
        )
        .expect("stamps");

        let after = page_content(&pdf, 0);
        assert!(after.ends_with(&before), "the page must be drawn last");
        assert!(after.contains("DRAFT"));
    }

    #[test]
    fn only_the_pages_asked_for_are_stamped() {
        let mut pdf = Pdf::open(fixture("many-pages.pdf")).expect("opens");
        let report = apply(&mut pdf, &[2, 4], &Watermark::text("COPY")).expect("stamps");

        assert_eq!(report.pages, 2);
        assert!(!page_content(&pdf, 0).contains("COPY"));
        assert!(page_content(&pdf, 1).contains("COPY"));
        assert!(!page_content(&pdf, 2).contains("COPY"));
        assert!(page_content(&pdf, 3).contains("COPY"));
    }

    #[test]
    fn the_stream_is_balanced_so_nothing_leaks_into_the_page() {
        // A watermark that left a fill colour or a matrix set would change the
        // document under it, which is a very hard bug to find later.
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        apply(&mut pdf, &[1], &Watermark::text("CONFIDENTIAL")).expect("stamps");

        let content = page_content(&pdf, 0);
        assert_eq!(
            content.matches('q').count(),
            content.matches('Q').count(),
            "unbalanced save/restore: {content}"
        );
        assert_eq!(content.matches("BT").count(), content.matches("ET").count());
    }

    #[test]
    fn opacity_is_written_as_a_graphics_state_the_page_can_see() {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        apply(
            &mut pdf,
            &[1],
            &Watermark::text("CONFIDENTIAL").with_opacity(0.25),
        )
        .expect("stamps");

        assert!(page_content(&pdf, 0).contains(&format!("/{GS_NAME} gs")));

        let page_id = pdf.page_ids()[0];
        let resources = pdf
            .raw()
            .get_dictionary(page_id)
            .expect("page")
            .get(b"Resources")
            .expect("resources");
        let resources = match resources {
            Object::Reference(id) => pdf.raw().get_dictionary(*id).expect("indirect").clone(),
            Object::Dictionary(dict) => dict.clone(),
            _ => panic!("no resources"),
        };
        assert!(
            resources
                .get(b"ExtGState")
                .and_then(Object::as_dict)
                .is_ok_and(|gs| gs.get(GS_NAME.as_bytes()).is_ok()),
            "the state has to be reachable from the page"
        );
    }

    #[test]
    fn tiling_repeats_the_text_across_the_page() {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        apply(
            &mut pdf,
            &[1],
            &Watermark::text("COPY")
                .with_position(Position::Tiled)
                .with_rotation(0.0),
        )
        .expect("stamps");

        let content = page_content(&pdf, 0);
        assert!(content.matches("(COPY) Tj").count() > 4, "{content}");
    }

    #[test]
    fn a_centred_watermark_lands_near_the_middle_of_the_page() {
        // 612x792, so the centre is (306, 396). The anchor is the start of the
        // text, not its centre, so it sits left of and below the middle.
        let (x, y) = anchor(Position::Center, (612.0, 792.0), 200.0, 20.0, 0.0);
        assert!((x - 206.0).abs() < 1.0, "{x}");
        assert!((y - 386.0).abs() < 1.0, "{y}");
    }

    #[test]
    fn a_corner_watermark_stays_inside_the_margin() {
        let (x, y) = anchor(Position::BottomLeft, (612.0, 792.0), 100.0, 20.0, 0.0);
        assert!(x >= MARGIN - 0.01, "{x}");
        assert!(y >= MARGIN - 0.01, "{y}");

        let (x, y) = anchor(Position::TopRight, (612.0, 792.0), 100.0, 20.0, 0.0);
        assert!(x + 100.0 <= 612.0 - MARGIN + 0.01, "{x}");
        assert!(y + 20.0 <= 792.0 - MARGIN + 0.01, "{y}");
    }

    #[test]
    fn text_that_the_font_cannot_encode_is_refused_rather_than_mangled() {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        let before = page_content(&pdf, 0);

        let report = apply(&mut pdf, &[1], &Watermark::text("\u{6a5f}\u{5bc6}")).expect("runs");

        assert_eq!(report.pages, 0);
        assert!(report.unencodable > 0);
        assert_eq!(page_content(&pdf, 0), before, "nothing may have been drawn");
    }

    #[test]
    fn the_document_still_opens_after_stamping() {
        let mut pdf = Pdf::open(fixture("many-pages.pdf")).expect("opens");
        apply(&mut pdf, &[1, 2, 3], &Watermark::text("CONFIDENTIAL")).expect("stamps");

        let bytes = pdf.to_bytes().expect("serializes");
        let reopened = Pdf::from_bytes(&bytes).expect("re-opens");
        assert_eq!(reopened.page_count(), 12);
    }
}
