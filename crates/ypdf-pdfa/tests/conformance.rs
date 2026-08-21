//! A document built to pass, and then broken one requirement at a time.
//!
//! The unit tests prove each check fires. This proves the other half: that a
//! file which meets the requirements is not reported as failing. A checker
//! that flags everything is as useless as one that flags nothing, and only a
//! passing document catches that.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use lopdf::{Dictionary, Document, Object, Stream, dictionary};
use ypdf_doc::Pdf;
use ypdf_pdfa::{Level, Report, validate};

/// A minimal document that meets every requirement this crate checks for
/// PDF/A-2b: identified in XMP, a file identifier, an embedded font, an output
/// intent carrying its profile, and no scripts, attachments, or LZW.
fn conforming() -> Document {
    let mut doc = Document::with_version("1.7");

    let font_file = doc.add_object(Stream::new(
        dictionary! { "Length1" => 4 },
        b"FONT".to_vec(),
    ));
    let descriptor = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "Archival",
        "Flags" => 32,
        "FontFile2" => Object::Reference(font_file),
    });
    let font = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "TrueType",
        "BaseFont" => "Archival",
        "Encoding" => "WinAnsiEncoding",
        "FontDescriptor" => Object::Reference(descriptor),
    });

    let pages_id = doc.new_object_id();
    let content = doc.add_object(Stream::new(
        Dictionary::new(),
        b"BT /F1 12 Tf 72 720 Td (Archived) Tj ET".to_vec(),
    ));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => Object::Reference(content),
        "Resources" => dictionary! {
            "Font" => dictionary! { "F1" => Object::Reference(font) },
        },
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );

    // The output intent, with the profile inside the file rather than named.
    let profile = doc.add_object(Stream::new(
        dictionary! { "N" => 3 },
        b"(not a real ICC profile)".to_vec(),
    ));
    let intent = doc.add_object(dictionary! {
        "Type" => "OutputIntent",
        "S" => "GTS_PDFA1",
        "OutputConditionIdentifier" => Object::string_literal("sRGB"),
        "DestOutputProfile" => Object::Reference(profile),
    });

    let metadata = doc.add_object(Stream::new(
        dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
        br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF
             xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
             <rdf:Description rdf:about="" pdfaid:part="2" pdfaid:conformance="B"
              xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/"/>
            </rdf:RDF></x:xmpmeta>"#
            .to_vec(),
    ));

    let catalog = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "Metadata" => Object::Reference(metadata),
        "OutputIntents" => vec![Object::Reference(intent)],
    });
    doc.trailer.set("Root", catalog);
    doc.trailer.set(
        "ID",
        vec![
            Object::string_literal("fedcba9876543210"),
            Object::string_literal("fedcba9876543210"),
        ],
    );
    doc
}

fn check(doc: Document) -> Report {
    let mut doc = doc;
    let mut buffer = Vec::new();
    doc.save_to(&mut buffer).expect("saves");
    let pdf = Pdf::from_bytes(&buffer).expect("reopens");
    validate(&pdf, None)
}

fn codes(report: &Report) -> Vec<&str> {
    report.violations.iter().map(|v| v.code).collect()
}

#[test]
fn a_document_that_meets_the_requirements_passes() {
    let report = check(conforming());
    assert!(
        report.passed,
        "a document built to conform was reported as failing: {:?}",
        report
            .violations
            .iter()
            .map(|v| format!("{}: {}", v.code, v.message))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        report.claimed.map(|l| l.to_string()).as_deref(),
        Some("PDF/A-2b")
    );
    assert_eq!(report.checked.to_string(), "PDF/A-2b");
}

#[test]
fn the_verdict_is_never_stated_without_its_limits() {
    // The one thing this crate must never do is answer YES on its own
    // authority. Even a clean pass prints what it did not look at.
    let text = check(conforming()).to_human();
    assert!(text.contains("Not checked here:"));
    assert!(text.contains("not a certificate of conformance"));
}

