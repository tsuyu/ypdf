//! The CMS/PKCS#7 half: what was signed, by which key, and does it check out.
//!
//! A PDF signature is a detached CMS `SignedData` blob sitting in `/Contents`.
//! Verifying it is two steps that are easy to conflate and must not be:
//!
//! 1. the digest in the signed attributes equals the digest of the bytes the
//!    `/ByteRange` covers — the document is as it was signed; and
//! 2. the signature over those attributes verifies against the certificate's
//!    public key — the signature is the key holder's.
//!
//! Failing the first and failing the second mean completely different things
//! to whoever is reading the report, so they are reported separately.

use cms::content_info::ContentInfo;
use cms::signed_data::{SignedData, SignerIdentifier, SignerInfo};
use const_oid::ObjectIdentifier;
use der::{Decode, Encode};
use sha1::Sha1;
use sha2::{Digest as _, Sha256, Sha384, Sha512};
use signature::Verifier;
use x509_cert::Certificate;

use crate::model::Note;

const ID_SIGNED_DATA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.2");
const ID_MESSAGE_DIGEST: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.4");
const ID_TIMESTAMP_TOKEN: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.2.14");

const SHA1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.14.3.2.26");
const SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1");
const SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.2");
const SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.3");

const RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");
const RSA_SHA1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.5");
const RSA_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.11");
const RSA_SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.12");
const RSA_SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.13");
const RSA_PSS: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.10");
const ECDSA_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");
const ECDSA_SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.3");

/// Which hash a signature uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hash {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    fn from_oid(oid: ObjectIdentifier) -> Option<Self> {
        match oid {
            SHA1 => Some(Self::Sha1),
            SHA256 => Some(Self::Sha256),
            SHA384 => Some(Self::Sha384),
            SHA512 => Some(Self::Sha512),
            _ => None,
        }
    }

    fn digest(self, bytes: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha1 => Sha1::digest(bytes).to_vec(),
            Self::Sha256 => Sha256::digest(bytes).to_vec(),
            Self::Sha384 => Sha384::digest(bytes).to_vec(),
            Self::Sha512 => Sha512::digest(bytes).to_vec(),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "SHA-1",
            Self::Sha256 => "SHA-256",
            Self::Sha384 => "SHA-384",
            Self::Sha512 => "SHA-512",
        }
    }
}

/// What verification concluded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Digest matches the bytes, and the signature over it verifies.
    Intact,
    /// The bytes are not the bytes that were signed.
    DigestMismatch,
    /// The bytes are as signed, but the signature is not the key holder's.
    SignatureInvalid,
    /// Real, and not implemented here.
    Unsupported(String),
    /// Could not be read as CMS at all.
    Unreadable(String),
}

/// Everything read out of one `/Contents` blob.
#[derive(Clone, Debug)]
pub struct Parsed {
    /// What the checks concluded.
    pub outcome: Outcome,
    /// The signing certificate, when one was found.
    pub certificate: Option<Certificate>,
    /// Every other certificate the signature carried.
    pub chain: Vec<Certificate>,
    /// Does the signature carry a timestamp token?
    pub timestamped: bool,
    /// Anything else worth saying.
    pub notes: Vec<Note>,
}

impl Parsed {
    fn unreadable(detail: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Unreadable(detail.into()),
            certificate: None,
            chain: Vec::new(),
            timestamped: false,
            notes: Vec::new(),
        }
    }
}

