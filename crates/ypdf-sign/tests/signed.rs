//! A genuinely signed PDF, made here and then checked.
//!
//! The unit tests prove the failure paths. This proves the one that matters
//! most and is easiest to get subtly wrong: that a real detached CMS signature
//! over a real `/ByteRange`, made with a real key, comes back **intact** — and
//! that changing one byte of the document afterwards does not.
//!
//! The document is assembled as bytes rather than through `lopdf`, because a
//! signature is about byte offsets: the `/ByteRange` has to name the exact
//! positions of the hole its own `/Contents` sits in, which means the file has
//! to be laid out before it can be signed, and then patched in place without
//! moving anything.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use cms::builder::{SignedDataBuilder, SignerInfoBuilder};
use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::signed_data::{EncapsulatedContentInfo, SignerIdentifier};
use const_oid::ObjectIdentifier;
use der::Encode;
use p256::ecdsa::SigningKey;
use p256::elliptic_curve::rand_core::OsRng;
use sha2::{Digest, Sha256};
use x509_cert::Certificate;
use x509_cert::builder::{Builder, CertificateBuilder, Profile};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::{
    AlgorithmIdentifierOwned, DynSignatureAlgorithmIdentifier, SubjectPublicKeyInfoOwned,
};
use x509_cert::time::Validity;
use ypdf_sign::{Coverage, Verdict, verify_file};

const ID_DATA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.1");
const SHA256_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1");
const ECDSA_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");

/// A P-256 key that the CMS and certificate builders will accept.
///
/// The builders want a signer that can name its own algorithm; `ecdsa` stops
/// short of that, so this says ECDSA-with-SHA-256 for it.
struct Signer(SigningKey);

impl signature::Keypair for Signer {
    type VerifyingKey = p256::ecdsa::VerifyingKey;

    fn verifying_key(&self) -> Self::VerifyingKey {
        *self.0.verifying_key()
    }
}

impl DynSignatureAlgorithmIdentifier for Signer {
    fn signature_algorithm_identifier(&self) -> x509_cert::spki::Result<AlgorithmIdentifierOwned> {
        Ok(AlgorithmIdentifierOwned {
            oid: ECDSA_SHA256,
            parameters: None,
        })
    }
}

impl signature::Signer<p256::ecdsa::DerSignature> for Signer {
    fn try_sign(&self, message: &[u8]) -> Result<p256::ecdsa::DerSignature, signature::Error> {
        signature::Signer::<p256::ecdsa::Signature>::try_sign(&self.0, message)
            .map(|signature| signature.to_der())
    }
}

fn certificate(signer: &Signer) -> Certificate {
    let subject: Name = "CN=Ada Lovelace,O=Analytical Engines,C=GB"
        .parse()
        .expect("a name");
    let public_key =
        SubjectPublicKeyInfoOwned::from_key(*signer.0.verifying_key()).expect("a public key");

    CertificateBuilder::new(
        Profile::Root,
        SerialNumber::from(42u32),
        Validity::from_now(std::time::Duration::from_secs(3600 * 24 * 365)).expect("validity"),
        subject,
        public_key,
        signer,
    )
    .expect("a builder")
    .build::<p256::ecdsa::DerSignature>()
    .expect("a certificate")
}

/// A detached CMS SignedData over `message`.
fn detached_signature(signer: &Signer, cert: &Certificate, message: &[u8]) -> Vec<u8> {
    let digest = Sha256::digest(message);

    // Detached: the content is the PDF, which stays in the PDF.
    let encapsulated = EncapsulatedContentInfo {
        econtent_type: ID_DATA,
        econtent: None,
    };

    let sid = SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
        issuer: cert.tbs_certificate.issuer.clone(),
        serial_number: cert.tbs_certificate.serial_number.clone(),
    });

    let digest_algorithm = AlgorithmIdentifierOwned {
        oid: SHA256_OID,
        parameters: None,
    };

    // The builder adds the message-digest and content-type attributes itself.
    let signer_info = SignerInfoBuilder::new(
        signer,
        sid,
        digest_algorithm.clone(),
        &encapsulated,
        Some(&digest),
    )
    .expect("a signer info builder");

    let mut builder = SignedDataBuilder::new(&encapsulated);
    let content = builder
        .add_digest_algorithm(digest_algorithm)
        .expect("digest algorithm")
        .add_certificate(CertificateChoices::Certificate(cert.clone()))
        .expect("certificate")
        .add_signer_info::<Signer, p256::ecdsa::DerSignature>(signer_info)
        .expect("signer info")
        .build()
        .expect("signed data");

    assert_eq!(
        content.content_type,
        ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.2")
    );
    content.to_der().expect("DER")
}

