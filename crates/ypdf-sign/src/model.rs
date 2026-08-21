//! What a signature is, and what checking one can honestly conclude.

/// What the cryptography says.
///
/// Deliberately narrow. None of these means "trusted": that is a question
/// about certificates and the people behind them, and this crate answers only
/// the question it can answer from the bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The digest matches the bytes and the signature verifies against the
    /// certificate in the file.
    Intact,
    /// The bytes covered by the signature are not the bytes that were signed:
    /// the document was altered after signing.
    DocumentAltered,
    /// The digest matches, but the signature over it does not verify. The
    /// document is as signed; the signature is not the signer's.
    SignatureBroken,
    /// Something in the signature is real but not implemented here.
    ///
    /// Reported rather than failed, because "this tool cannot check it" and
    /// "this signature is bad" are different sentences and only one of them
    /// should worry anyone.
    Unsupported {
        /// What could not be checked.
        detail: String,
    },
    /// The signature could not be read at all.
    Unreadable {
        /// What went wrong.
        detail: String,
    },
}

impl Verdict {
    /// A word for a badge or a column.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Intact => "intact",
            Self::DocumentAltered => "document altered",
            Self::SignatureBroken => "signature does not verify",
            Self::Unsupported { .. } => "not checked",
            Self::Unreadable { .. } => "unreadable",
        }
    }

    /// Did the check actually succeed?
    #[must_use]
    pub const fn is_intact(&self) -> bool {
        matches!(self, Self::Intact)
    }
}

/// How much of the file a signature covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    /// Every byte in the file, apart from the hole the signature sits in.
    WholeFile,
    /// An earlier revision. Bytes were appended after this signature was made,
    /// which is legal — that is how a second signature is added — and is also
    /// how a page gets added to a document somebody already signed.
    PartOfFile {
        /// Bytes the signature covers.
        signed: usize,
        /// Bytes in the file.
        total: usize,
    },
}

impl Coverage {
    /// Bytes in the file that this signature says nothing about.
    #[must_use]
    pub const fn unsigned_bytes(self) -> usize {
        match self {
            Self::WholeFile => 0,
            Self::PartOfFile { signed, total } => total.saturating_sub(signed),
        }
    }
}

/// What a certificate says about itself.
///
/// Read out of the file. Nothing here has been checked against a trust store,
/// because there is no trust store: see [`crate::TRUST_LIMITS`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CertificateInfo {
    /// Who the certificate is for, in RFC 4514 form.
    pub subject: String,
    /// Who issued it.
    pub issuer: String,
    /// Serial number, as hex.
    pub serial: String,
    /// Valid from, as `YYYY-MM-DD`.
    pub not_before: String,
    /// Valid until, as `YYYY-MM-DD`.
    pub not_after: String,
    /// The public key algorithm.
    pub key_algorithm: String,
    /// Does the certificate name itself as its own issuer?
    pub self_signed: bool,
}

impl CertificateInfo {
    /// The common name, when the subject has one; the whole subject otherwise.
    #[must_use]
    pub fn common_name(&self) -> String {
        self.subject
            .split(',')
            .map(str::trim)
            .find_map(|part| part.strip_prefix("CN="))
            .unwrap_or(&self.subject)
            .to_string()
    }
}

/// Something about a signature worth saying out loud.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    /// Stable code, for scripting against.
    pub code: &'static str,
    /// What it means, in a sentence.
    pub message: String,
}

impl Note {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// One signature in a document (spec §9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    /// The form field it lives in.
    pub field: String,
    /// The page its widget is on, counting from 1, when it has one.
    pub page: Option<u32>,
    /// The name the signature dictionary gives for the signer.
    ///
    /// Typed into the file by whoever made it. The certificate's subject is
    /// the one that took a private key to write.
    pub declared_name: Option<String>,
    /// Why, as stated.
    pub reason: Option<String>,
    /// Where, as stated.
    pub location: Option<String>,
    /// When, as stated — the signer's clock, not a timestamp authority's.
    pub claimed_time: Option<String>,
    /// The `/SubFilter`: which flavour of signature this is.
    pub sub_filter: String,
    /// Whether this signature certifies the document rather than approving it.
    pub certification: bool,
    /// Does the signature carry a timestamp token from a timestamp authority?
    ///
    /// Reported, never verified: checking one means validating a second
    /// signature and trusting whoever issued it.
    pub timestamped: bool,
    /// What it covers.
    pub coverage: Coverage,
    /// The result of checking it.
    pub verdict: Verdict,
    /// The signing certificate.
    pub certificate: Option<CertificateInfo>,
    /// The rest of the chain carried in the signature, issuer-ward.
    pub chain: Vec<CertificateInfo>,
    /// Everything else worth knowing.
    pub notes: Vec<Note>,
}

impl Signature {
    /// The signer as best the file can say: the certificate subject when there
    /// is one, and the declared name otherwise.
    #[must_use]
    pub fn signer(&self) -> String {
        self.certificate.as_ref().map_or_else(
            || {
                self.declared_name
                    .clone()
                    .unwrap_or_else(|| "(not stated)".to_string())
            },
            CertificateInfo::common_name,
        )
    }
}

/// What checking a signature here does *not* establish.
///
/// Printed with every report. A signature that verifies proves the bytes have
/// not changed since someone signed them with the key in the file. Who that
/// someone is, and whether anyone should believe them, is a different question
/// and needs things this tool does not have.
pub const TRUST_LIMITS: &[&str] = &[
    "the certificate is not checked against any trust store: nothing here says the signer \
     is who the certificate claims",
    "revocation is not checked — CRL and OCSP both need network access, and this tool has \
     none",
    "the chain is reported as the file carries it, not built or validated to a root",
    "signing times are the signer's own clock unless a timestamp is present; embedded \
     timestamp tokens are reported, not verified",
    "certificate policies, key usage, and name constraints are not enforced",
];