/// Verify a detached CMS signature over the bytes the byte range covered.
#[must_use]
pub fn verify(blob: &[u8], signed_bytes: &[u8]) -> Parsed {
    // The blob is zero-padded to fill the hole reserved for it, so the DER
    // ends well before the buffer does.
    let der = trim_padding(blob);

    let content = match ContentInfo::from_der(der) {
        Ok(content) => content,
        Err(e) => return Parsed::unreadable(format!("not a CMS message ({e})")),
    };
    if content.content_type != ID_SIGNED_DATA {
        return Parsed::unreadable(format!(
            "the signature is a CMS message of type {}, not SignedData",
            content.content_type
        ));
    }
    let signed_data: SignedData = match content.content.decode_as() {
        Ok(data) => data,
        Err(e) => return Parsed::unreadable(format!("the SignedData could not be read ({e})")),
    };

    let Some(signer) = signed_data.signer_infos.0.iter().next() else {
        return Parsed::unreadable("the signature carries no signer");
    };

    let certificates: Vec<Certificate> = signed_data
        .certificates
        .as_ref()
        .map(|set| {
            set.0
                .iter()
                .filter_map(|choice| match choice {
                    cms::cert::CertificateChoices::Certificate(cert) => Some(cert.clone()),
                    cms::cert::CertificateChoices::Other(_) => None,
                })
                .collect()
        })
        .unwrap_or_default();

    let signing_cert = find_signer(signer, &certificates);
    let chain: Vec<Certificate> = certificates
        .iter()
        .filter(|cert| Some(*cert) != signing_cert.as_ref())
        .cloned()
        .collect();

    let timestamped = signer
        .unsigned_attrs
        .as_ref()
        .is_some_and(|attrs| attrs.iter().any(|attr| attr.oid == ID_TIMESTAMP_TOKEN));

    let mut notes = Vec::new();
    if timestamped {
        // Reported, not verified: checking a timestamp means validating a
        // second signature from a timestamp authority, and trusting it means
        // the trust store this crate does not have.
        notes.push(Note::new(
            "SIG_TIMESTAMPED",
            "the signature carries a timestamp token; it is reported, not verified",
        ));
    }

    let outcome = check(signer, signed_bytes, signing_cert.as_ref(), &mut notes);

    Parsed {
        outcome,
        certificate: signing_cert,
        chain,
        timestamped,
        notes,
    }
}

fn check(
    signer: &SignerInfo,
    signed_bytes: &[u8],
    certificate: Option<&Certificate>,
    notes: &mut Vec<Note>,
) -> Outcome {
    let Some(hash) = Hash::from_oid(signer.digest_alg.oid) else {
        return Outcome::Unsupported(format!("digest algorithm {}", signer.digest_alg.oid));
    };
    if hash == Hash::Sha1 {
        notes.push(Note::new(
            "SIG_WEAK_DIGEST",
            "the signature uses SHA-1, which has been broken since 2017: a matching digest \
             no longer proves the document was not substituted",
        ));
    }

    let computed = hash.digest(signed_bytes);

    // With signed attributes — which every PDF signature in practice has — the
    // signature is over the attributes, and the document's digest is one of
    // them. Without them, it is over the document bytes directly.
    let to_verify = match &signer.signed_attrs {
        Some(attrs) => {
            let Some(declared) = message_digest(signer) else {
                return Outcome::Unreadable(
                    "the signed attributes carry no message digest".to_string(),
                );
            };
            if declared != computed {
                return Outcome::DigestMismatch;
            }
            match attrs.to_der() {
                Ok(der) => der,
                Err(e) => {
                    return Outcome::Unreadable(format!(
                        "the signed attributes could not be re-encoded ({e})"
                    ));
                }
            }
        }
        None => {
            notes.push(Note::new(
                "SIG_NO_SIGNED_ATTRIBUTES",
                "the signature is over the document bytes directly, with no signed \
                 attributes",
            ));
            signed_bytes.to_vec()
        }
    };

    let Some(certificate) = certificate else {
        return Outcome::Unsupported(
            "the signature does not carry the certificate it was made with, so there is no \
             public key here to check it against"
                .to_string(),
        );
    };

    verify_signature(signer, certificate, &to_verify, hash, notes)
}

fn message_digest(signer: &SignerInfo) -> Option<Vec<u8>> {
    let attrs = signer.signed_attrs.as_ref()?;
    let attr = attrs.iter().find(|attr| attr.oid == ID_MESSAGE_DIGEST)?;
    let value = attr.values.iter().next()?;
    let octets = der::asn1::OctetString::from_der(&value.to_der().ok()?).ok()?;
    Some(octets.as_bytes().to_vec())
}

