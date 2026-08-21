//! Font requirements: the glyphs travel with the file, and map back to text.

use lopdf::Dictionary;

use crate::Context;
use crate::model::Violation;
use crate::walk::{dict_at, dictionaries, is_type, name_at, resolve};

pub fn check(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    let mut unembedded: Vec<String> = Vec::new();
    let mut unmapped: Vec<String> = Vec::new();

    for font in dictionaries(ctx.doc)
        .into_iter()
        .filter(|dict| is_type(dict, b"Font"))
    {
        let subtype = name_at(ctx.doc, font, b"Subtype").unwrap_or_default();
        let name = name_at(ctx.doc, font, b"BaseFont").unwrap_or_else(|| "(unnamed)".to_string());

        // A Type 3 font *is* its glyphs: they are content streams in the file,
        // so there is nothing to embed.
        if subtype == "Type3" {
            continue;
        }
        // A Type 0 font is a shell; the program hangs off its descendant.
        let carrier = if subtype == "Type0" {
            descendant(ctx, font).unwrap_or(font)
        } else {
            font
        };

        if !embedded(ctx, carrier) {
            unembedded.push(name.clone());
        }

        // Levels a and u promise the text can be recovered. Without a
        // /ToUnicode map, copying out of the document gives glyph numbers, and
        // a screen reader gives nothing.
        if ctx.level.conformance.needs_unicode()
            && !font.has(b"ToUnicode")
            && !predictable_encoding(ctx, font)
        {
            unmapped.push(name);
        }
    }

    if !unembedded.is_empty() {
        unembedded.sort_unstable();
        unembedded.dedup();
        out.push(Violation::counted(
            "PDFA_FONT_NOT_EMBEDDED",
            "every font embedded",
            format!(
                "fonts are used but not embedded: {}. A file that borrows fonts from the \
                 machine reading it looks different on every machine",
                unembedded.join(", ")
            ),
            unembedded.len(),
        ));
    }

    if !unmapped.is_empty() {
        unmapped.sort_unstable();
        unmapped.dedup();
        out.push(Violation::counted(
            "PDFA_NO_TOUNICODE",
            "text maps back to Unicode",
            format!(
                "fonts have no /ToUnicode map and no encoding that implies one: {}. Text in \
                 them cannot be copied out or read aloud",
                unmapped.join(", ")
            ),
            unmapped.len(),
        ));
    }
}

/// The descendant of a Type 0 font.
fn descendant<'a>(ctx: &'a Context<'_>, font: &'a Dictionary) -> Option<&'a Dictionary> {
    let value = font.get(b"DescendantFonts").ok()?;
    let items = resolve(ctx.doc, value)?.as_array().ok()?;
    let first = items.first()?;
    match resolve(ctx.doc, first)? {
        lopdf::Object::Dictionary(dict) => Some(dict),
        _ => None,
    }
}

/// Does this font carry its own program?
fn embedded(ctx: &Context<'_>, font: &Dictionary) -> bool {
    let Some(descriptor) = dict_at(ctx.doc, font, b"FontDescriptor") else {
        // No descriptor at all means one of the fourteen standard fonts, which
        // PDF/A does not exempt: an archive cannot assume Helvetica exists.
        return false;
    };
    descriptor.has(b"FontFile") || descriptor.has(b"FontFile2") || descriptor.has(b"FontFile3")
}

/// Encodings whose character codes already say what the text is.
///
/// A simple font with one of these has a defined mapping to Unicode without
/// carrying a /ToUnicode map, which is how most conforming text-only documents
/// are built. `Identity-H` is deliberately *not* here: it maps codes to glyph
/// numbers inside one particular font program, which is exactly the case where
/// a /ToUnicode map is the only way back to the text.
fn predictable_encoding(ctx: &Context<'_>, font: &Dictionary) -> bool {
    matches!(
        name_at(ctx.doc, font, b"Encoding").as_deref(),
        Some("WinAnsiEncoding" | "MacRomanEncoding" | "StandardEncoding")
    )
}
