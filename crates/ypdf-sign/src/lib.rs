//! Digital signatures (spec §9): reading, and checking what they cover.
//!
//! A PDF signature answers one question honestly and several others only if
//! you are careful: *these bytes have not changed since someone signed them
//! with the private key belonging to this certificate*. Everything people
//! actually want to know — is this really them, was it signed before the
//! deadline, has the certificate been revoked — needs more than the file.
//!
//! Three rules shape the design:
//!
//! * **Trust is never asserted.** There is no trust store, and revocation
//!   checking needs network access this tool does not have. [`TRUST_LIMITS`] is
//!   printed with every report, so an intact signature is never mistaken for a
//!   trusted one.
//! * **"Cannot check" and "invalid" are different answers.** An algorithm this
//!   does not implement is reported as unchecked. Calling it a bad signature
//!   would be a lie in the direction that makes people panic; calling it fine
//!   would be a lie in the direction that gets documents accepted.
//! * **The byte range is checked as hard as the maths.** Most signature fraud
//!   is not cryptographic: it is a valid signature over a small honest part of
//!   a file, with the rest left uncovered.
//!
//! ```no_run
//! use ypdf_sign::verify_file;
//!
//! # fn main() -> ypdf_core::Result<()> {
//! let report = verify_file("contract.pdf")?;
//! println!("{}", report.to_human());
//! # Ok(())
//! # }
//! ```

mod certificate;
mod coverage;
mod fields;
mod model;
mod pkcs7;

pub use certificate::to_der as certificate_der;
pub use model::{CertificateInfo, Coverage, Note, Report, Signature, TRUST_LIMITS, Verdict};

use std::path::Path;

use ypdf_core::{Error, Result};
use ypdf_doc::Pdf;

use crate::coverage::ByteRange;
use crate::pkcs7::Outcome;

/// Check every signature in a file (spec §9).
///
/// Takes a path rather than an open document because verification is about the
/// bytes on disk: a signature covers byte ranges of the file it was made in,
/// and a re-serialized copy is a different file even when it says the same
/// thing.
pub fn verify_file(path: impl AsRef<Path>) -> Result<Report> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|source| Error::Io {
        path: Some(path.to_path_buf()),
        source,
    })?;
    let pdf = Pdf::open(path)?;
    Ok(verify(&pdf, &bytes))
}

/// Check every signature in a document against the bytes it came from.
///
/// `bytes` must be the file as it was read. Handing in a re-saved copy is not
/// an error and will simply report every signature as covering an altered
/// document — which, of that copy, is true.
#[must_use]
pub fn verify(pdf: &Pdf, bytes: &[u8]) -> Report {
    let doc = pdf.raw();
    let mut report = Report::default();

    for found in fields::signature_fields(doc) {
        let Some(value) = found.value else {
            report.empty_fields.push(found.name);
            continue;
        };

        let sub_filter = value
            .get(b"SubFilter")
            .and_then(lopdf::Object::as_name)
            .map_or_else(
                |_| "(not stated)".to_string(),
                |name| String::from_utf8_lossy(name).into_owned(),
            );

        let mut notes = Vec::new();
        let mut certificate = None;
        let mut chain = Vec::new();
        let mut timestamped = false;

        let contents = value
            .get(b"Contents")
            .and_then(lopdf::Object::as_str)
            .map(<[u8]>::to_vec)
            .unwrap_or_default();

        let (coverage, verdict) = match ByteRange::read(&value) {
            None => (
                Coverage::PartOfFile {
                    signed: 0,
                    total: bytes.len(),
                },
                Verdict::Unreadable {
                    detail: "the signature has no readable /ByteRange, so there is nothing \
                             saying which bytes it covers"
                        .to_string(),
                },
            ),
            Some(range) => {
                let (coverage, range_notes) = coverage::assess(&range, contents.len(), bytes);
                notes.extend(range_notes);

                match range.extract(bytes) {
                    None => (
                        coverage,
                        Verdict::Unreadable {
                            detail: "the byte range does not fit inside the file".to_string(),
                        },
                    ),
                    Some(signed_bytes) => {
                        let parsed = pkcs7::verify(&contents, &signed_bytes);
                        notes.extend(parsed.notes);
                        certificate = parsed.certificate.as_ref().map(certificate::describe);
                        chain = parsed.chain.iter().map(certificate::describe).collect();
                        timestamped = parsed.timestamped;
                        (coverage, verdict_of(parsed.outcome))
                    }
                }
            }
        };

        // A signature over part of the file can still be cryptographically
        // intact. Saying only "intact" would be true and misleading, so the
        // note travels with it and the report prints both.
        if matches!(coverage, Coverage::PartOfFile { .. }) && verdict.is_intact() {
            notes.push(Note::new(
                "SIG_INTACT_BUT_PARTIAL",
                "the signature is intact for the bytes it covers — and it does not cover the \
                 whole file",
            ));
        }

        report.signatures.push(Signature {
            field: found.name,
            page: found.page,
            declared_name: fields::text(&value, b"Name"),
            reason: fields::text(&value, b"Reason"),
            location: fields::text(&value, b"Location"),
            claimed_time: fields::text(&value, b"M"),
            sub_filter,
            certification: fields::is_certification(doc, &value),
            timestamped,
            coverage,
            verdict,
            certificate,
            chain,
            notes,
        });
    }

    // In file order: the first signature covers least, each later one covers
    // everything before it.
    report
        .signatures
        .sort_by_key(|signature| match signature.coverage {
            Coverage::WholeFile => usize::MAX,
            Coverage::PartOfFile { signed, .. } => signed,
        });

    tracing::info!(
        signatures = report.signatures.len(),
        empty_fields = report.empty_fields.len(),
        all_intact = report.all_intact(),
        "signatures checked"
    );

    report
}

