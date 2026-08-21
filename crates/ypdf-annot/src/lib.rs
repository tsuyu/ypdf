//! Annotations: comments and marks on a page (spec §10).
//!
//! Everything here is **additive and reversible**. An annotation is an object
//! in a page's `/Annots` list; removing one is deleting an entry. Nothing this
//! crate does touches a content stream, so a document can be marked up, sent
//! back, and stripped clean without the pages ever being rewritten.
//!
//! Two rules it is built on.
//!
//! **Every annotation carries its own appearance.** One without an `/AP` stream
//! is a suggestion: readers draw it however they like, or not at all, and
//! printing usually ignores it. What is written here is what a reader shows.
//!
//! **A highlight multiplies rather than covers.** An opaque wash over words is
//! a redaction that says "highlight", and it is the one drawing mistake in this
//! area that loses information rather than merely looking wrong.
//!
//! ```no_run
//! use ypdf_annot::{Annotation, Shape, add};
//! use ypdf_doc::Pdf;
//!
//! # fn main() -> ypdf_core::Result<()> {
//! let mut pdf = Pdf::open("report.pdf")?;
//! add(
//!     &mut pdf,
//!     &Annotation::new(1, Shape::Highlight { quads: vec![[72.0, 700.0, 300.0, 714.0]] })
//!         .with_contents("check this figure")
//!         .with_author("Ada"),
//! )?;
//! pdf.save("report-reviewed.pdf")?;
//! # Ok(())
//! # }
//! ```

mod appearance;
mod model;

use lopdf::{Dictionary, Object, ObjectId, dictionary};
use ypdf_core::{Error, Result};
use ypdf_doc::Pdf;

pub use model::{Annotation, Colour, Rect, Shape};

/// Annotation flags: `Print` means it appears on paper as well as on screen.
///
/// Off by default in the format, which is how people end up with comments that
/// vanish when the document is printed.
const PRINT: i64 = 4;

/// Add an annotation to a document.
///
/// # Errors
///
/// Refuses a page outside the document, and a shape with no area — an
/// annotation nobody can see or click is not one.
pub fn add(pdf: &mut Pdf, annotation: &Annotation) -> Result<()> {
    let count = pdf.page_count();
    if annotation.page == 0 || annotation.page > count {
        return Err(Error::PageOutOfRange {
            requested: annotation.page,
            pages: count,
        });
    }

    // The rectangle covers the stroke, not just the path: a horizontal line
    // has no height as geometry, and a reader clips a zero-height rectangle to
    // nothing.
    let bounds = annotation.shape.bounds_with_width(annotation.width);
    if bounds[2] - bounds[0] < 0.5 || bounds[3] - bounds[1] < 0.5 {
        return Err(Error::Config {
            detail: format!(
                "this {} has no area; nothing would be drawn",
                annotation.shape.label()
            ),
            source_path: None,
        });
    }

    let Some(page_id) = pdf.page_ids().get((annotation.page - 1) as usize).copied() else {
        return Ok(());
    };

    let drawn = appearance::draw(annotation);
    if drawn.unencodable {
        // The text stays in `/Contents`, where a reader shows it in full; what
        // could not be drawn is the copy painted onto the page. Saying so beats
        // an annotation that silently comes out blank.
        tracing::warn!(
            kind = annotation.shape.label(),
            "the built-in font cannot draw this text; the comment is still attached"
        );
    }
    let state_id = pdf
        .raw_mut()
        .add_object(Object::Dictionary(appearance::graphics_state(annotation)));

    // The graphics state is added after the stream is built, so its name is
    // filled in here rather than guessed at inside the drawing code.
    let mut stream = drawn.stream;
    if let Ok(Object::Dictionary(resources)) = stream.dict.get_mut(b"Resources")
        && let Ok(Object::Dictionary(states)) = resources.get_mut(b"ExtGState")
    {
        states.set("YPDFAG", Object::Reference(state_id));
    }
    let stream_id = pdf.raw_mut().add_object(Object::Stream(stream));

    let mut dict = dictionary! {
        "Type" => "Annot",
        "Subtype" => annotation.shape.subtype(),
        "Rect" => Object::Array(bounds.iter().map(|v| Object::Real(*v)).collect()),
        "F" => Object::Integer(PRINT),
        "C" => colour(annotation.colour),
        "CA" => Object::Real(annotation.opacity.clamp(0.0, 1.0)),
        "AP" => Object::Dictionary(dictionary! { "N" => Object::Reference(stream_id) }),
        "P" => Object::Reference(page_id),
    };

    if !annotation.contents.is_empty() {
        dict.set(
            "Contents",
            Object::String(
                ypdf_doc::encode_text(&annotation.contents),
                lopdf::StringFormat::Literal,
            ),
        );
    }
    if !annotation.author.is_empty() {
        dict.set(
            "T",
            Object::String(
                ypdf_doc::encode_text(&annotation.author),
                lopdf::StringFormat::Literal,
            ),
        );
    }
    dict.set(
        "M",
        Object::string_literal(annotation.modified.clone().unwrap_or_else(now)),
    );

    // The geometry each subtype needs in its own right. A reader that rebuilds
    // appearances — and several do, on edit — works from these, not from the
    // stream, so leaving them out means the annotation survives until someone
    // touches it and then vanishes.
    add_geometry(&mut dict, annotation, bounds);

    if annotation.width > 0.0 {
        dict.set(
            "BS",
            Object::Dictionary(dictionary! {
                "W" => Object::Real(annotation.width),
                "S" => "S",
            }),
        );
    }

    let annotation_id = pdf.raw_mut().add_object(Object::Dictionary(dict));
    append(pdf, page_id, annotation_id);

    tracing::info!(
        page = annotation.page,
        kind = annotation.shape.label(),
        "annotation added"
    );
    Ok(())
}

