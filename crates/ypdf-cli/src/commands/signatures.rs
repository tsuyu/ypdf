//! `signatures` — read and check digital signatures (spec §9).

use std::path::Path;

use serde_json::{Value, json};
use ypdf_core::{CancelToken, Result};
use ypdf_sign::{Coverage, TRUST_LIMITS, Verdict};

use crate::batch::FileReport;

/// Check one file.
///
/// Reads only, and reads the file on disk rather than a re-serialized copy: a
/// signature covers byte offsets, so the bytes have to be the ones it was made
/// over.
pub fn run(
    path: &Path,
    require_signed: bool,
    _password: Option<&str>,
    _cancel: &CancelToken,
) -> Result<FileReport> {
    let report = ypdf_sign::verify_file(path)?;

    let signatures: Vec<Value> = report
        .signatures
        .iter()
        .map(|signature| {
            json!({
                "field": signature.field,
                "page": signature.page,
                "signer": signature.signer(),
                "declared_name": signature.declared_name,
                "reason": signature.reason,
                "location": signature.location,
                "claimed_time": signature.claimed_time,
                "sub_filter": signature.sub_filter,
                "certification": signature.certification,
                "timestamped": signature.timestamped,
                "verdict": verdict_name(&signature.verdict),
                "detail": verdict_detail(&signature.verdict),
                "covers_whole_file": signature.coverage == Coverage::WholeFile,
                "unsigned_bytes": signature.coverage.unsigned_bytes(),
                "certificate": signature.certificate.as_ref().map(|cert| json!({
                    "subject": cert.subject,
                    "issuer": cert.issuer,
                    "serial": cert.serial,
                    "not_before": cert.not_before,
                    "not_after": cert.not_after,
                    "key_algorithm": cert.key_algorithm,
                    "self_signed": cert.self_signed,
                })),
                "chain_length": signature.chain.len(),
                "notes": signature.notes.iter().map(|note| json!({
                    "code": note.code,
                    "message": note.message,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();

    // A signature that failed is the finding. An unsigned document only counts
    // as one when the caller said signatures were expected — plenty of files
    // are legitimately unsigned, and exiting non-zero for all of them would
    // make the flag useless.
    let broken = report
        .signatures
        .iter()
        .any(|signature| !signature.verdict.is_intact());
    let flagged = broken || (require_signed && !report.is_signed());

    let mut human = report.to_human();
    if require_signed && !report.is_signed() {
        human.push_str("\nSignatures were required, and this file has none.\n");
    }

    Ok(FileReport {
        human,
        json: json!({
            "signed": report.is_signed(),
            "all_intact": report.all_intact(),
            "signatures": signatures,
            "empty_fields": report.empty_fields,
            "not_established": TRUST_LIMITS,
            "flagged": flagged,
        }),
        flagged,
        ..FileReport::default()
    })
}

const fn verdict_name(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Intact => "intact",
        Verdict::DocumentAltered => "document_altered",
        Verdict::SignatureBroken => "signature_broken",
        Verdict::Unsupported { .. } => "not_checked",
        Verdict::Unreadable { .. } => "unreadable",
    }
}

fn verdict_detail(verdict: &Verdict) -> Option<&str> {
    match verdict {
        Verdict::Unsupported { detail } | Verdict::Unreadable { detail } => Some(detail),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    #[test]
    fn an_unsigned_file_is_reported_and_does_not_fail_the_run() {
        let report =
            run(&fixture("two-pages.pdf"), false, None, &CancelToken::new()).expect("reads");

        assert_eq!(report.json["signed"], false);
        assert!(
            !report.flagged,
            "not being signed is not a failure by itself"
        );
        assert!(report.human.contains("not signed"));
    }

    #[test]
    fn requiring_a_signature_makes_an_unsigned_file_fail() {
        let report =
            run(&fixture("two-pages.pdf"), true, None, &CancelToken::new()).expect("reads");
        assert!(report.flagged);
        assert!(report.human.contains("Signatures were required"));
    }

    #[test]
    fn the_report_always_carries_what_it_did_not_establish() {
        let report =
            run(&fixture("two-pages.pdf"), false, None, &CancelToken::new()).expect("reads");
        assert!(
            !report.json["not_established"]
                .as_array()
                .expect("limits")
                .is_empty()
        );
    }
}
