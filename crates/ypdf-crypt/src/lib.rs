//! PDF encryption and permissions (spec §8).
//!
//! Two things this crate does, and one it deliberately does not.
//!
//! It **encrypts** a document with AES-128 or AES-256 and a pair of passwords,
//! and it **removes** that encryption from a document that has already been
//! opened with the right password. It reports what protection a file carries.
//!
//! It does not, and will not, remove protection from a document nobody can
//! open. Recovering a document whose password is lost is password cracking
//! whatever it is called in the menu, and a tool that offers it is a tool for
//! opening other people's files.
//!
//! ```no_run
//! use ypdf_crypt::{Algorithm, EncryptSettings, Permissions, encrypt};
//! use ypdf_doc::Pdf;
//!
//! # fn main() -> ypdf_core::Result<()> {
//! let mut pdf = Pdf::open("report.pdf")?;
//! encrypt(
//!     &mut pdf,
//!     &EncryptSettings {
//!         algorithm: Algorithm::Aes256,
//!         user_password: "open-me".into(),
//!         owner_password: "change-me".into(),
//!         permissions: Permissions::read_only(),
//!     },
//! )?;
//! pdf.save("report-protected.pdf")?;
//! # Ok(())
//! # }
//! ```

mod permissions;

use std::collections::BTreeMap;
use std::sync::Arc;

use lopdf::encryption::crypt_filters::{Aes128CryptFilter, Aes256CryptFilter, CryptFilter};
use lopdf::{EncryptionState, EncryptionVersion, Object};
use rand::Rng as _;
use ypdf_core::{Error, Result};
use ypdf_doc::Pdf;

pub use permissions::Permissions;

/// Which cipher to protect the document with.
///
/// Only AES is offered. RC4 is still readable — plenty of documents in the
/// world use it — but producing a new one would be handing someone protection
/// that has been broken since 2001 while calling it encryption.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Algorithm {
    /// AES-128, PDF 1.6 and later. Readable by essentially everything.
    Aes128,
    /// AES-256, PDF 2.0. Stronger, and refused by readers older than about 2017.
    #[default]
    Aes256,
}

impl Algorithm {
    /// The name to show.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Aes128 => "AES-128",
            Self::Aes256 => "AES-256",
        }
    }

    /// Key length in bits.
    #[must_use]
    pub const fn key_bits(self) -> u32 {
        match self {
            Self::Aes128 => 128,
            Self::Aes256 => 256,
        }
    }
}

/// How to protect a document.
#[derive(Clone, Debug, Default)]
pub struct EncryptSettings {
    /// Which cipher.
    pub algorithm: Algorithm,
    /// Needed to open the document at all. Empty means anyone can open it and
    /// only the permission flags apply.
    pub user_password: String,
    /// Needed to change the protection or override the permissions. Empty means
    /// the user password is used for both.
    pub owner_password: String,
    /// What a reader is allowed to do.
    pub permissions: Permissions,
}