/// Room reserved in the file for the signature, in bytes.
const RESERVED: usize = 4096;

/// A signed file, and where its signature sits in it.
struct Signed {
    bytes: Vec<u8>,
    /// Offset of the `<` that opens `/Contents`.
    hole_start: usize,
    /// Length of the DER actually written, before the padding.
    cms_len: usize,
    /// The `startxref` trailer, repeated when appending to keep the file
    /// readable — a real incremental update writes its own.
    trailer: String,
}

/// Build a signed PDF.
fn signed_pdf() -> Signed {
    let signer = Signer(SigningKey::random(&mut OsRng));
    let cert = certificate(&signer);

    // The `/ByteRange` numbers are written at a fixed width so that filling
    // them in later cannot move a single byte of the file.
    let objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [4 0 R] /SigFlags 3 >> >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into(),
        "<< /FT /Sig /T (Signature1) /V 5 0 R /P 3 0 R /Ff 0 >>".into(),
        format!(
            "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached \
             /Name (Ada Lovelace) /Reason (I agree) /M (D:20260822120000Z) \
             /ByteRange [0 {:010} {:010} {:010}] /Contents <{}> >>",
            0,
            0,
            0,
            "0".repeat(RESERVED * 2)
        ),
    ];

    let mut file = String::from("%PDF-1.7\n");
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(file.len());
        file.push_str(&format!("{} 0 obj\n{body}\nendobj\n", index + 1));
    }

    let xref_at = file.len();
    file.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    ));
    for offset in &offsets {
        file.push_str(&format!("{offset:010} 00000 n \n"));
    }
    file.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R /ID [<41424344> <41424344>] >>\nstartxref\n{xref_at}\n%%EOF\n",
        objects.len() + 1
    ));

    let mut bytes = file.into_bytes();

    // Where the signature will live, and therefore what it can cover.
    let hole_start = find(&bytes, b"/Contents <").expect("the placeholder") + "/Contents ".len();
    let hole_length = RESERVED * 2 + 2;
    let after_hole = hole_start + hole_length;
    let tail_length = bytes.len() - after_hole;

    let byte_range = format!("/ByteRange [0 {hole_start:010} {after_hole:010} {tail_length:010}]");
    let range_at = find(&bytes, b"/ByteRange [").expect("the byte range");
    bytes[range_at..range_at + byte_range.len()].copy_from_slice(byte_range.as_bytes());

    // Everything except the hole: exactly what the reader will digest.
    let mut signed = Vec::new();
    signed.extend_from_slice(&bytes[..hole_start]);
    signed.extend_from_slice(&bytes[after_hole..]);

    let cms = detached_signature(&signer, &cert, &signed);
    assert!(
        cms.len() <= RESERVED,
        "the signature needs {} bytes and {RESERVED} were reserved",
        cms.len()
    );

    let mut hex = String::from("<");
    for byte in &cms {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex.push_str(&"0".repeat((RESERVED - cms.len()) * 2));
    hex.push('>');
    bytes[hole_start..after_hole].copy_from_slice(hex.as_bytes());

    Signed {
        bytes,
        hole_start,
        cms_len: cms.len(),
        trailer: format!(
            "startxref
{xref_at}
%%EOF
"
        ),
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn write(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("ypdf-sign-tests");
    std::fs::create_dir_all(&dir).expect("scratch directory");
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("writes");
    path
}

#[test]
fn a_real_signature_over_a_real_document_verifies() {
    let path = write("intact.pdf", &signed_pdf().bytes);
    let report = verify_file(&path).expect("checks");

    assert_eq!(report.signatures.len(), 1, "{}", report.to_human());
    let signature = &report.signatures[0];
    assert_eq!(signature.verdict, Verdict::Intact, "{}", report.to_human());
    assert_eq!(signature.coverage, Coverage::WholeFile);
    assert!(report.all_intact());

    // The certificate is read out and reported, and it is the one that signed.
    let certificate = signature.certificate.as_ref().expect("a certificate");
    assert_eq!(certificate.common_name(), "Ada Lovelace");
    assert!(
        certificate.self_signed,
        "this test certificate is its own root"
    );
    assert!(certificate.key_algorithm.contains("ECDSA"));

    // And the report still refuses to call it trusted.
    let text = report.to_human();
    assert!(text.contains("What this does not establish:"));
    assert!(text.contains("trust store"));

    let _ = std::fs::remove_file(path);
}

#[test]
fn changing_one_byte_of_the_document_is_caught() {
    let mut bytes = signed_pdf().bytes;

    // The visible title of the page's signature field: a change a reader would
    // see, in a part of the file the signature covers.
    let at = find(&bytes, b"(Ada Lovelace)").expect("the name") + 1;
    bytes[at] = b'E';

    let path = write("altered.pdf", &bytes);
    let report = verify_file(&path).expect("checks");

    assert_eq!(
        report.signatures[0].verdict,
        Verdict::DocumentAltered,
        "a changed byte inside the signed range must not verify"
    );
    assert!(!report.all_intact());

    let _ = std::fs::remove_file(path);
}

#[test]
fn tampering_with_the_signature_itself_is_a_different_answer() {
    let signed = signed_pdf();
    let mut bytes = signed.bytes;

    // Flip a byte at the very end of the DER, which is inside the signature
    // value itself rather than the certificate. The document's digest still
    // matches; the signature over it no longer does. Reporting that as
    // "document altered" would send someone looking for a change that is not
    // there.
    let last_hex = signed.hole_start + 1 + signed.cms_len * 2 - 2;
    bytes[last_hex] = if bytes[last_hex] == b'a' { b'b' } else { b'a' };

    let path = write("tampered.pdf", &bytes);
    let report = verify_file(&path).expect("checks");

    assert!(
        matches!(
            report.signatures[0].verdict,
            Verdict::SignatureBroken | Verdict::Unreadable { .. }
        ),
        "expected a signature failure, got {:?}",
        report.signatures[0].verdict
    );
    assert_ne!(report.signatures[0].verdict, Verdict::DocumentAltered);

    let _ = std::fs::remove_file(path);
}

#[test]
fn appending_a_page_after_signing_leaves_the_signature_intact_and_says_so() {
    // This is the case that matters in practice and that "valid ✓" hides: the
    // signature is still perfectly good, and it no longer covers the file.
    let signed = signed_pdf();
    let mut bytes = signed.bytes;
    bytes.extend_from_slice(b"\n% and then somebody added all of this\n");
    bytes.extend_from_slice(&vec![b'x'; 2_000]);
    // A real appended revision ends with its own trailer, and without one no
    // reader would open the file at all — including the one under test.
    bytes.extend_from_slice(b"\n");
    bytes.extend_from_slice(signed.trailer.as_bytes());

    let path = write("appended.pdf", &bytes);
    let report = verify_file(&path).expect("checks");

    let signature = &report.signatures[0];
    assert_eq!(signature.verdict, Verdict::Intact);
    assert!(matches!(signature.coverage, Coverage::PartOfFile { .. }));
    assert!(
        signature
            .notes
            .iter()
            .any(|note| note.code == "SIG_INTACT_BUT_PARTIAL"),
        "an intact signature over part of a file has to say which part: {:?}",
        signature.notes
    );
    assert!(signature.coverage.unsigned_bytes() >= 2_000);

    let _ = std::fs::remove_file(path);
}

/// Write `tests/fixtures/signed.pdf`, for the tests that need a signed file
/// they did not build themselves.
///
/// Ignored by default: it writes into the repository. Run it with
/// `cargo test -p ypdf-sign --test signed -- --ignored` when the fixture needs
/// regenerating. The signature is made with a throwaway key generated on the
/// spot, and the certificate is its own root — it is a test file, and nothing
/// in it should ever be trusted.
#[test]
#[ignore = "writes a file into the repository"]
fn write_the_shared_fixture() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/signed.pdf");
    std::fs::write(&path, signed_pdf().bytes).expect("writes the fixture");

    let report = verify_file(&path).expect("checks");
    assert_eq!(report.signatures[0].verdict, Verdict::Intact);
}
