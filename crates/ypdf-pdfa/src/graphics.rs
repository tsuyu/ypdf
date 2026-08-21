//! Graphics requirements: transparency, interpolation, tagging.

use crate::Context;
use crate::model::{Conformance, Violation};
use crate::walk::{bool_at, dict_at, dictionaries, is_type, name_at, number_at, streams};

pub fn check(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    transparency(ctx, out);
    interpolation(ctx, out);
    tagging(ctx, out);
}

/// Part 1 has no transparency model at all.
///
/// Part 2 introduced one, so this check is only for part 1 — flagging a soft
/// mask in a PDF/A-2 file would be reporting a conforming feature as a fault.
fn transparency(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    if ctx.level.part != 1 {
        return;
    }

    let mut found = 0;

    for dict in dictionaries(ctx.doc) {
        if is_type(dict, b"ExtGState") {
            let soft_mask = name_at(ctx.doc, dict, b"SMask");
            if dict.has(b"SMask") && soft_mask.as_deref() != Some("None") {
                found += 1;
            }
            for key in [b"CA".as_slice(), b"ca".as_slice()] {
                if number_at(ctx.doc, dict, key)
                    .is_some_and(|alpha| (alpha - 1.0).abs() > f32::EPSILON)
                {
                    found += 1;
                }
            }
            if let Some(mode) = name_at(ctx.doc, dict, b"BM")
                && !matches!(mode.as_str(), "Normal" | "Compatible")
            {
                found += 1;
            }
        }

        // A page or form whose group is a transparency group is transparent
        // even if nothing in it ever sets an alpha.
        if is_type(dict, b"Group")
            && name_at(ctx.doc, dict, b"S").as_deref() == Some("Transparency")
        {
            found += 1;
        }
    }

    // An image with a soft mask is the most common way transparency arrives in
    // a file nobody meant to make transparent.
    found += streams(ctx.doc)
        .iter()
        .filter(|stream| stream.dict.has(b"SMask") || stream.dict.has(b"SMaskInData"))
        .count();

    if found > 0 {
        out.push(Violation::counted(
            "PDFA_TRANSPARENCY",
            "no transparency",
            "the document uses transparency — soft masks, alpha constants, or blend modes. \
             PDF/A-1 has no transparency model, so what a reader draws is its own decision. \
             PDF/A-2 and later allow it",
            found,
        ));
    }
}

fn interpolation(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    let count = streams(ctx.doc)
        .iter()
        .filter(|stream| bool_at(ctx.doc, &stream.dict, b"Interpolate") == Some(true))
        .count();
    if count > 0 {
        out.push(Violation::counted(
            "PDFA_INTERPOLATE",
            "images are not interpolated",
            "images ask to be smoothed on display with /Interpolate true; how much smoothing \
             is up to the reader, so the page is not the same twice",
            count,
        ));
    }
}

/// Level a is the accessible level: the document has to say what its content
/// *is*, not only what it looks like.
fn tagging(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    if ctx.level.conformance != Conformance::A {
        return;
    }
    let Ok(catalog) = ctx.doc.catalog() else {
        return;
    };

    let marked = dict_at(ctx.doc, catalog, b"MarkInfo")
        .and_then(|info| bool_at(ctx.doc, info, b"Marked"))
        .unwrap_or(false);
    if !marked || !catalog.has(b"StructTreeRoot") {
        out.push(Violation::new(
            "PDFA_NOT_TAGGED",
            "tagged PDF",
            "the document has no structure tree, or does not declare itself marked; level a \
             is the accessible level and needs both",
        ));
    }

    if !catalog.has(b"Lang") {
        out.push(Violation::new(
            "PDFA_NO_LANGUAGE",
            "natural language declared",
            "the catalog has no /Lang, so nothing says what language the text is in; a \
             screen reader has to guess how to pronounce it",
        ));
    }
}