/// Encrypt a document and return the protected file.
///
/// Encryption is a property of the **written file**, not of the document in
/// memory: the objects have to be plaintext to be worked on and ciphertext to
/// be stored. So this returns bytes rather than mutating `pdf`, and the caller
/// writes them. There is no state in which a `Pdf` is "the encrypted one".
///
/// # Errors
///
/// Refuses a document that was itself opened from an encrypted file: producing
/// a second layer would quietly replace protection the caller may not have
/// meant to change. Decrypt first, deliberately, then encrypt.
pub fn encrypt(pdf: &mut Pdf, settings: &EncryptSettings) -> Result<Vec<u8>> {
    if settings.user_password.is_empty() && settings.owner_password.is_empty() {
        return Err(Error::Config {
            detail: "encryption needs at least one password".into(),
            source_path: None,
        });
    }
    if pdf.was_encrypted() {
        return Err(Error::Unsupported {
            feature: "re-encrypting a document that is already protected".into(),
        });
    }

    // An owner password that is not set falls back to the user password. The
    // alternative - an empty owner password - is one any reader will accept to
    // lift every restriction, which makes the restrictions decorative.
    let owner = if settings.owner_password.is_empty() {
        settings.user_password.clone()
    } else {
        settings.owner_password.clone()
    };
    let user = settings.user_password.clone();

    // Flatten the page tree and settle every pending edit first: commit()
    // rewrites objects, and rewriting them after encryption would write
    // plaintext into an encrypted file.
    let bytes = pdf.to_bytes()?;
    let mut document = lopdf::Document::load_mem(&bytes).map_err(|e| Error::Backend {
        backend: "lopdf",
        detail: e.to_string(),
    })?;

    ensure_file_id(&mut document);

    let permissions = settings.permissions.to_bits();
    let mut key = [0_u8; 32];

    let state = match settings.algorithm {
        Algorithm::Aes128 => {
            let filter: Arc<dyn CryptFilter> = Arc::new(Aes128CryptFilter);
            EncryptionState::try_from(EncryptionVersion::V4 {
                document: &document,
                encrypt_metadata: true,
                crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]),
                stream_filter: b"StdCF".to_vec(),
                string_filter: b"StdCF".to_vec(),
                owner_password: &owner,
                user_password: &user,
                permissions,
            })
        }
        Algorithm::Aes256 => {
            rand::rng().fill(&mut key);
            let filter: Arc<dyn CryptFilter> = Arc::new(Aes256CryptFilter);
            EncryptionState::try_from(EncryptionVersion::V5 {
                encrypt_metadata: true,
                crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]),
                file_encryption_key: &key,
                stream_filter: b"StdCF".to_vec(),
                string_filter: b"StdCF".to_vec(),
                owner_password: &owner,
                user_password: &user,
                permissions,
            })
        }
    }
    .map_err(|e| Error::Backend {
        backend: "lopdf",
        detail: e.to_string(),
    })?;

    document.encrypt(&state).map_err(|e| Error::Backend {
        backend: "lopdf",
        detail: e.to_string(),
    })?;

    // AES-256 is a PDF 2.0 feature; a 1.7 header over an AESV3 filter is a file
    // a strict reader is entitled to reject.
    if settings.algorithm == Algorithm::Aes256 {
        document.version = "2.0".to_string();
    }

    let mut encrypted = Vec::new();
    document.save_to(&mut encrypted).map_err(|e| Error::Io {
        path: None,
        source: e,
    })?;

    tracing::info!(
        algorithm = settings.algorithm.label(),
        restrictions = settings.permissions.restrictions().len(),
        bytes = encrypted.len(),
        "document encrypted"
    );
    Ok(encrypted)
}

/// Give the document a file identifier if it has none.
///
/// `/ID` feeds the key derivation for everything before AES-256, so a document
/// without one cannot be encrypted at all. Most producers write one; hand-built
/// files often do not.
fn ensure_file_id(document: &mut lopdf::Document) {
    if document.trailer.get(b"ID").is_ok() {
        return;
    }

    let mut id = [0_u8; 16];
    rand::rng().fill(&mut id);
    let value = Object::Array(vec![
        Object::String(id.to_vec(), lopdf::StringFormat::Hexadecimal),
        Object::String(id.to_vec(), lopdf::StringFormat::Hexadecimal),
    ]);
    document.trailer.set("ID", value);
}

/// Open an encrypted document.
///
/// Either password works: the user password gives the contents, the owner
/// password gives the contents and the standing to change the protection.
///
/// There is no way here to open a document whose password nobody has.
/// Recovering one is password cracking whatever the menu calls it, and this
/// crate will not do it.
pub fn open_protected(path: impl AsRef<std::path::Path>, password: &str) -> Result<Pdf> {
    Pdf::open_with_password(path, password)
}

/// Whether saving this document would drop protection it arrived with.
///
/// True for any document opened from an encrypted file: the objects were
/// decrypted to be worked on, and a plain save writes them as they are. The
/// caller is expected to say so before writing rather than letting someone
/// discover it afterwards.
#[must_use]
pub fn would_lose_protection(pdf: &Pdf) -> bool {
    pdf.was_encrypted()
}

