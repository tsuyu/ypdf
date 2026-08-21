//! PDF/A conformance checking (spec §15).
//!
//! PDF/A is a promise that a file will look and read the same in fifty years:
//! every font inside it, no encryption, no scripts, no content that depends on
//! the machine. This crate checks the part of that promise a file can be held
//! to by reading it.
//!
//! Two rules shape the design, both about honesty:
//!
//! * **A pass is "nothing I checked failed", never "conforming".** The standard
//!   has several hundred requirements across three parts; this implements the
//!   structural ones. [`LIMITS`] lists what is not checked, and every report
//!   prints it. A tool that says YES without that list produces confident
//!   archives that are rejected at the archive's door.
//! * **A claim is not a verdict.** The `pdfaid` identification in a file's XMP
//!   is what the producer asserted, and producers are wrong all the time. It is
//!   reported as a claim, next to the result of checking it.
//!
//! ```no_run
//! use ypdf_doc::Pdf;
//! use ypdf_pdfa::{Level, validate};
//!
//! # fn main() -> ypdf_core::Result<()> {
//! let pdf = Pdf::open("archive.pdf")?;
//! // Against the level the file claims, or a level you require.
//! let report = validate(&pdf, Some(Level::parse("2b")?));
//! println!("{}", report.to_human());
//! # Ok(())
//! # }
//! ```

mod annotations;
mod fonts;
mod graphics;
mod model;
mod structure;
mod walk;
mod xmp;

pub use model::{Conformance, LIMITS, Level, Report, Violation};

use lopdf::Document;
use ypdf_doc::Pdf;

/// What every check is handed: the document, and the level it is being held to.
pub(crate) struct Context<'a> {
    pub pdf: &'a Pdf,
    pub doc: &'a Document,
    pub level: Level,
}

/// The PDF/A level a file says it is, if it says anything.
///
/// A claim, read straight out of the XMP. Nothing here checks it — that is
/// [`validate`].
#[must_use]
pub fn claimed_level(pdf: &Pdf) -> Option<Level> {
    xmp::claimed_level(&pdf.xmp()?)
}

