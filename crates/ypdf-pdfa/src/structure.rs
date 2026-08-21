//! File-level requirements: identification, encryption, actions, filters.

use lopdf::content::Content;
use lopdf::{Document, Object};

use crate::model::Violation;
use crate::walk::{array_at, bool_at, dict_at, dictionaries, filters, is_type, name_at, streams};
use crate::{Context, xmp};

/// Actions that reach outside the page.
///
/// PDF/A is a promise that the file will look and behave the same in fifty
/// years. An action that runs a program, plays a sound, or submits a form is a
/// promise about the *machine*, which nobody can keep.
const FORBIDDEN_ACTIONS: [&[u8]; 11] = [
    b"Launch",
    b"Sound",
    b"Movie",
    b"ResetForm",
    b"ImportData",
    b"JavaScript",
    b"Rendition",
    b"Trans",
    b"GoTo3DView",
    b"SetState",
    b"NoOp",
];

pub fn check(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    identification(ctx, out);
    encryption(ctx, out);
    version(ctx, out);
    output_intents(ctx, out);
    actions(ctx, out);
    embedded_files(ctx, out);
    optional_content(ctx, out);
    stream_rules(ctx, out);
    form_appearances(ctx, out);
}

/// The XMP that says what the file claims to be.
fn identification(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    let Some(packet) = ctx.pdf.xmp() else {
        out.push(Violation::new(
            "PDFA_NO_XMP",
            "XMP metadata present",
            "the document has no XMP metadata stream; PDF/A identifies itself in XMP, so a \
             file without it cannot be conforming whatever else is right",
        ));
        return;
    };

    match xmp::claimed_level(&packet) {
        None => out.push(Violation::new(
            "PDFA_NO_IDENTIFICATION",
            "PDF/A identification in XMP",
            "the XMP carries no pdfaid:part and pdfaid:conformance, so the file does not \
             identify itself as PDF/A",
        )),
        Some(claimed) if claimed != ctx.level => out.push(Violation::new(
            "PDFA_IDENTIFICATION_MISMATCH",
            "PDF/A identification matches the level checked",
            format!(
                "the file identifies itself as {claimed}, and it was checked against {}",
                ctx.level
            ),
        )),
        Some(_) => {}
    }

    // The title in two places has to be the same title. A catalogue that reads
    // one and a reader that shows the other is exactly the confusion archiving
    // is meant to prevent.
    if let (Some(info_title), Some(xmp_title)) = (ctx.pdf.metadata().title, xmp::title(&packet))
        && info_title.trim() != xmp_title.trim()
    {
        out.push(Violation::new(
            "PDFA_XMP_TITLE_MISMATCH",
            "XMP and document information agree",
            format!(
                "/Info gives the title as `{}` and the XMP gives it as `{}`",
                info_title.trim(),
                xmp_title.trim()
            ),
        ));
    }
}

fn encryption(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    // `was_encrypted` and not the trailer: opening a protected file decrypts
    // it in memory and takes /Encrypt out, so by the time anything is checked
    // the trailer looks innocent.
    if ctx.pdf.was_encrypted() || ctx.doc.trailer.get(b"Encrypt").is_ok() {
        out.push(Violation::new(
            "PDFA_ENCRYPTED",
            "no encryption",
            "the file is encrypted; PDF/A forbids it, because an archive that needs a \
             password or a working cipher to be read is not archived",
        ));
    }

    if ctx.doc.trailer.get(b"ID").is_err() {
        out.push(Violation::new(
            "PDFA_NO_FILE_ID",
            "file identifier present",
            "the trailer has no /ID; without it two versions of a document cannot be told \
             apart",
        ));
    }
}

fn version(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    let highest = if ctx.level.part == 1 { "1.4" } else { "1.7" };
    let numeric = |text: &str| -> f32 { text.trim().parse::<f32>().unwrap_or(0.0) };

    if numeric(&ctx.doc.version) > numeric(highest) {
        out.push(Violation::new(
            "PDFA_VERSION",
            "PDF version within the part",
            format!(
                "the header says PDF {}, and {} is built on PDF {highest} and earlier",
                ctx.doc.version, ctx.level
            ),
        ));
    }
}

