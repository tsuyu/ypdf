//! The scanner against a deliberately hostile fixture.
//!
//! `hostile.pdf` carries an open-action that runs JavaScript, a launch action,
//! an embedded `.exe`, a `javascript:` link, a link to a bare IP address, an
//! ordinary link, event-triggered actions, and a rich-media annotation — all
//! inert. Nothing in these tests executes, follows, or fetches any of it.

// Integration tests compile as their own crate, so `cfg(test)` is not set and
// the clippy.toml allowance for tests does not reach here.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use ypdf_doc::{Pdf, Severity};
use ypdf_security::{ScanReport, scan};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn open(name: &str) -> Pdf {
    match Pdf::open(fixture(name)) {
        Ok(pdf) => pdf,
        Err(e) => panic!("{}", e.report().to_human()),
    }
}

fn codes(report: &ScanReport) -> Vec<&'static str> {
    report.findings.iter().map(|f| f.code).collect()
}

#[test]
fn a_plain_document_scans_clean() {
    let report = scan(&open("many-pages.pdf"));
    assert!(
        report.is_clean(),
        "unexpected findings: {:?}",
        codes(&report)
    );
    assert!(!report.javascript);
    assert_eq!(report.launch_actions, 0);
    assert_eq!(report.embedded_files, 0);
    assert!(report.urls.is_empty());
}

#[test]
fn ordinary_links_are_reported_without_alarm() {
    let report = scan(&open("linked.pdf"));
    assert_eq!(report.urls.len(), 1);
    assert_eq!(report.urls[0], "https://example.org/docs");
    assert!(
        report.is_clean(),
        "an https link is not a finding: {:?}",
        codes(&report)
    );
}

#[test]
fn the_hostile_fixture_trips_every_detector() {
    let report = scan(&open("hostile.pdf"));
    let found = codes(&report);

    for expected in [
        "S_LAUNCH_ACTION",
        "S_EMBEDDED_EXECUTABLE",
        "S_JAVASCRIPT",
        "S_OPEN_ACTION",
        "S_ADDITIONAL_ACTIONS",
        "S_URL_SCRIPT",
        "S_URL_IP_ADDRESS",
        "S_RICH_MEDIA",
    ] {
        assert!(
            found.contains(&expected),
            "missing {expected}; found {found:?}"
        );
    }

    assert!(report.javascript);
    assert_eq!(report.launch_actions, 1);
    assert!(report.auto_actions);
    assert!(!report.is_clean());
    assert_eq!(report.worst(), Some(Severity::Critical));
}

#[test]
fn findings_are_ordered_worst_first() {
    let report = scan(&open("hostile.pdf"));
    let severities: Vec<Severity> = report.findings.iter().map(|f| f.severity).collect();
    let mut sorted = severities.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(
        severities, sorted,
        "a critical finding must not sit below a warning"
    );
    assert_eq!(report.findings[0].severity, Severity::Critical);
}

#[test]
fn an_embedded_executable_is_critical_and_names_the_file() {
    let report = scan(&open("hostile.pdf"));
    let finding = report
        .findings
        .iter()
        .find(|f| f.code == "S_EMBEDDED_EXECUTABLE")
        .expect("the .exe attachment");

    assert_eq!(finding.severity, Severity::Critical);
    assert!(
        finding.detail.contains("payload.exe"),
        "got {:?}",
        finding.detail
    );
}

#[test]
fn the_javascript_finding_says_it_cannot_run() {
    // The report has to be clear that detection is not execution.
    let report = scan(&open("hostile.pdf"));
    let finding = report
        .findings
        .iter()
        .find(|f| f.code == "S_JAVASCRIPT")
        .expect("the script finding");
    assert_eq!(finding.severity, Severity::High);
    assert!(
        finding.detail.contains("no JavaScript engine"),
        "the finding must say the script cannot run here: {:?}",
        finding.detail
    );
}

#[test]
fn the_launch_finding_says_it_is_never_followed() {
    let report = scan(&open("hostile.pdf"));
    let finding = report
        .findings
        .iter()
        .find(|f| f.code == "S_LAUNCH_ACTION")
        .expect("the launch action");
    assert_eq!(finding.severity, Severity::Critical);
    assert!(
        finding.detail.contains("never follows"),
        "got {:?}",
        finding.detail
    );
}