/// The subtype-specific geometry keys.
fn add_geometry(dict: &mut Dictionary, annotation: &Annotation, bounds: Rect) {
    match &annotation.shape {
        Shape::Highlight { quads } | Shape::Underline { quads } | Shape::StrikeOut { quads } => {
            // QuadPoints run top-left, top-right, bottom-left, bottom-right —
            // an order that has caught out every implementer at least once.
            let mut points = Vec::with_capacity(quads.len() * 8);
            for quad in quads {
                for value in [
                    quad[0], quad[3], quad[2], quad[3], quad[0], quad[1], quad[2], quad[1],
                ] {
                    points.push(Object::Real(value));
                }
            }
            dict.set("QuadPoints", Object::Array(points));
        }
        Shape::Ink { strokes } => {
            let list = strokes
                .iter()
                .map(|stroke| {
                    Object::Array(
                        stroke
                            .iter()
                            .flat_map(|point| [Object::Real(point[0]), Object::Real(point[1])])
                            .collect(),
                    )
                })
                .collect();
            dict.set("InkList", Object::Array(list));
        }
        Shape::Line { from, to, arrow } => {
            dict.set(
                "L",
                Object::Array(vec![
                    Object::Real(from[0]),
                    Object::Real(from[1]),
                    Object::Real(to[0]),
                    Object::Real(to[1]),
                ]),
            );
            if *arrow {
                dict.set(
                    "LE",
                    Object::Array(vec![
                        Object::Name(b"None".to_vec()),
                        Object::Name(b"OpenArrow".to_vec()),
                    ]),
                );
            }
        }
        Shape::Rectangle { fill, .. } | Shape::Ellipse { fill, .. } => {
            if let Some(fill) = fill {
                dict.set("IC", colour(*fill));
            }
        }
        Shape::TextBox { size, .. } => {
            let size = size.unwrap_or(0.0);
            dict.set(
                "DA",
                Object::string_literal(format!(
                    "/Helv {size:.2} Tf {:.4} {:.4} {:.4} rg",
                    annotation.colour.0, annotation.colour.1, annotation.colour.2
                )),
            );
        }
        Shape::Stamp { text, .. } => {
            // `/Name` is what a reader falls back to without an appearance.
            let name = match text.to_ascii_uppercase().as_str() {
                "APPROVED" => "Approved",
                "DRAFT" => "Draft",
                "CONFIDENTIAL" => "Confidential",
                "FINAL" => "Final",
                _ => "Draft",
            };
            dict.set("Name", Object::Name(name.as_bytes().to_vec()));
        }
        Shape::Note { .. } => {
            dict.set("Name", Object::Name(b"Comment".to_vec()));
            // Without this the icon is drawn at whatever size the reader likes.
            let _ = bounds;
        }
    }
}

