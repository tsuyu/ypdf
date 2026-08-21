//! `pdfa` — check a document against PDF/A (spec §15).

use std::path::Path;

use serde_json::{Value, json};
use ypdf_core::{CancelToken, Result};
use ypdf_pdfa::{LIMITS, Level, validate};

use crate::batch::FileReport;

/// Check one file.
///
/// The command exits non-zero when the check fails, because that is the whole
/// question it was asked. Nothing is written: this reads.
pub fn run(
    path: &Path,
    level: Option<Level>,
    password: Option<&str>,
    _cancel: &CancelToken,
) -> Result<FileReport> {
    let pdf = super::open_input(path, password)?;
    let report = validate(&pdf, level);

    let violations: Vec<Value> = report
        .violations
        .iter()
        .map(|violation| {
            json!({
                "code": violation.code,
                "requirement": violation.requirement,
                "message": violation.message,
                "count": violation.count,
            })
        })
        .collect();

    Ok(FileReport {
        human: report.to_human(),
        json: json!({
            "claimed": report.claimed.map(|level| level.to_string()),
            "checked": report.checked.to_string(),
            // Named `passed`, not `compliant`: what this ran is the list of
            // checks in `not_checked`'s complement, and calling that
            // compliance would be a claim nobody here can make.
            "passed": report.passed,
            "violations": violations,
            "not_checked": LIMITS,
        }),
        flagged: !report.passed,
        ..FileReport::default()
    })
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
    fn an_ordinary_file_fails_and_says_why() {
        let report = run(&fixture("two-pages.pdf"), None, None, &CancelToken::new())
            .expect("the check runs");
        assert!(report.flagged, "a file with no XMP is not PDF/A");
        assert!(report.human.contains("PDFA_NO_XMP"));
        // Even a failure prints what was not looked at, so nobody reads the
        // violation list as exhaustive.
        assert!(report.human.contains("Not checked here:"));
    }

    #[test]
    fn the_level_asked_for_is_the_level_reported() {
        let level = Level::parse("1b").expect("level");
        let report = run(
            &fixture("two-pages.pdf"),
            Some(level),
            None,
            &CancelToken::new(),
        )
        .expect("the check runs");
        assert_eq!(report.json["checked"], "PDF/A-1b");
    }
}