#[test]
fn every_url_is_collected_and_classified() {
    let report = scan(&open("hostile.pdf"));
    assert_eq!(report.urls.len(), 3, "got {:?}", report.urls);

    let script = report
        .findings
        .iter()
        .find(|f| f.code == "S_URL_SCRIPT")
        .expect("script link");
    assert_eq!(script.severity, Severity::Critical);

    let ip = report
        .findings
        .iter()
        .find(|f| f.code == "S_URL_IP_ADDRESS")
        .expect("ip link");
    assert_eq!(ip.severity, Severity::Warning);
}

#[test]
fn the_text_report_follows_the_spec_shape() {
    let report = scan(&open("hostile.pdf"));
    let text = report.to_human();
    assert!(text.starts_with("PDF SECURITY SCAN"));
    assert!(text.contains("Launch action"));
    assert!(text.contains("JavaScript"));
}

#[test]
fn scanning_never_modifies_the_document() {
    let before = std::fs::read(fixture("hostile.pdf")).expect("readable");
    let _ = scan(&open("hostile.pdf"));
    let after = std::fs::read(fixture("hostile.pdf")).expect("readable");
    assert_eq!(before, after, "the scanner is read-only");
}

#[test]
fn diagnostics_count_what_is_in_the_file() {
    let pdf = open("hostile.pdf");
    let report = pdf.diagnostics().expect("diagnostics");

    assert_eq!(report.pages, 1);
    assert_eq!(report.version, "1.7");
    assert_eq!(report.fonts, 1);
    assert_eq!(report.annotations, 4);
    assert!(
        report.embedded_files >= 1,
        "the .exe attachment must be counted"
    );
    assert!(report.file_size.is_some_and(|size| size > 0));
    assert!(!report.encrypted);
}

#[test]
fn diagnostics_notice_a_document_with_no_fonts() {
    // Which is the signal that its pages are scans and want OCR.
    let pdf = open("no-fonts.pdf");
    let report = pdf.diagnostics().expect("diagnostics");
    assert_eq!(report.fonts, 0);
    assert!(
        report.issues.iter().any(|i| i.code == "D_NO_FONTS"),
        "expected the no-fonts note, got {:?}",
        report.issues
    );
}

#[test]
fn diagnostics_report_a_dangling_reference() {
    let pdf = open("dangling.pdf");
    let report = pdf.diagnostics().expect("diagnostics");
    let issue = report
        .issues
        .iter()
        .find(|i| i.code == "D_DANGLING_REFERENCE")
        .expect("the broken reference");
    assert_eq!(issue.severity, Severity::High);
}

#[test]
fn metadata_reads_the_info_dictionary() {
    let pdf = open("metadata.pdf");
    let meta = pdf.metadata();

    assert_eq!(meta.title.as_deref(), Some("Quarterly Report"));
    assert_eq!(meta.author.as_deref(), Some("Ada Lovelace"));
    assert_eq!(meta.keywords.as_deref(), Some("finance, quarterly"));
    assert_eq!(meta.created.as_deref(), Some("2024-03-05 14:22:01 +08:00"));
    assert_eq!(meta.version, "1.7");
    assert_eq!(meta.page_count, 1);
}

#[test]
fn metadata_edits_survive_a_save() {
    let mut pdf = open("metadata.pdf");
    let edit = ypdf_doc::MetadataEdit {
        title: Some("Renamed — with an em dash".into()),
        author: Some(String::new()),
        ..ypdf_doc::MetadataEdit::default()
    };
    pdf.set_metadata(&edit).expect("sets metadata");

    let bytes = pdf.to_bytes().expect("serializes");
    let reopened = Pdf::from_bytes(&bytes).expect("re-opens");
    let meta = reopened.metadata();

    assert_eq!(meta.title.as_deref(), Some("Renamed — with an em dash"));
    assert_eq!(
        meta.author, None,
        "an emptied field is removed, not blanked"
    );
    assert_eq!(
        meta.keywords.as_deref(),
        Some("finance, quarterly"),
        "untouched fields stay"
    );
}