/// Every annotation this crate can describe, in page order.
///
/// Annotations it did not write are listed too, as best it can read them: a
/// listing that only showed its own marks would be a poor account of a document
/// that came back from review.
#[must_use]
pub fn list(pdf: &Pdf) -> Vec<Annotation> {
    let mut out = Vec::new();

    for (index, page_id) in pdf.page_ids().iter().enumerate() {
        let Ok(page) = u32::try_from(index + 1) else {
            continue;
        };
        for (_, dict) in annotations(pdf, *page_id) {
            if let Some(annotation) = read_one(pdf, page, &dict) {
                out.push(annotation);
            }
        }
    }

    out
}

/// Remove annotations matching a predicate, returning how many went.
pub fn remove_where(pdf: &mut Pdf, mut predicate: impl FnMut(&Annotation) -> bool) -> usize {
    let mut removed = 0;

    for (index, page_id) in pdf.page_ids().to_vec().into_iter().enumerate() {
        let Ok(page) = u32::try_from(index + 1) else {
            continue;
        };

        let mut keep = Vec::new();
        let mut changed = false;

        for (reference, dict) in annotations(pdf, page_id) {
            let matched =
                read_one(pdf, page, &dict).is_some_and(|annotation| predicate(&annotation));
            if matched {
                removed += 1;
                changed = true;
            } else {
                keep.push(reference);
            }
        }

        if changed && let Ok(page) = pdf.raw_mut().get_dictionary_mut(page_id) {
            if keep.is_empty() {
                page.remove(b"Annots");
            } else {
                page.set("Annots", Object::Array(keep));
            }
        }
    }

    if removed > 0 {
        tracing::info!(removed, "annotations removed");
    }
    removed
}

/// Read one annotation dictionary.
///
/// Widgets are skipped: a form field is not a comment, and `ypdf-forms` is
/// where it belongs.
fn read_one(pdf: &Pdf, page: u32, dict: &Dictionary) -> Option<Annotation> {
    let subtype = dict.get(b"Subtype").and_then(Object::as_name).ok()?;
    if subtype == b"Widget" || subtype == b"Popup" || subtype == b"Link" {
        return None;
    }

    let rect = rect_of(dict)?;
    let quads = quad_points(dict).unwrap_or_else(|| vec![rect]);

    let shape = match subtype {
        b"Highlight" => Shape::Highlight { quads },
        b"Underline" => Shape::Underline { quads },
        b"StrikeOut" => Shape::StrikeOut { quads },
        b"Text" => Shape::Note {
            at: [rect[0], rect[3]],
        },
        b"FreeText" => Shape::TextBox { rect, size: None },
        b"Ink" => Shape::Ink {
            strokes: ink_list(dict).unwrap_or_default(),
        },
        b"Square" => Shape::Rectangle {
            rect,
            fill: interior(dict),
        },
        b"Circle" => Shape::Ellipse {
            rect,
            fill: interior(dict),
        },
        b"Line" => {
            let points = numbers(dict.get(b"L").ok());
            match points[..] {
                [x0, y0, x1, y1] => Shape::Line {
                    from: [x0, y0],
                    to: [x1, y1],
                    arrow: dict.get(b"LE").is_ok(),
                },
                _ => Shape::Line {
                    from: [rect[0], rect[1]],
                    to: [rect[2], rect[3]],
                    arrow: false,
                },
            }
        }
        b"Stamp" => Shape::Stamp {
            rect,
            text: dict
                .get(b"Name")
                .and_then(Object::as_name)
                .map(|name| String::from_utf8_lossy(name).into_owned())
                .unwrap_or_default(),
        },
        // Something else entirely — a sound, a movie, a 3D scene. Reported as a
        // note so it appears in the listing rather than being silently dropped.
        _ => Shape::Note {
            at: [rect[0], rect[3]],
        },
    };

    let _ = pdf;
    Some(Annotation {
        page,
        shape,
        colour: read_colour(dict.get(b"C").ok()).unwrap_or_default(),
        opacity: dict
            .get(b"CA")
            .and_then(Object::as_float)
            .unwrap_or(1.0)
            .clamp(0.0, 1.0),
        width: dict
            .get(b"BS")
            .and_then(Object::as_dict)
            .ok()
            .and_then(|bs| bs.get(b"W").and_then(Object::as_float).ok())
            .unwrap_or(1.5),
        contents: text_of(dict.get(b"Contents").ok()),
        author: text_of(dict.get(b"T").ok()),
        modified: match dict.get(b"M").ok() {
            Some(Object::String(bytes, _)) => Some(ypdf_doc::decode_text(bytes)),
            _ => None,
        },
    })
}

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

