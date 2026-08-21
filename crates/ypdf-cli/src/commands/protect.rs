//! Password protection and permissions (spec §8).
//!
//! Two commands that are opposites, and one rule they share: **neither one can
//! open a document whose password nobody has.** `decrypt` removes protection
//! the caller can already lift by supplying the password. There is no option
//! that recovers a lost one, because that is password cracking whatever the
//! flag is called.

use std::path::Path;

use serde_json::json;
use ypdf_core::{CancelToken, Error, Result};
use ypdf_crypt::{Algorithm, EncryptSettings, Permissions};

use crate::batch::FileReport;
use crate::cli::{DecryptArgs, EncryptArgs};
use crate::paths;

/// Turn the `--no-…` flags into a permission set.
///
/// Everything is allowed unless it was explicitly taken away: encrypting a
/// document should not quietly remove abilities nobody asked to remove.
#[must_use]
pub fn permissions_from(args: &EncryptArgs) -> Permissions {
    Permissions {
        print: !args.no_print,
        // Denying printing entirely denies the degraded form too; leaving the
        // high-quality bit set while printing is off is a contradiction.
        print_high_quality: !args.no_print && !args.no_high_quality_print,
        modify: !args.no_modify,
        copy: !args.no_copy,
        annotate: !args.no_annotate,
        fill_forms: !args.no_forms,
        assemble: !args.no_assemble,
    }
}

/// `encrypt` (spec §8).
pub fn encrypt(
    path: &Path,
    args: &EncryptArgs,
    password: Option<&str>,
    many: bool,
    overwrite: bool,
    _cancel: &CancelToken,
) -> Result<FileReport> {
    let target = paths::output_for(&args.output, path, many, "-protected")?;
    paths::guard(&target, overwrite)?;

    let mut pdf = super::open_input(path, password)?;
    let permissions = permissions_from(args);
    let settings = EncryptSettings {
        algorithm: args.algorithm.into(),
        user_password: args.user_password.clone().unwrap_or_default(),
        owner_password: args.owner_password.clone().unwrap_or_default(),
        permissions,
    };

    let bytes = ypdf_crypt::encrypt(&mut pdf, &settings)?;
    paths::write(&target, &bytes, overwrite)?;

    let restrictions = permissions.restrictions();
    let mut human = format!(
        "Protected with {} → {} ({})",
        settings.algorithm.label(),
        target.display(),
        ypdf_optimize::format_size(bytes.len() as u64)
    );
    if settings.user_password.is_empty() {
        human.push_str("\nNo open password: anyone can read it, and only the flags apply.");
    }
    if restrictions.is_empty() {
        human.push_str("\nNothing restricted.");
    } else {
        human.push_str(&format!("\nRestricted: {}", restrictions.join(", ")));
        human.push_str(
            "\nPermission flags are advisory: conforming readers honour them, nothing enforces them.",
        );
    }

    Ok(FileReport {
        human,
        json: json!({
            "input": path.display().to_string(),
            "output": target.display().to_string(),
            "algorithm": settings.algorithm.label(),
            "key_bits": settings.algorithm.key_bits(),
            "open_password_required": !settings.user_password.is_empty(),
            "restrictions": restrictions,
            "bytes": bytes.len(),
        }),
        bytes_in: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        bytes_out: bytes.len() as u64,
        flagged: false,
    })
}

/// `decrypt` — write the document out without its protection (spec §8).
pub fn decrypt(
    path: &Path,
    args: &DecryptArgs,
    password: Option<&str>,
    many: bool,
    overwrite: bool,
    _cancel: &CancelToken,
) -> Result<FileReport> {
    let target = paths::output_for(&args.output, path, many, "-decrypted")?;
    paths::guard(&target, overwrite)?;

    let mut pdf = super::open_input(path, password)?;
    if !pdf.was_encrypted() {
        return Err(Error::Config {
            detail: format!("{} is not encrypted", path.display()),
            source_path: Some(path.to_path_buf()),
        });
    }

    let info = ypdf_crypt::security_info(&pdf);
    // Opening decrypted it; writing it plainly is all that is left to do.
    let bytes = pdf.to_bytes()?;
    paths::write(&target, &bytes, overwrite)?;

    Ok(FileReport {
        human: format!(
            "Removed {} protection → {} ({})",
            info.algorithm.as_deref().unwrap_or("unknown"),
            target.display(),
            ypdf_optimize::format_size(bytes.len() as u64)
        ),
        json: json!({
            "input": path.display().to_string(),
            "output": target.display().to_string(),
            "was": info.algorithm,
            "bytes": bytes.len(),
        }),
        bytes_in: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        bytes_out: bytes.len() as u64,
        flagged: false,
    })
}