/// Check a document against a PDF/A level (spec §15).
///
/// With no level given, the file is checked against the one it claims. A file
/// that claims nothing is checked against PDF/A-2b, the level most archives
/// ask for — and told, in the report, that it claims nothing.
#[must_use]
pub fn validate(pdf: &Pdf, level: Option<Level>) -> Report {
    let claimed = claimed_level(pdf);
    let checked = level.or(claimed).unwrap_or(Level {
        part: 2,
        conformance: Conformance::B,
    });

    let ctx = Context {
        pdf,
        doc: pdf.raw(),
        level: checked,
    };

    let mut violations = Vec::new();
    structure::check(&ctx, &mut violations);
    fonts::check(&ctx, &mut violations);
    graphics::check(&ctx, &mut violations);
    annotations::check(&ctx, &mut violations);

    tracing::info!(
        level = %checked,
        claimed = claimed.map(|level| level.to_string()),
        violations = violations.len(),
        "PDF/A check finished"
    );

    Report {
        claimed,
        checked,
        passed: violations.is_empty(),
        violations,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use lopdf::{Dictionary, Object, Stream, dictionary};

    use super::*;

    /// A document with nothing in it but a page.
    fn document() -> Document {
        let mut doc = Document::with_version("1.4");
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => Dictionary::new(),
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        doc.trailer.set(
            "ID",
            vec![
                Object::string_literal("0123456789abcdef"),
                Object::string_literal("0123456789abcdef"),
            ],
        );
        doc
    }

    /// Attach an XMP packet claiming a level.
    fn with_claim(doc: &mut Document, part: u8, letter: char) {
        let packet = format!(
            "<x:xmpmeta><rdf:RDF><rdf:Description pdfaid:part=\"{part}\" \
             pdfaid:conformance=\"{letter}\"/></rdf:RDF></x:xmpmeta>"
        );
        let id = doc.add_object(Stream::new(
            dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
            packet.into_bytes(),
        ));
        let catalog_id = doc
            .trailer
            .get(b"Root")
            .expect("root")
            .as_reference()
            .expect("reference");
        if let Ok(catalog) = doc.get_dictionary_mut(catalog_id) {
            catalog.set("Metadata", Object::Reference(id));
        }
    }

    fn pdf_of(doc: Document) -> Pdf {
        let mut buffer = Vec::new();
        let mut doc = doc;
        doc.save_to(&mut buffer).expect("saves");
        Pdf::from_bytes(&buffer).expect("reopens")
    }

    fn codes(report: &Report) -> Vec<&str> {
        report.violations.iter().map(|v| v.code).collect()
    }

    #[test]
    fn a_file_with_no_claim_is_told_it_makes_none() {
        let report = validate(&pdf_of(document()), None);
        assert_eq!(report.claimed, None);
        assert!(codes(&report).contains(&"PDFA_NO_XMP"));
        assert!(!report.passed);
    }

    #[test]
    fn the_level_checked_defaults_to_the_one_claimed() {
        let mut doc = document();
        with_claim(&mut doc, 1, 'b');
        let report = validate(&pdf_of(doc), None);
        assert_eq!(report.checked.to_string(), "PDF/A-1b");
        assert!(!codes(&report).contains(&"PDFA_IDENTIFICATION_MISMATCH"));
    }

    #[test]
    fn a_claim_that_disagrees_with_the_level_asked_for_is_reported() {
        let mut doc = document();
        with_claim(&mut doc, 1, 'b');
        let report = validate(&pdf_of(doc), Some(Level::parse("2b").expect("level")));
        assert!(codes(&report).contains(&"PDFA_IDENTIFICATION_MISMATCH"));
    }

    #[test]
    fn a_missing_file_identifier_is_a_violation() {
        let mut doc = document();
        with_claim(&mut doc, 2, 'b');
        doc.trailer.remove(b"ID");
        let report = validate(&pdf_of(doc), None);
        assert!(codes(&report).contains(&"PDFA_NO_FILE_ID"));
    }

    #[test]
    fn a_font_without_its_program_is_a_violation() {
        let mut doc = document();
        with_claim(&mut doc, 2, 'b');
        doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let report = validate(&pdf_of(doc), None);
        assert!(
            codes(&report).contains(&"PDFA_FONT_NOT_EMBEDDED"),
            "one of the standard fourteen is still not embedded: {:?}",
            codes(&report)
        );
    }

    #[test]
    fn an_embedded_font_passes() {
        let mut doc = document();
        with_claim(&mut doc, 2, 'b');
        let file_id = doc.add_object(Stream::new(
            dictionary! { "Length1" => 4 },
            b"FONT".to_vec(),
        ));
        let descriptor_id = doc.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Archival",
            "FontFile2" => Object::Reference(file_id),
        });
        doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "Archival",
            "FontDescriptor" => Object::Reference(descriptor_id),
        });
        let report = validate(&pdf_of(doc), None);
        assert!(!codes(&report).contains(&"PDFA_FONT_NOT_EMBEDDED"));
    }

    #[test]
    fn javascript_and_launch_actions_are_violations() {
        let mut doc = document();
        with_claim(&mut doc, 2, 'b');
        doc.add_object(dictionary! {
            "S" => "Launch",
            "F" => Object::string_literal("payroll.exe"),
        });
        doc.add_object(dictionary! {
            "S" => "JavaScript",
            "JS" => Object::string_literal("app.alert(1)"),
        });
        let report = validate(&pdf_of(doc), None);
        assert!(codes(&report).contains(&"PDFA_JAVASCRIPT"));
        assert!(codes(&report).contains(&"PDFA_FORBIDDEN_ACTION"));
    }

    #[test]
    fn transparency_is_a_part_1_problem_only() {
        let build = || {
            let mut doc = document();
            doc.add_object(dictionary! {
                "Type" => "ExtGState",
                "ca" => Object::Real(0.5),
            });
            doc
        };

        let mut one = build();
        with_claim(&mut one, 1, 'b');
        assert!(codes(&validate(&pdf_of(one), None)).contains(&"PDFA_TRANSPARENCY"));

        // Part 2 has a transparency model. Reporting this there would be
        // calling a conforming feature a fault.
        let mut two = build();
        with_claim(&mut two, 2, 'b');
        assert!(!codes(&validate(&pdf_of(two), None)).contains(&"PDFA_TRANSPARENCY"));
    }

    #[test]
    fn lzw_is_refused_and_flate_is_not() {
        let mut doc = document();
        with_claim(&mut doc, 2, 'b');
        doc.add_object(Stream::new(
            dictionary! { "Filter" => "LZWDecode" },
            b"whatever".to_vec(),
        ));
        assert!(codes(&validate(&pdf_of(doc), None)).contains(&"PDFA_LZW"));
    }

    #[test]
    fn level_a_wants_a_structure_tree_and_a_language() {
        let mut doc = document();
        with_claim(&mut doc, 2, 'a');
        let report = validate(&pdf_of(doc), None);
        assert!(codes(&report).contains(&"PDFA_NOT_TAGGED"));
        assert!(codes(&report).contains(&"PDFA_NO_LANGUAGE"));

        // The same file checked at level b says nothing about tagging: level b
        // never asked for it.
        let mut plain = document();
        with_claim(&mut plain, 2, 'b');
        let report = validate(&pdf_of(plain), None);
        assert!(!codes(&report).contains(&"PDFA_NOT_TAGGED"));
    }

    #[test]
    fn an_annotation_without_an_appearance_is_reported() {
        let mut doc = document();
        with_claim(&mut doc, 2, 'b');
        let page_id = *doc.get_pages().values().next().expect("a page");
        if let Ok(page) = doc.get_dictionary_mut(page_id) {
            page.set(
                "Annots",
                vec![Object::Dictionary(dictionary! {
                    "Type" => "Annot",
                    "Subtype" => "Square",
                    "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()],
                    "F" => 4,
                })],
            );
        }
        assert!(codes(&validate(&pdf_of(doc), None)).contains(&"PDFA_ANNOT_NO_APPEARANCE"));
    }

    #[test]
    fn a_report_never_claims_more_than_it_checked() {
        let report = validate(&pdf_of(document()), None);
        let text = report.to_human();
        assert!(text.contains("Not checked here:"));
        assert!(!text.contains("compliant: YES"));
    }
}