/// Is the document signed at all?
///
/// Cheap: reads the field tree and stops there, with no cryptography.
#[must_use]
pub fn is_signed(pdf: &Pdf) -> bool {
    fields::signature_fields(pdf.raw())
        .iter()
        .any(|field| field.value.is_some())
}

fn verdict_of(outcome: Outcome) -> Verdict {
    match outcome {
        Outcome::Intact => Verdict::Intact,
        Outcome::DigestMismatch => Verdict::DocumentAltered,
        Outcome::SignatureInvalid => Verdict::SignatureBroken,
        Outcome::Unsupported(detail) => Verdict::Unsupported { detail },
        Outcome::Unreadable(detail) => Verdict::Unreadable { detail },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use lopdf::{Document, Object, dictionary};

    use super::*;

    /// A page tree with one page in it, since a document without pages is not
    /// one `ypdf-doc` will open.
    fn one_page(doc: &mut Document) -> lopdf::ObjectId {
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        pages_id
    }

    /// A document with one signature field, signed with `contents`.
    fn signed_document(byte_range: Vec<i64>, contents: Vec<u8>) -> (Pdf, Vec<u8>) {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );

        let signature = doc.add_object(dictionary! {
            "Type" => "Sig",
            "Filter" => "Adobe.PPKLite",
            "SubFilter" => "adbe.pkcs7.detached",
            "Name" => Object::string_literal("A. Signer"),
            "Reason" => Object::string_literal("I agree"),
            "M" => Object::string_literal("D:20260101120000Z"),
            "ByteRange" => byte_range.into_iter().map(Object::Integer).collect::<Vec<_>>(),
            "Contents" => Object::String(contents, lopdf::StringFormat::Hexadecimal),
        });
        let field = doc.add_object(dictionary! {
            "FT" => "Sig",
            "T" => Object::string_literal("Signature1"),
            "V" => Object::Reference(signature),
            "P" => Object::Reference(page_id),
        });
        let catalog = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
            "AcroForm" => dictionary! { "Fields" => vec![Object::Reference(field)] },
        });
        doc.trailer.set("Root", catalog);

        let mut buffer = Vec::new();
        doc.save_to(&mut buffer).expect("saves");
        let pdf = Pdf::from_bytes(&buffer).expect("reopens");
        (pdf, buffer)
    }

    #[test]
    fn an_unsigned_document_is_reported_as_unsigned() {
        let mut doc = Document::with_version("1.7");
        let pages_id = one_page(&mut doc);
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        let mut buffer = Vec::new();
        doc.save_to(&mut buffer).expect("saves");
        let pdf = Pdf::from_bytes(&buffer).expect("reopens");

        let report = verify(&pdf, &buffer);
        assert!(!report.is_signed());
        assert!(!report.all_intact(), "unsigned is not the same as verified");
    }

    #[test]
    fn a_signature_that_is_not_cms_is_unreadable_not_forged() {
        let (pdf, bytes) = signed_document(vec![0, 100, 400, 100], b"nonsense".to_vec());
        let report = verify(&pdf, &bytes);

        assert_eq!(report.signatures.len(), 1);
        assert!(matches!(
            report.signatures[0].verdict,
            Verdict::Unreadable { .. }
        ));
        assert_eq!(
            report.signatures[0].declared_name.as_deref(),
            Some("A. Signer")
        );
        assert_eq!(report.signatures[0].reason.as_deref(), Some("I agree"));
    }

    #[test]
    fn a_byte_range_that_does_not_fit_the_file_is_refused() {
        // Rather than verifying whatever bytes happen to be there.
        let (pdf, bytes) = signed_document(vec![0, 10, 20, 900_000], b"x".to_vec());
        let report = verify(&pdf, &bytes);
        assert!(matches!(
            report.signatures[0].verdict,
            Verdict::Unreadable { .. }
        ));
    }

    #[test]
    fn an_empty_signature_field_is_listed_separately() {
        let mut doc = Document::with_version("1.7");
        let field = doc.add_object(dictionary! {
            "FT" => "Sig",
            "T" => Object::string_literal("CountersignHere"),
        });
        let pages_id = one_page(&mut doc);
        let catalog = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
            "AcroForm" => dictionary! { "Fields" => vec![Object::Reference(field)] },
        });
        doc.trailer.set("Root", catalog);
        let mut buffer = Vec::new();
        doc.save_to(&mut buffer).expect("saves");
        let pdf = Pdf::from_bytes(&buffer).expect("reopens");

        let report = verify(&pdf, &buffer);
        assert!(report.signatures.is_empty());
        assert_eq!(report.empty_fields, vec!["CountersignHere".to_string()]);
        assert!(!is_signed(&pdf), "an empty field is not a signature");
    }

    #[test]
    fn appended_content_is_reported_even_when_the_signature_cannot_be_read() {
        // The coverage check does not depend on the cryptography, and must
        // still fire when the blob is nonsense.
        let (pdf, mut bytes) = signed_document(vec![0, 100, 400, 100], b"nonsense".to_vec());
        bytes.extend_from_slice(&vec![b'x'; 5_000]);
        let report = verify(&pdf, &bytes);

        assert!(matches!(
            report.signatures[0].coverage,
            Coverage::PartOfFile { .. }
        ));
        assert!(
            report.signatures[0]
                .notes
                .iter()
                .any(|note| note.code == "SIG_COVERS_EARLIER_REVISION")
        );
    }
}