/// The algorithm named on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum AlgorithmArg {
    /// AES-128: readable by essentially every PDF reader.
    Aes128,
    /// AES-256: stronger, needs a reader from about 2017 or later.
    Aes256,
}

impl From<AlgorithmArg> for Algorithm {
    fn from(value: AlgorithmArg) -> Self {
        match value {
            AlgorithmArg::Aes128 => Self::Aes128,
            AlgorithmArg::Aes256 => Self::Aes256,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use ypdf_doc::Pdf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("ypdf-cli-tests").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");
        dir
    }

    fn encrypt_args(output: PathBuf) -> EncryptArgs {
        EncryptArgs {
            inputs: Vec::new(),
            output,
            user_password: Some("open-me".into()),
            owner_password: None,
            algorithm: AlgorithmArg::Aes256,
            no_print: false,
            no_high_quality_print: false,
            no_modify: false,
            no_copy: false,
            no_annotate: false,
            no_forms: false,
            no_assemble: false,
        }
    }

    #[test]
    fn nothing_is_restricted_unless_it_was_asked_for() {
        let permissions = permissions_from(&encrypt_args(PathBuf::new()));
        assert!(permissions.is_unrestricted());
    }

    #[test]
    fn denying_printing_denies_the_degraded_form_too() {
        let mut args = encrypt_args(PathBuf::new());
        args.no_print = true;
        let permissions = permissions_from(&args);
        assert!(!permissions.print);
        assert!(!permissions.print_high_quality);
    }

    #[test]
    fn encrypting_writes_a_file_that_needs_the_password() {
        let dir = scratch("encrypt");
        let target = dir.join("out.pdf");
        let args = encrypt_args(target.clone());

        let report = encrypt(
            &fixture("many-pages.pdf"),
            &args,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("encrypts");

        assert_eq!(report.json["algorithm"], "AES-256");
        assert!(Pdf::open(&target).is_err(), "it must need the password");
        assert_eq!(
            Pdf::open_with_password(&target, "open-me")
                .expect("opens")
                .page_count(),
            12
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_round_trip_through_encrypt_and_decrypt_returns_the_document() {
        let dir = scratch("round-trip");
        let protected = dir.join("protected.pdf");
        let plain = dir.join("plain.pdf");

        encrypt(
            &fixture("many-pages.pdf"),
            &encrypt_args(protected.clone()),
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("encrypts");

        decrypt(
            &protected,
            &DecryptArgs {
                inputs: Vec::new(),
                output: plain.clone(),
            },
            Some("open-me"),
            false,
            false,
            &CancelToken::never(),
        )
        .expect("decrypts");

        assert_eq!(Pdf::open(&plain).expect("opens plainly").page_count(), 12);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decrypting_needs_the_password_rather_than_finding_a_way_round_it() {
        let dir = scratch("decrypt-no-password");
        let protected = dir.join("protected.pdf");
        encrypt(
            &fixture("many-pages.pdf"),
            &encrypt_args(protected.clone()),
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("encrypts");

        let result = decrypt(
            &protected,
            &DecryptArgs {
                inputs: Vec::new(),
                output: dir.join("plain.pdf"),
            },
            None,
            false,
            false,
            &CancelToken::never(),
        );

        assert!(result.is_err(), "no password must mean no document");
        assert_eq!(
            result.err().map(|e| e.code()),
            Some("E_PASSWORD_REQUIRED"),
            "and it must say so plainly"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decrypting_an_unprotected_document_says_so_rather_than_writing_a_copy() {
        let dir = scratch("decrypt-plain");
        let target = dir.join("out.pdf");

        let result = decrypt(
            &fixture("many-pages.pdf"),
            &DecryptArgs {
                inputs: Vec::new(),
                output: target.clone(),
            },
            None,
            false,
            false,
            &CancelToken::never(),
        );

        assert!(result.is_err());
        assert!(!target.exists(), "nothing should have been written");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
