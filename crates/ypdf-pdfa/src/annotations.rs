//! Annotation requirements: what may be on a page, and what it must carry.

use crate::Context;
use crate::model::Violation;
use crate::walk::{array_at, dict_at, name_at, number_at, resolve};

/// Annotation flags, from the PDF specification's table.
const HIDDEN: i64 = 1 << 1;
const PRINT: i64 = 1 << 2;
const NO_VIEW: i64 = 1 << 5;

/// Annotations that play, run, or show something other than themselves.
///
/// Not a complete permitted-list check — that is in [`crate::LIMITS`] — but
/// these are the ones that carry behaviour, and behaviour is what PDF/A
/// excludes.
const FORBIDDEN: [&str; 5] = ["Movie", "Sound", "Screen", "RichMedia", "3D"];

pub fn check(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    let mut forbidden: Vec<String> = Vec::new();
    let mut no_appearance = 0;
    let mut hidden = 0;
    let mut not_printing = 0;
    let mut transparent = 0;

    for page_id in ctx.doc.get_pages().values() {
        let Ok(page) = ctx.doc.get_dictionary(*page_id) else {
            continue;
        };
        let Some(annots) = array_at(ctx.doc, page, b"Annots") else {
            continue;
        };

        for item in annots {
            let Some(annot) = resolve(ctx.doc, item).and_then(|object| match object {
                lopdf::Object::Dictionary(dict) => Some(dict),
                _ => None,
            }) else {
                continue;
            };

            let subtype = name_at(ctx.doc, annot, b"Subtype").unwrap_or_default();
            if FORBIDDEN.contains(&subtype.as_str()) {
                forbidden.push(subtype.clone());
            }
            if ctx.level.part == 1 && subtype == "FileAttachment" {
                forbidden.push(subtype.clone());
            }

            let flags = number_at(ctx.doc, annot, b"F").unwrap_or(0.0) as i64;
            if flags & HIDDEN != 0 || flags & NO_VIEW != 0 {
                hidden += 1;
            }
            // A Popup is drawn by the reader when someone opens the note it
            // belongs to, and Link has no appearance of its own; everything
            // else has to carry what it looks like.
            if !matches!(subtype.as_str(), "Popup" | "Link") {
                if flags & PRINT == 0 {
                    not_printing += 1;
                }
                if !has_normal_appearance(ctx, annot) {
                    no_appearance += 1;
                }
            }

            if ctx.level.part == 1
                && number_at(ctx.doc, annot, b"CA")
                    .is_some_and(|alpha| (alpha - 1.0).abs() > f32::EPSILON)
            {
                transparent += 1;
            }
        }
    }

    if !forbidden.is_empty() {
        forbidden.sort_unstable();
        forbidden.dedup();
        out.push(Violation::counted(
            "PDFA_FORBIDDEN_ANNOTATION",
            "no annotations that carry behaviour",
            format!(
                "annotation types the standard does not permit: {}",
                forbidden.join(", ")
            ),
            forbidden.len(),
        ));
    }

    if no_appearance > 0 {
        out.push(Violation::counted(
            "PDFA_ANNOT_NO_APPEARANCE",
            "annotations carry their own appearance",
            "annotations have no normal appearance stream, so what they look like is left to \
             whichever reader opens the file",
            no_appearance,
        ));
    }

    if hidden > 0 {
        out.push(Violation::counted(
            "PDFA_ANNOT_HIDDEN",
            "nothing is hidden",
            "annotations are marked Hidden or NoView; a file that shows different content \
             depending on how it is opened cannot be archived as one document",
            hidden,
        ));
    }

    if not_printing > 0 {
        out.push(Violation::counted(
            "PDFA_ANNOT_NOT_PRINTING",
            "annotations print",
            "annotations do not have the Print flag set, so printing the file loses them",
            not_printing,
        ));
    }

    if transparent > 0 {
        out.push(Violation::counted(
            "PDFA_ANNOT_TRANSPARENT",
            "annotations are opaque",
            "annotations set /CA to something other than 1.0; PDF/A-1 has no transparency \
             model to draw them with",
            transparent,
        ));
    }
}

/// Does the annotation have an `/AP` with an `/N` in it?
fn has_normal_appearance(ctx: &Context<'_>, annot: &lopdf::Dictionary) -> bool {
    dict_at(ctx.doc, annot, b"AP").is_some_and(|ap| ap.has(b"N"))
}