/// What protection a document carries (spec §8, "Security Information").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecurityInfo {
    /// Is there any encryption at all?
    pub encrypted: bool,
    /// The cipher, as the file describes it: `AES-256`, `RC4 128-bit`, and so on.
    pub algorithm: Option<String>,
    /// Key length in bits, when the file states one.
    pub key_bits: Option<u32>,
    /// What readers are permitted to do.
    pub permissions: Permissions,
    /// Is the document's metadata encrypted along with its contents?
    pub encrypt_metadata: bool,
    /// Does opening it need a password?
    pub user_password_required: bool,
}

impl SecurityInfo {
    /// The block from spec §8.
    #[must_use]
    pub fn to_human(&self) -> String {
        if !self.encrypted {
            return "Encryption:  none\n".to_string();
        }

        let mut out = format!(
            "Encryption:  {}\n",
            self.algorithm.as_deref().unwrap_or("unknown")
        );
        if let Some(bits) = self.key_bits {
            out.push_str(&format!("Key length:  {bits} bits\n"));
        }
        out.push_str(&format!(
            "Open password: {}\n",
            if self.user_password_required {
                "required"
            } else {
                "not required"
            }
        ));
        out.push_str(&format!(
            "Metadata:    {}\n",
            if self.encrypt_metadata {
                "encrypted"
            } else {
                "left readable"
            }
        ));

        let restrictions = self.permissions.restrictions();
        if restrictions.is_empty() {
            out.push_str("Permissions: everything allowed\n");
        } else {
            out.push_str(&format!("Restricted:  {}\n", restrictions.join(", ")));
            // Anyone reading this list is about to decide how much to trust it.
            out.push_str(
                "\nPermission flags are advisory: every conforming reader honours them,\n\
                 but nothing enforces them. The encryption is what protects the file.\n",
            );
        }
        out
    }
}

/// Read the protection a document arrived with.
///
/// Reports the file as it was on disk. A document that was never encrypted
/// reports nothing rather than reporting defaults: "AES-256, everything
/// permitted" and "no protection at all" are not the same claim.
#[must_use]
pub fn security_info(pdf: &Pdf) -> SecurityInfo {
    let Some(state) = pdf.encryption() else {
        return SecurityInfo::default();
    };

    // The reader keeps the decrypted state rather than the dictionary, so the
    // dictionary is rebuilt from it. Same values, and it is the only way to ask
    // the questions below without re-parsing the file.
    let dict = state.encode().ok();
    let method = dict.as_ref().and_then(|dict| {
        dict.get(b"CF")
            .and_then(Object::as_dict)
            .ok()
            .and_then(|filters| {
                filters
                    .iter()
                    .find_map(|(_, filter)| filter.as_dict().ok()?.get(b"CFM").ok()?.as_name().ok())
                    .map(|name| String::from_utf8_lossy(name).into_owned())
            })
    });

    let version = state.version();
    let revision = state.revision();
    let length = state.key_length().unwrap_or(5) * 8;

    let algorithm = match method.as_deref() {
        Some("AESV3") => "AES-256".to_string(),
        Some("AESV2") => "AES-128".to_string(),
        Some("V2") => format!("RC4 {length}-bit"),
        Some("None") => "none".to_string(),
        Some(other) => other.to_string(),
        None => match version {
            1 => "RC4 40-bit".to_string(),
            2 => format!("RC4 {length}-bit"),
            5 => "AES-256".to_string(),
            _ => format!("standard security handler, V{version} R{revision}"),
        },
    };

    let key_bits = match method.as_deref() {
        Some("AESV3") => Some(256),
        Some("AESV2") => Some(128),
        _ => u32::try_from(length).ok(),
    };

    SecurityInfo {
        encrypted: true,
        algorithm: Some(algorithm),
        key_bits,
        permissions: Permissions::from_bits(state.permissions()),
        encrypt_metadata: state.encrypt_metadata(),
        user_password_required: empty_password_is_refused(pdf, dict.as_ref()),
    }
}