#[test]
fn taking_the_font_program_out_fails_the_same_document() {
    let mut doc = conforming();
    let descriptor_id = doc
        .objects
        .iter()
        .find(|(_, object)| {
            object.as_dict().is_ok_and(|dict| {
                dict.get(b"Type")
                    .and_then(Object::as_name)
                    .is_ok_and(|name| name == b"FontDescriptor")
            })
        })
        .map(|(id, _)| *id)
        .expect("a descriptor");
    doc.get_dictionary_mut(descriptor_id)
        .expect("descriptor")
        .remove(b"FontFile2");

    let report = check(doc);
    assert!(!report.passed);
    assert_eq!(codes(&report), vec!["PDFA_FONT_NOT_EMBEDDED"]);
}

#[test]
fn taking_the_output_intent_out_fails_a_document_that_paints_in_device_colour() {
    let mut doc = conforming();
    // Paint something in DeviceRGB, then remove the profile that says what
    // that red actually is.
    let content_id = doc
        .objects
        .iter()
        .find(|(_, object)| {
            object
                .as_stream()
                .is_ok_and(|stream| stream.content.starts_with(b"BT"))
        })
        .map(|(id, _)| *id)
        .expect("page content");
    if let Ok(stream) = doc
        .get_object_mut(content_id)
        .and_then(Object::as_stream_mut)
    {
        stream.set_content(b"1 0 0 rg 0 0 100 100 re f".to_vec());
    }
    let catalog_id = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
    doc.get_dictionary_mut(catalog_id)
        .expect("catalog")
        .remove(b"OutputIntents");

    let report = check(doc);
    assert!(codes(&report).contains(&"PDFA_NO_OUTPUT_INTENT"));
}

#[test]
fn a_conforming_file_checked_against_a_level_it_does_not_claim_says_so() {
    let mut doc = conforming();
    let mut buffer = Vec::new();
    doc.save_to(&mut buffer).expect("saves");
    let pdf = Pdf::from_bytes(&buffer).expect("reopens");

    let report = validate(&pdf, Some(Level::parse("1b").expect("level")));
    assert!(codes(&report).contains(&"PDFA_IDENTIFICATION_MISMATCH"));
    // And the part-1 rules are the ones applied: this file is PDF 1.7.
    assert!(codes(&report).contains(&"PDFA_VERSION"));
}

#[test]
fn an_attachment_makes_the_same_file_pass_as_part_3_and_fail_as_part_1() {
    let mut doc = conforming();
    let attached = doc.add_object(Stream::new(
        dictionary! { "Type" => "EmbeddedFile" },
        b"invoice data".to_vec(),
    ));
    let spec = doc.add_object(dictionary! {
        "Type" => "Filespec",
        "F" => Object::string_literal("invoice.xml"),
        "AFRelationship" => "Source",
        "EF" => dictionary! { "F" => Object::Reference(attached) },
    });
    let catalog_id = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
    if let Ok(catalog) = doc.get_dictionary_mut(catalog_id) {
        catalog.set(
            "Names",
            dictionary! { "EmbeddedFiles" => dictionary! {
                "Names" => vec![Object::string_literal("invoice.xml"), Object::Reference(spec)],
            }},
        );
    }

    let mut buffer = Vec::new();
    doc.save_to(&mut buffer).expect("saves");
    let pdf = Pdf::from_bytes(&buffer).expect("reopens");

    // Part 3 exists precisely so a document can carry the data it was made
    // from, provided it says how the two relate.
    let three = validate(&pdf, Some(Level::parse("3b").expect("level")));
    assert!(!codes(&three).contains(&"PDFA_EMBEDDED_FILE"));
    assert!(!codes(&three).contains(&"PDFA_NO_AF_RELATIONSHIP"));

    let one = validate(&pdf, Some(Level::parse("1b").expect("level")));
    assert!(codes(&one).contains(&"PDFA_EMBEDDED_FILE"));
}