/// The colour promise: a device colour is meaningless without a profile that
/// says what that device was.
fn output_intents(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    let intents: Vec<&lopdf::Dictionary> = ctx
        .doc
        .catalog()
        .ok()
        .and_then(|catalog| array_at(ctx.doc, catalog, b"OutputIntents"))
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match crate::walk::resolve(ctx.doc, item) {
                    Some(Object::Dictionary(dict)) => Some(dict),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    let pdfa_intents: Vec<&&lopdf::Dictionary> = intents
        .iter()
        .filter(|dict| name_at(ctx.doc, dict, b"S").as_deref() == Some("GTS_PDFA1"))
        .collect();

    if pdfa_intents.is_empty() {
        if uses_device_colour(ctx.doc) {
            out.push(Violation::new(
                "PDFA_NO_OUTPUT_INTENT",
                "output intent for device colour",
                "the pages use device colour (DeviceRGB, DeviceGray, or DeviceCMYK) and the \
                 catalog has no GTS_PDFA1 output intent, so nothing in the file says what \
                 those numbers mean",
            ));
        }
        return;
    }

    if pdfa_intents
        .iter()
        .any(|dict| dict.get(b"DestOutputProfile").is_err())
    {
        out.push(Violation::new(
            "PDFA_OUTPUT_INTENT_NO_PROFILE",
            "output intent carries its ICC profile",
            "an output intent names a colour space but embeds no /DestOutputProfile; the \
             profile has to travel with the file",
        ));
    }

    if pdfa_intents.len() > 1 {
        out.push(Violation::counted(
            "PDFA_MIXED_OUTPUT_INTENTS",
            "one output intent",
            "the catalog has more than one GTS_PDFA1 output intent; they cannot all be the \
             destination profile",
            pdfa_intents.len(),
        ));
    }
}

fn actions(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    let mut forbidden: Vec<String> = Vec::new();
    let mut javascript = 0;

    for dict in dictionaries(ctx.doc) {
        if dict.has(b"JS") || dict.has(b"JavaScript") {
            javascript += 1;
        }
        if let Ok(Object::Name(kind)) = dict.get(b"S")
            && FORBIDDEN_ACTIONS.contains(&kind.as_slice())
        {
            forbidden.push(String::from_utf8_lossy(kind).into_owned());
        }
        // An additional-actions dictionary fires on events — opening a page,
        // leaving a field — which is behaviour, and behaviour is not archived.
        if dict.has(b"AA") {
            forbidden.push("AA".to_string());
        }
    }

    if javascript > 0 {
        out.push(Violation::counted(
            "PDFA_JAVASCRIPT",
            "no JavaScript",
            "the document carries JavaScript; what it does depends on the reader, which is \
             the opposite of what an archived file promises",
            javascript,
        ));
    }

    if !forbidden.is_empty() {
        forbidden.sort_unstable();
        forbidden.dedup();
        out.push(Violation::counted(
            "PDFA_FORBIDDEN_ACTION",
            "no actions that leave the document",
            format!(
                "actions the standard does not permit: {}",
                forbidden.join(", ")
            ),
            forbidden.len(),
        ));
    }
}

fn embedded_files(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    let specs: Vec<&lopdf::Dictionary> = dictionaries(ctx.doc)
        .into_iter()
        .filter(|dict| is_type(dict, b"Filespec") && dict.has(b"EF"))
        .collect();
    if specs.is_empty() {
        return;
    }

    match ctx.level.part {
        1 => out.push(Violation::counted(
            "PDFA_EMBEDDED_FILE",
            "no embedded files",
            "the document carries embedded files, which PDF/A-1 does not allow at all",
            specs.len(),
        )),
        2 => {
            // Part 2 allows an attachment only if the attachment is itself
            // PDF/A. Checked by its own identification, not validated in full.
            let mut not_pdfa = 0;
            for spec in &specs {
                if !embedded_claims_pdfa(ctx.doc, spec) {
                    not_pdfa += 1;
                }
            }
            if not_pdfa > 0 {
                out.push(Violation::counted(
                    "PDFA_EMBEDDED_FILE_NOT_PDFA",
                    "embedded files are themselves PDF/A",
                    "the document carries attachments that do not identify themselves as \
                     PDF/A; PDF/A-2 only allows attachments that are conforming files",
                    not_pdfa,
                ));
            }
        }
        _ => {
            let missing = specs
                .iter()
                .filter(|spec| !spec.has(b"AFRelationship"))
                .count();
            if missing > 0 {
                out.push(Violation::counted(
                    "PDFA_NO_AF_RELATIONSHIP",
                    "attachments say how they relate to the document",
                    "an attachment has no /AFRelationship; PDF/A-3 allows any file to be \
                     attached, on condition the file says what it is to the document",
                    missing,
                ));
            }
        }
    }
}

/// Does an attachment identify itself as PDF/A?
fn embedded_claims_pdfa(doc: &Document, spec: &lopdf::Dictionary) -> bool {
    let Some(ef) = dict_at(doc, spec, b"EF") else {
        return false;
    };
    let Some(stream) = ef
        .get(b"F")
        .ok()
        .and_then(|value| crate::walk::resolve(doc, value))
        .and_then(|object| object.as_stream().ok())
    else {
        return false;
    };
    let bytes = stream
        .decompressed_content()
        .unwrap_or_else(|_| stream.content.clone());
    // Read the attachment as a document rather than grepping the bytes: a
    // string containing "pdfaid:part" proves nothing about the file's own XMP.
    ypdf_doc::Pdf::from_bytes(&bytes).is_ok_and(|inner| {
        inner
            .xmp()
            .and_then(|packet| xmp::claimed_level(&packet))
            .is_some()
    })
}

fn optional_content(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    if ctx.level.part != 1 {
        return;
    }
    if ctx
        .doc
        .catalog()
        .is_ok_and(|catalog| catalog.has(b"OCProperties"))
    {
        out.push(Violation::new(
            "PDFA_OPTIONAL_CONTENT",
            "no optional content",
            "the document has optional content groups; in PDF/A-1 what is on the page must \
             not depend on which layers a reader decides to show",
        ));
    }
}

fn stream_rules(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    let mut lzw = 0;
    let mut external = 0;

    for stream in streams(ctx.doc) {
        if filters(ctx.doc, &stream.dict)
            .iter()
            .any(|filter| filter == "LZWDecode")
        {
            lzw += 1;
        }
        // /F on a *stream* means the data lives in another file. An archive
        // whose content is somewhere else is not an archive.
        if stream.dict.has(b"F") && !stream.dict.has(b"Subtype") {
            external += 1;
        }
    }

    if lzw > 0 {
        out.push(Violation::counted(
            "PDFA_LZW",
            "no LZW compression",
            "streams are compressed with LZWDecode, which PDF/A does not permit; Flate is \
             the replacement",
            lzw,
        ));
    }
    if external > 0 {
        out.push(Violation::counted(
            "PDFA_EXTERNAL_STREAM",
            "content is inside the file",
            "streams refer to data in another file; everything a conforming file needs has \
             to be in it",
            external,
        ));
    }
}

fn form_appearances(ctx: &Context<'_>, out: &mut Vec<Violation>) {
    let Ok(catalog) = ctx.doc.catalog() else {
        return;
    };
    let Some(acroform) = dict_at(ctx.doc, catalog, b"AcroForm") else {
        return;
    };
    if bool_at(ctx.doc, acroform, b"NeedAppearances") == Some(true) {
        out.push(Violation::new(
            "PDFA_NEED_APPEARANCES",
            "form fields carry their own appearance",
            "/NeedAppearances is true, which asks the reader to draw the fields itself; the \
             file has to carry what it looks like",
        ));
    }
}

/// Do the pages paint in a device colour space?
///
/// Only asked when there is no output intent, and stops at the first one
/// found: this decodes content streams, which is the expensive part of the
/// whole check.
fn uses_device_colour(doc: &Document) -> bool {
    const DEVICE_OPERATORS: [&str; 6] = ["g", "rg", "k", "G", "RG", "K"];

    for page_id in doc.get_pages().values() {
        let bytes = doc.get_page_content(*page_id);
        let Ok(content) = Content::decode(&bytes) else {
            continue;
        };
        if content
            .operations
            .iter()
            .any(|operation| DEVICE_OPERATORS.contains(&operation.operator.as_str()))
        {
            return true;
        }
    }

    // An image can name a device space without any operator saying so.
    streams(doc).iter().any(|stream| {
        matches!(
            name_at(doc, &stream.dict, b"ColorSpace").as_deref(),
            Some("DeviceRGB" | "DeviceGray" | "DeviceCMYK")
        )
    })
}