fn verify_signature(
    signer: &SignerInfo,
    certificate: &Certificate,
    message: &[u8],
    hash: Hash,
    notes: &mut Vec<Note>,
) -> Outcome {
    let algorithm = signer.signature_algorithm.oid;
    let signature = signer.signature.as_bytes();
    let Ok(spki) = certificate.tbs_certificate.subject_public_key_info.to_der() else {
        return Outcome::Unreadable("the certificate's public key could not be read".to_string());
    };

    match algorithm {
        RSA | RSA_SHA1 | RSA_SHA256 | RSA_SHA384 | RSA_SHA512 => {
            rsa_pkcs1(&spki, message, signature, hash)
        }
        RSA_PSS => {
            // The PSS parameters are an AlgorithmIdentifier of their own. The
            // overwhelmingly common case is MGF1 with the same hash and a salt
            // the size of that hash, which is what this assumes — and says.
            notes.push(Note::new(
                "SIG_PSS_PARAMETERS_ASSUMED",
                "RSASSA-PSS was checked assuming MGF1 with the signature's own digest and a \
                 salt of the same length",
            ));
            rsa_pss(&spki, message, signature, hash)
        }
        ECDSA_SHA256 => ecdsa_p256(&spki, message, signature),
        ECDSA_SHA384 => ecdsa_p384(&spki, message, signature),
        other => Outcome::Unsupported(format!(
            "signature algorithm {other} (the digest was {})",
            hash.name()
        )),
    }
}

fn rsa_pkcs1(spki: &[u8], message: &[u8], signature: &[u8], hash: Hash) -> Outcome {
    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::pkcs8::DecodePublicKey;

    let Ok(key) = rsa::RsaPublicKey::from_public_key_der(spki) else {
        return Outcome::Unsupported("the certificate does not carry an RSA key".to_string());
    };
    let Ok(signature) = Signature::try_from(signature) else {
        return Outcome::SignatureInvalid;
    };

    let verified = match hash {
        Hash::Sha1 => VerifyingKey::<Sha1>::new(key).verify(message, &signature),
        Hash::Sha256 => VerifyingKey::<Sha256>::new(key).verify(message, &signature),
        Hash::Sha384 => VerifyingKey::<Sha384>::new(key).verify(message, &signature),
        Hash::Sha512 => VerifyingKey::<Sha512>::new(key).verify(message, &signature),
    };
    if verified.is_ok() {
        Outcome::Intact
    } else {
        Outcome::SignatureInvalid
    }
}

fn rsa_pss(spki: &[u8], message: &[u8], signature: &[u8], hash: Hash) -> Outcome {
    use rsa::pkcs8::DecodePublicKey;
    use rsa::pss::{Signature, VerifyingKey};

    let Ok(key) = rsa::RsaPublicKey::from_public_key_der(spki) else {
        return Outcome::Unsupported("the certificate does not carry an RSA key".to_string());
    };
    let Ok(signature) = Signature::try_from(signature) else {
        return Outcome::SignatureInvalid;
    };

    let verified = match hash {
        Hash::Sha1 => VerifyingKey::<Sha1>::new(key).verify(message, &signature),
        Hash::Sha256 => VerifyingKey::<Sha256>::new(key).verify(message, &signature),
        Hash::Sha384 => VerifyingKey::<Sha384>::new(key).verify(message, &signature),
        Hash::Sha512 => VerifyingKey::<Sha512>::new(key).verify(message, &signature),
    };
    if verified.is_ok() {
        Outcome::Intact
    } else {
        Outcome::SignatureInvalid
    }
}