fn append(pdf: &mut Pdf, page_id: ObjectId, annotation_id: ObjectId) {
    let mut annots = pdf
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

    annots.push(Object::Reference(annotation_id));
    if let Ok(page) = pdf.raw_mut().get_dictionary_mut(page_id) {
        page.set("Annots", Object::Array(annots));
    }
}

fn colour(colour: Colour) -> Object {
    Object::Array(vec![
        Object::Real(colour.0),
        Object::Real(colour.1),
        Object::Real(colour.2),
    ])
}

fn read_colour(value: Option<&Object>) -> Option<Colour> {
    let numbers = numbers(value);
    match numbers[..] {
        [grey] => Some(Colour(grey, grey, grey)),
        [r, g, b] => Some(Colour(r, g, b)),
        // CMYK, which this does not convert; the default is closer than a
        // guess would be.
        _ => None,
    }
}

fn numbers(value: Option<&Object>) -> Vec<f32> {
    value
        .and_then(|value| value.as_array().ok())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_float().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn rect_of(dict: &Dictionary) -> Option<Rect> {
    let values = numbers(dict.get(b"Rect").ok());
    match values[..] {
        [x0, y0, x1, y1] => Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)]),
        _ => None,
    }
}

fn quad_points(dict: &Dictionary) -> Option<Vec<Rect>> {
    let values = numbers(dict.get(b"QuadPoints").ok());
    if values.is_empty() || !values.len().is_multiple_of(8) {
        return None;
    }

    Some(
        values
            .chunks_exact(8)
            .map(|quad| {
                let xs = [quad[0], quad[2], quad[4], quad[6]];
                let ys = [quad[1], quad[3], quad[5], quad[7]];
                [
                    xs.iter().copied().fold(f32::MAX, f32::min),
                    ys.iter().copied().fold(f32::MAX, f32::min),
                    xs.iter().copied().fold(f32::MIN, f32::max),
                    ys.iter().copied().fold(f32::MIN, f32::max),
                ]
            })
            .collect(),
    )
}

fn ink_list(dict: &Dictionary) -> Option<Vec<Vec<[f32; 2]>>> {
    let strokes = dict.get(b"InkList").ok()?.as_array().ok()?;
    Some(
        strokes
            .iter()
            .map(|stroke| {
                numbers(Some(stroke))
                    .chunks_exact(2)
                    .map(|pair| [pair[0], pair[1]])
                    .collect()
            })
            .collect(),
    )
}

fn interior(dict: &Dictionary) -> Option<Colour> {
    read_colour(dict.get(b"IC").ok())
}

fn text_of(value: Option<&Object>) -> String {
    match value {
        Some(Object::String(bytes, _)) => ypdf_doc::decode_text(bytes),
        _ => String::new(),
    }
}

/// The current time as a PDF date string.
///
/// Written in UTC with an explicit `Z`, rather than a local time with no zone:
/// annotations travel between people, and a timestamp nobody can place is worse
/// than one an hour out.
fn now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);

    let days = (seconds / 86_400) as i64;
    let time = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);

    format!(
        "D:{year:04}{month:02}{day:02}{:02}{:02}{:02}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

/// Days since the epoch to a calendar date.
///
/// Howard Hinnant's `civil_from_days`, which is the standard way to do this
/// without a calendar library.
const fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;

    (if month <= 2 { year + 1 } else { year }, month, day)
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
    fn a_date_is_written_in_the_shape_a_pdf_expects() {
        let date = now();
        assert!(date.starts_with("D:"), "{date}");
        assert_eq!(date.len(), 17, "{date}");
        assert!(date.ends_with('Z'), "a timestamp needs a zone: {date}");
    }

    #[test]
    fn the_calendar_conversion_agrees_with_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        // A leap day, which is where a wrong conversion shows first.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn a_page_that_does_not_exist_is_refused() {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        let error = add(
            &mut pdf,
            &Annotation::new(9, Shape::Note { at: [10.0, 10.0] }),
        )
        .expect_err("refused");
        assert_eq!(error.code(), "E_PAGE_RANGE");
    }

    #[test]
    fn a_shape_with_no_area_is_refused() {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        assert!(
            add(
                &mut pdf,
                &Annotation::new(
                    1,
                    Shape::Rectangle {
                        rect: [10.0, 10.0, 10.0, 40.0],
                        fill: None,
                    },
                ),
            )
            .is_err()
        );
    }
}