/// Every signature in a document, with the verdicts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The signatures, in the order they were applied.
    pub signatures: Vec<Signature>,
    /// Signature fields that exist but have never been signed.
    pub empty_fields: Vec<String>,
}

impl Report {
    /// Is the document signed at all?
    #[must_use]
    pub fn is_signed(&self) -> bool {
        !self.signatures.is_empty()
    }

    /// Did every signature check out?
    #[must_use]
    pub fn all_intact(&self) -> bool {
        self.is_signed() && self.signatures.iter().all(|s| s.verdict.is_intact())
    }

    /// The report as text (spec §9).
    #[must_use]
    pub fn to_human(&self) -> String {
        let mut out = String::from("DIGITAL SIGNATURES\n\n");

        if self.signatures.is_empty() {
            out.push_str("The document is not signed.\n");
            if !self.empty_fields.is_empty() {
                out.push_str(&format!(
                    "{} empty signature field(s): {}\n",
                    self.empty_fields.len(),
                    self.empty_fields.join(", ")
                ));
            }
            return out;
        }

        for (index, signature) in self.signatures.iter().enumerate() {
            out.push_str(&format!(
                "{}. {} — {}\n",
                index + 1,
                signature.signer(),
                signature.verdict.label()
            ));
            out.push_str(&format!("   Field:     {}\n", signature.field));
            if signature.certification {
                out.push_str("   Type:      certification (it says what may be changed)\n");
            }
            if let Some(reason) = &signature.reason {
                out.push_str(&format!("   Reason:    {reason}\n"));
            }
            if let Some(time) = &signature.claimed_time {
                out.push_str(&format!("   Claimed:   {time}\n"));
            }
            if signature.timestamped {
                out.push_str("   Timestamp: present, and not verified here\n");
            }
            out.push_str(&format!("   Format:    {}\n", signature.sub_filter));

            match signature.coverage {
                Coverage::WholeFile => {
                    out.push_str("   Covers:    the whole file\n");
                }
                Coverage::PartOfFile { signed, total } => {
                    out.push_str(&format!(
                        "   Covers:    {signed} of {total} bytes — {} were added after it\n",
                        total.saturating_sub(signed)
                    ));
                }
            }

            if let Some(cert) = &signature.certificate {
                out.push_str(&format!("   Subject:   {}\n", cert.subject));
                out.push_str(&format!("   Issuer:    {}\n", cert.issuer));
                out.push_str(&format!(
                    "   Valid:     {} to {}\n",
                    cert.not_before, cert.not_after
                ));
                out.push_str(&format!("   Key:       {}\n", cert.key_algorithm));
                if cert.self_signed {
                    out.push_str("   Note:      the certificate issued itself\n");
                }
            }

            for note in &signature.notes {
                out.push_str(&format!("   [{}] {}\n", note.code, note.message));
            }
            out.push('\n');
        }

        if !self.empty_fields.is_empty() {
            out.push_str(&format!(
                "Empty signature fields: {}\n\n",
                self.empty_fields.join(", ")
            ));
        }

        out.push_str("What this does not establish:\n");
        for limit in TRUST_LIMITS {
            out.push_str(&format!("  - {limit}\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(verdict: Verdict) -> Signature {
        Signature {
            field: "Signature1".into(),
            page: Some(1),
            declared_name: Some("A. Signer".into()),
            reason: None,
            location: None,
            claimed_time: None,
            sub_filter: "adbe.pkcs7.detached".into(),
            certification: false,
            timestamped: false,
            coverage: Coverage::WholeFile,
            verdict,
            certificate: None,
            chain: Vec::new(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn a_report_always_says_what_it_did_not_establish() {
        let report = Report {
            signatures: vec![signature(Verdict::Intact)],
            empty_fields: Vec::new(),
        };
        let text = report.to_human();
        assert!(text.contains("What this does not establish:"));
        assert!(text.contains("trust store"));
    }

    #[test]
    fn an_unsigned_document_says_so_rather_than_passing() {
        let report = Report::default();
        assert!(!report.is_signed());
        // `all_intact` on an unsigned file must be false: "no signature failed"
        // is not "the document is signed".
        assert!(!report.all_intact());
        assert!(report.to_human().contains("not signed"));
    }

    #[test]
    fn the_common_name_is_pulled_out_of_a_subject() {
        let cert = CertificateInfo {
            subject: "CN=Ada Lovelace, O=Analytical Engines, C=GB".into(),
            ..CertificateInfo::default()
        };
        assert_eq!(cert.common_name(), "Ada Lovelace");
    }

    #[test]
    fn coverage_counts_what_was_added_after_signing() {
        let coverage = Coverage::PartOfFile {
            signed: 900,
            total: 1200,
        };
        assert_eq!(coverage.unsigned_bytes(), 300);
        assert_eq!(Coverage::WholeFile.unsigned_bytes(), 0);
    }

    #[test]
    fn a_verdict_this_tool_cannot_reach_is_not_a_failure() {
        let unsupported = Verdict::Unsupported {
            detail: "DSA".into(),
        };
        assert!(!unsupported.is_intact());
        assert_eq!(unsupported.label(), "not checked");
        assert_ne!(unsupported.label(), Verdict::SignatureBroken.label());
    }
}