fn ecdsa_p256(spki: &[u8], message: &[u8], signature: &[u8]) -> Outcome {
    use p256::ecdsa::{Signature, VerifyingKey};
    use p256::pkcs8::DecodePublicKey;

    let Ok(key) = VerifyingKey::from_public_key_der(spki) else {
        return Outcome::Unsupported(
            "the certificate does not carry a P-256 key, and the signature says ECDSA with \
             SHA-256"
                .to_string(),
        );
    };
    let Ok(signature) = Signature::from_der(signature) else {
        return Outcome::SignatureInvalid;
    };
    if key.verify(message, &signature).is_ok() {
        Outcome::Intact
    } else {
        Outcome::SignatureInvalid
    }
}

fn ecdsa_p384(spki: &[u8], message: &[u8], signature: &[u8]) -> Outcome {
    use p384::ecdsa::{Signature, VerifyingKey};
    use p384::pkcs8::DecodePublicKey;

    let Ok(key) = VerifyingKey::from_public_key_der(spki) else {
        return Outcome::Unsupported(
            "the certificate does not carry a P-384 key, and the signature says ECDSA with \
             SHA-384"
                .to_string(),
        );
    };
    let Ok(signature) = Signature::from_der(signature) else {
        return Outcome::SignatureInvalid;
    };
    if key.verify(message, &signature).is_ok() {
        Outcome::Intact
    } else {
        Outcome::SignatureInvalid
    }
}

/// Which of the carried certificates signed this.
fn find_signer(signer: &SignerInfo, certificates: &[Certificate]) -> Option<Certificate> {
    match &signer.sid {
        SignerIdentifier::IssuerAndSerialNumber(id) => certificates
            .iter()
            .find(|cert| {
                cert.tbs_certificate.serial_number == id.serial_number
                    && cert.tbs_certificate.issuer == id.issuer
            })
            .cloned(),
        SignerIdentifier::SubjectKeyIdentifier(key_id) => certificates
            .iter()
            .find(|cert| subject_key_identifier(cert).as_deref() == Some(key_id.0.as_bytes()))
            .cloned()
            // A signature that identifies its key by an identifier no carried
            // certificate has is not one this can match up; with exactly one
            // certificate present, that one is the only candidate.
            .or_else(|| certificates.first().cloned()),
    }
}

fn subject_key_identifier(certificate: &Certificate) -> Option<Vec<u8>> {
    const SUBJECT_KEY_ID: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.14");
    let extensions = certificate.tbs_certificate.extensions.as_ref()?;
    let extension = extensions
        .iter()
        .find(|ext| ext.extn_id == SUBJECT_KEY_ID)?;
    let octets = der::asn1::OctetString::from_der(extension.extn_value.as_bytes()).ok()?;
    Some(octets.as_bytes().to_vec())
}

/// Trim the zero padding a producer leaves after the DER in `/Contents`.
fn trim_padding(blob: &[u8]) -> &[u8] {
    let end = blob
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |index| index + 1);
    &blob[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_after_the_der_is_trimmed() {
        let mut blob = b"\x30\x03\x02\x01\x2a".to_vec();
        blob.extend_from_slice(&[0u8; 200]);
        assert_eq!(trim_padding(&blob).len(), 5);
    }

    #[test]
    fn something_that_is_not_cms_is_unreadable_rather_than_invalid() {
        // "I cannot read this" and "this signature is forged" must never be
        // the same answer.
        let parsed = verify(b"not a signature at all", b"document");
        assert!(matches!(parsed.outcome, Outcome::Unreadable(_)));
        assert!(parsed.certificate.is_none());
    }

    #[test]
    fn an_empty_blob_is_unreadable() {
        let parsed = verify(&[0u8; 512], b"document");
        assert!(matches!(parsed.outcome, Outcome::Unreadable(_)));
    }

    #[test]
    fn hashes_map_to_their_oids() {
        assert_eq!(Hash::from_oid(SHA256), Some(Hash::Sha256));
        assert_eq!(Hash::from_oid(SHA1), Some(Hash::Sha1));
        assert_eq!(Hash::from_oid(ID_SIGNED_DATA), None);
        assert_eq!(Hash::Sha256.digest(b"").len(), 32);
        assert_eq!(Hash::Sha512.digest(b"").len(), 64);
    }
}