/// Does this file actually need a password to open?
///
/// A document can be encrypted with an empty user password: anyone opens it,
/// and only the permission flags apply. Saying "password required" about such a
/// file would overstate its protection, which is the direction of error that
/// gets a document sent to the wrong person.
///
/// The question is answered by asking a throwaway document carrying the same
/// encryption dictionary, so nothing is re-parsed and no password is guessed at
/// beyond the empty one the format itself defines as the default.
fn empty_password_is_refused(pdf: &Pdf, dict: Option<&lopdf::Dictionary>) -> bool {
    let Some(dict) = dict else {
        return true;
    };

    let mut probe = lopdf::Document::new();
    // `/Encrypt` is looked up as an indirect reference, so it has to be a real
    // object rather than a dictionary sitting in the trailer.
    let encrypt_id = probe.add_object(Object::Dictionary(dict.clone()));
    probe.trailer.set("Encrypt", Object::Reference(encrypt_id));
    if let Ok(id) = pdf.raw().trailer.get(b"ID") {
        probe.trailer.set("ID", id.clone());
    }

    probe.authenticate_password("").is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Pdf {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/many-pages.pdf");
        Pdf::open(path).expect("the fixture opens")
    }

    fn settings(algorithm: Algorithm) -> EncryptSettings {
        EncryptSettings {
            algorithm,
            user_password: "open-me".into(),
            owner_password: "owner".into(),
            permissions: Permissions::all(),
        }
    }

    #[test]
    fn an_encrypted_document_cannot_be_opened_without_the_password() {
        let bytes = encrypt(&mut sample(), &settings(Algorithm::Aes256)).expect("encrypts");

        let opened = Pdf::from_bytes(&bytes);
        assert!(opened.is_err(), "no password should not be enough");
        assert_eq!(
            opened.err().map(|e| e.code()),
            Some("E_PASSWORD_REQUIRED"),
            "and the error should say which of the two problems it is"
        );

        assert!(
            Pdf::from_encrypted_bytes(&bytes, "not-it").is_err(),
            "the wrong password should not be enough either"
        );

        let right = Pdf::from_encrypted_bytes(&bytes, "open-me").expect("opens with the password");
        assert_eq!(right.page_count(), 12);
    }

    #[test]
    fn aes_128_works_the_same_way() {
        let bytes = encrypt(&mut sample(), &settings(Algorithm::Aes128)).expect("encrypts");

        assert!(Pdf::from_bytes(&bytes).is_err());
        let opened = Pdf::from_encrypted_bytes(&bytes, "open-me").expect("opens");
        assert_eq!(opened.page_count(), 12);
        assert_eq!(security_info(&opened).algorithm.as_deref(), Some("AES-128"));
        assert_eq!(security_info(&opened).key_bits, Some(128));
    }

    #[test]
    fn aes_256_is_reported_as_what_it_is() {
        let bytes = encrypt(&mut sample(), &settings(Algorithm::Aes256)).expect("encrypts");
        let opened = Pdf::from_encrypted_bytes(&bytes, "open-me").expect("opens");
        let info = security_info(&opened);

        assert_eq!(info.algorithm.as_deref(), Some("AES-256"));
        assert_eq!(info.key_bits, Some(256));
        assert!(info.user_password_required);
        assert!(info.encrypt_metadata);
    }

    #[test]
    fn the_owner_password_also_opens_the_document() {
        let bytes = encrypt(&mut sample(), &settings(Algorithm::Aes256)).expect("encrypts");
        assert!(Pdf::from_encrypted_bytes(&bytes, "owner").is_ok());
    }

    #[test]
    fn a_document_still_reads_correctly_through_the_encryption() {
        // Round-tripping the structure is not enough: the page contents have to
        // survive the cipher, and a stream that decrypts to rubbish still
        // parses as a document.
        let bytes = encrypt(&mut sample(), &settings(Algorithm::Aes256)).expect("encrypts");
        let opened = Pdf::from_encrypted_bytes(&bytes, "open-me").expect("opens");

        let text = opened
            .raw()
            .extract_text(&[1])
            .expect("the first page still has extractable text");
        assert!(text.contains('1'), "expected page text, got {text:?}");
    }

    #[test]
    fn restrictions_survive_the_round_trip() {
        let bytes = encrypt(
            &mut sample(),
            &EncryptSettings {
                permissions: Permissions {
                    copy: false,
                    modify: false,
                    ..Permissions::all()
                },
                ..settings(Algorithm::Aes256)
            },
        )
        .expect("encrypts");

        let opened = Pdf::from_encrypted_bytes(&bytes, "open-me").expect("opens");
        let info = security_info(&opened);

        assert!(!info.permissions.copy);
        assert!(!info.permissions.modify);
        assert!(info.permissions.print, "printing was never taken away");

        let human = info.to_human();
        assert!(human.contains("copying text"), "{human}");
        assert!(
            human.contains("advisory"),
            "the report must say the flags are not enforced:\n{human}"
        );
    }

    #[test]
    fn a_document_with_no_open_password_still_carries_its_flags() {
        let bytes = encrypt(
            &mut sample(),
            &EncryptSettings {
                user_password: String::new(),
                owner_password: "owner".into(),
                permissions: Permissions::read_only(),
                algorithm: Algorithm::Aes256,
            },
        )
        .expect("encrypts");

        // It opens with no password at all...
        let opened = Pdf::from_encrypted_bytes(&bytes, "").expect("opens");
        let info = security_info(&opened);
        assert!(!info.user_password_required);
        // ...and still says what it does not want done with it.
        assert!(!info.permissions.copy);
        assert!(info.permissions.print);
    }

    #[test]
    fn encryption_needs_at_least_one_password() {
        let result = encrypt(
            &mut sample(),
            &EncryptSettings {
                user_password: String::new(),
                owner_password: String::new(),
                ..settings(Algorithm::Aes256)
            },
        );
        assert!(
            result.is_err(),
            "encrypting with no password protects nothing"
        );
    }

    #[test]
    fn an_already_protected_document_is_refused_rather_than_layered() {
        let bytes = encrypt(&mut sample(), &settings(Algorithm::Aes256)).expect("encrypts");
        let mut opened = Pdf::from_encrypted_bytes(&bytes, "open-me").expect("opens");

        let error = encrypt(&mut opened, &settings(Algorithm::Aes128)).expect_err("refused");
        assert_eq!(error.code(), "E_UNSUPPORTED");
    }

    #[test]
    fn saving_a_protected_document_plainly_is_flagged_not_silent() {
        // Opening decrypts; a plain save writes plaintext. That is the right
        // behaviour and the wrong surprise, so it has to be announceable.
        let bytes = encrypt(&mut sample(), &settings(Algorithm::Aes256)).expect("encrypts");
        let mut opened = Pdf::from_encrypted_bytes(&bytes, "open-me").expect("opens");

        assert!(would_lose_protection(&opened));

        let plain = opened.to_bytes().expect("serializes");
        let reopened = Pdf::from_bytes(&plain).expect("opens with no password");
        assert_eq!(reopened.page_count(), 12);
        assert!(!security_info(&reopened).encrypted);
        assert!(!would_lose_protection(&reopened));
    }

    #[test]
    fn a_plain_document_reports_no_encryption() {
        let info = security_info(&sample());
        assert!(!info.encrypted);
        assert_eq!(info.to_human(), "Encryption:  none\n");
        assert!(!would_lose_protection(&sample()));
    }

    #[test]
    fn a_document_without_a_file_id_can_still_be_encrypted() {
        // /ID feeds the key derivation for AES-128. Hand-built files often have
        // none, and refusing them would be refusing half the test corpus.
        let mut pdf = sample();
        assert!(
            pdf.raw().trailer.get(b"ID").is_err(),
            "this fixture is expected to have no /ID"
        );

        let bytes = encrypt(&mut pdf, &settings(Algorithm::Aes128)).expect("encrypts anyway");
        assert!(Pdf::from_encrypted_bytes(&bytes, "open-me").is_ok());
    }
}
