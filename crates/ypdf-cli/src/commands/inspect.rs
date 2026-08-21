//! Commands that read a document and report on it (spec §13, §14, §26).

use std::path::Path;

use serde_json::{Value, json};
use ypdf_core::{CancelToken, Error, Result};
use ypdf_doc::{MetadataEdit, Pdf, Severity};

use crate::batch::FileReport;
use crate::cli::MetadataArgs;
use crate::paths;

/// `info` — the short form: what this file is.
pub fn info(path: &Path, password: Option<&str>, _cancel: &CancelToken) -> Result<FileReport> {
    let pdf = super::open_input(path, password)?;
    let diagnostics = pdf.diagnostics()?;
    let metadata = pdf.metadata();

    let size = diagnostics.file_size.unwrap_or(0);
    let security = ypdf_crypt::security_info(&pdf);
    let human = format!(
        "File:      {}\nPages:     {}\nSize:      {}\nVersion:   PDF {}\nTitle:     {}\nEncrypted: {}",
        path.display(),
        diagnostics.pages,
        ypdf_optimize::format_size(size),
        diagnostics.version,
        metadata.title.as_deref().unwrap_or("(none)"),
        // What the file carried, not what the parser is holding: the objects
        // are plaintext in memory by the time anyone asks.
        security
            .algorithm
            .clone()
            .unwrap_or_else(|| "no".to_string()),
    );

    Ok(FileReport {
        human,
        json: json!({
            "pages": diagnostics.pages,
            "bytes": size,
            "version": diagnostics.version,
            "title": metadata.title,
            "author": metadata.author,
            "encrypted": security.encrypted,
            "encryption": security.algorithm,
            "open_password_required": security.user_password_required,
            "linearized": diagnostics.linearized,
        }),
        ..FileReport::default()
    })
}

/// `diagnostics` — the long form, including what is wrong (spec §14).
pub fn diagnostics(
    path: &Path,
    password: Option<&str>,
    _cancel: &CancelToken,
) -> Result<FileReport> {
    let pdf = super::open_input(path, password)?;
    let d = pdf.diagnostics()?;
    let security = ypdf_crypt::security_info(&pdf);

    let mut human = format!(
        "PDF DIAGNOSTICS\n\n\
         Version:        PDF {}\n\
         Pages:          {}\n\
         Size:           {}\n\
         Objects:        {}\n\
         Fonts:          {}\n\
         Images:         {}\n\
         Annotations:    {}\n\
         Form fields:    {}\n\
         Embedded files: {}\n\
         Encrypted:      {}\n\
         Linearized:     {}\n",
        d.version,
        d.pages,
        ypdf_optimize::format_size(d.file_size.unwrap_or(0)),
        d.objects,
        d.fonts,
        d.images,
        d.annotations,
        d.form_fields,
        d.embedded_files,
        yes_no(d.encrypted),
        yes_no(d.linearized),
    );

    if security.encrypted {
        human.push('\n');
        human.push_str(&security.to_human());
    }

    if let Some(claim) = &d.pdf_a_claim {
        // A claim, not a verdict: this line reports what the file says about
        // itself, and `ypdf-cli pdfa` is the command that checks it.
        human.push_str(&format!(
            "PDF/A:          claims {claim} — checked by `ypdf-cli pdfa`\n"
        ));
    }

    if d.issues.is_empty() {
        human.push_str("\nNo structural problems found.\n");
    } else {
        human.push_str("\nIssues:\n");
        for issue in &d.issues {
            human.push_str(&format!(
                "{:<4} [{}] {}\n",
                issue.severity.marker(),
                issue.code,
                issue.message
            ));
        }
    }

    let issues: Vec<Value> = d
        .issues
        .iter()
        .map(|issue| {
            json!({
                "severity": severity_name(issue.severity),
                "code": issue.code,
                "message": issue.message,
            })
        })
        .collect();

    Ok(FileReport {
        human,
        json: json!({
            "version": d.version,
            "pages": d.pages,
            "bytes": d.file_size,
            "objects": d.objects,
            "fonts": d.fonts,
            "images": d.images,
            "annotations": d.annotations,
            "form_fields": d.form_fields,
            "embedded_files": d.embedded_files,
            "encrypted": security.encrypted,
            "encryption": security.algorithm,
            "open_password_required": security.user_password_required,
            "restrictions": security.permissions.restrictions(),
            "linearized": d.linearized,
            "pdf_a_claim": d.pdf_a_claim,
            "issues": issues,
        }),
        ..FileReport::default()
    })
}

/// `security-scan` (spec §26).
///
/// Reads only. Nothing found here is executed, followed, or fetched, and the
/// URLs are printed as text rather than as anything a terminal might turn into
/// a link.
pub fn security_scan(
    path: &Path,
    fail_on: Option<Severity>,
    password: Option<&str>,
    _cancel: &CancelToken,
) -> Result<FileReport> {
    let pdf = super::open_input(path, password)?;
    let report = ypdf_security::scan(&pdf);
    let protection = ypdf_crypt::security_info(&pdf);

    let mut human = report.to_human();
    if protection.encrypted {
        human.push('\n');
        human.push_str(&protection.to_human());
    }
    if !report.urls.is_empty() {
        human.push_str("\nExternal links:\n");
        for url in &report.urls {
            human.push_str(&format!("  {url}\n"));
        }
    }

    let findings: Vec<Value> = report
        .findings
        .iter()
        .map(|finding| {
            json!({
                "severity": severity_name(finding.severity),
                "code": finding.code,
                "title": finding.title,
                "detail": finding.detail,
                "count": finding.count,
            })
        })
        .collect();

    let worst = report.worst();
    let flagged = match (fail_on, worst) {
        (Some(threshold), Some(worst)) => worst >= threshold,
        _ => false,
    };
    if flagged {
        human.push_str("\nAt or above the --fail-on threshold.\n");
    }

    Ok(FileReport {
        human,
        json: json!({
            "clean": report.is_clean(),
            "worst": worst.map(severity_name),
            "javascript": report.javascript,
            "launch_actions": report.launch_actions,
            "auto_actions": report.auto_actions,
            "embedded_files": report.embedded_files,
            "encrypted": protection.encrypted,
            "encryption": protection.algorithm,
            "restrictions": protection.permissions.restrictions(),
            "signed": report.signed,
            "urls": report.urls,
            "findings": findings,
            "flagged": flagged,
        }),
        flagged,
        ..FileReport::default()
    })
}

/// `metadata` — read it, or edit it (spec §13).
pub fn metadata(
    path: &Path,
    args: &MetadataArgs,
    password: Option<&str>,
    many: bool,
    overwrite: bool,
    _cancel: &CancelToken,
) -> Result<FileReport> {
    let mut pdf = super::open_input(path, password)?;

    if !args.is_edit() {
        return Ok(read_metadata(&pdf));
    }

    if args.strip {
        ypdf_optimize::strip_metadata(&mut pdf);
    } else {
        pdf.set_metadata(&MetadataEdit {
            title: args.set_title.clone(),
            author: args.set_author.clone(),
            subject: args.set_subject.clone(),
            keywords: args.set_keywords.clone(),
        })?;
    }

    // Writing back over the input is an overwrite like any other, and needs to
    // be asked for: `--set-title` on a thousand files should not be able to
    // rewrite a thousand originals by accident.
    let (target, in_place) = match &args.output {
        Some(output) => (paths::output_for(output, path, many, "")?, false),
        None => (path.to_path_buf(), true),
    };
    if in_place && !overwrite {
        return Err(Error::Config {
            detail: format!(
                "editing {} in place replaces it. Pass --overwrite, or --output to write elsewhere.",
                path.display()
            ),
            source_path: Some(path.to_path_buf()),
        });
    }

    let bytes = pdf.to_bytes()?;
    paths::write(&target, &bytes, overwrite || in_place)?;

    if pdf.was_encrypted() {
        // Editing decrypts, and writing plainly is what the caller asked for.
        // Saying so is the difference between a tool and a trap.
        tracing::warn!(
            path = %target.display(),
            "the document was protected; the copy written is not"
        );
    }

    let mut report = read_metadata(&pdf);
    report.human = format!("{}\n\nWritten to {}", report.human, target.display());
    if let Value::Object(object) = &mut report.json {
        object.insert(
            "written_to".into(),
            Value::String(target.display().to_string()),
        );
    }
    Ok(report)
}

fn read_metadata(pdf: &Pdf) -> FileReport {
    let m = pdf.metadata();
    let human = format!(
        "Title:      {}\n\
         Author:     {}\n\
         Subject:    {}\n\
         Keywords:   {}\n\
         Creator:    {}\n\
         Producer:   {}\n\
         Created:    {}\n\
         Modified:   {}\n\
         Version:    PDF {}\n\
         Pages:      {}\n\
         XMP:        {}",
        or_none(m.title.as_deref()),
        or_none(m.author.as_deref()),
        or_none(m.subject.as_deref()),
        or_none(m.keywords.as_deref()),
        or_none(m.creator.as_deref()),
        or_none(m.producer.as_deref()),
        or_none(m.created.as_deref().map(ypdf_doc::format_date).as_deref()),
        or_none(m.modified.as_deref().map(ypdf_doc::format_date).as_deref()),
        m.version,
        m.page_count,
        if m.has_xmp { "present" } else { "none" },
    );

    FileReport::text(
        human,
        json!({
            "title": m.title,
            "author": m.author,
            "subject": m.subject,
            "keywords": m.keywords,
            "creator": m.creator,
            "producer": m.producer,
            "created": m.created,
            "modified": m.modified,
            "version": m.version,
            "pages": m.page_count,
            "has_xmp": m.has_xmp,
        }),
    )
}

fn or_none(value: Option<&str>) -> &str {
    value.unwrap_or("(none)")
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// The lowercase name used in JSON, so a script can compare strings.
const fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::High => "high",
        Severity::Critical => "critical",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    #[test]
    fn info_reports_the_page_count_in_both_forms() {
        let report = info(&fixture("many-pages.pdf"), None, &CancelToken::never()).expect("reads");
        assert_eq!(report.json["pages"], 12);
        assert!(report.human.contains("Pages:     12"), "{}", report.human);
    }

    #[test]
    fn diagnostics_names_every_issue_with_its_code() {
        let report =
            diagnostics(&fixture("dangling.pdf"), None, &CancelToken::never()).expect("reads");
        let issues = report.json["issues"].as_array().expect("an array");
        assert!(!issues.is_empty(), "the fixture has a dangling reference");
        assert!(issues.iter().all(|i| i["code"].is_string()));
    }

    #[test]
    fn a_hostile_file_scans_dirty_and_a_plain_one_does_not() {
        let hostile = security_scan(&fixture("hostile.pdf"), None, None, &CancelToken::never())
            .expect("scans");
        assert_eq!(hostile.json["clean"], false);
        assert_eq!(hostile.json["javascript"], true);

        let plain = security_scan(
            &fixture("many-pages.pdf"),
            None,
            None,
            &CancelToken::never(),
        )
        .expect("scans");
        assert_eq!(plain.json["clean"], true);
    }

    #[test]
    fn fail_on_flags_a_file_without_failing_to_read_it() {
        let report = security_scan(
            &fixture("hostile.pdf"),
            Some(Severity::Warning),
            None,
            &CancelToken::never(),
        )
        .expect("scans");
        assert!(report.flagged);
        assert_eq!(report.json["flagged"], true);
    }

    #[test]
    fn a_threshold_above_what_was_found_does_not_flag() {
        let report = security_scan(
            &fixture("many-pages.pdf"),
            Some(Severity::Critical),
            None,
            &CancelToken::never(),
        )
        .expect("scans");
        assert!(!report.flagged);
    }

    #[test]
    fn metadata_is_read_without_touching_the_file() {
        let path = fixture("metadata.pdf");
        let before = std::fs::metadata(&path).expect("stat").len();

        let args = MetadataArgs {
            inputs: vec![path.display().to_string()],
            set_title: None,
            set_author: None,
            set_subject: None,
            set_keywords: None,
            strip: false,
            output: None,
        };
        let report =
            metadata(&path, &args, None, false, false, &CancelToken::never()).expect("reads");

        assert!(report.json["title"].is_string());
        assert_eq!(std::fs::metadata(&path).expect("stat").len(), before);
    }

    #[test]
    fn editing_in_place_is_refused_without_overwrite() {
        let path = fixture("metadata.pdf");
        let args = MetadataArgs {
            inputs: vec![path.display().to_string()],
            set_title: Some("New".into()),
            set_author: None,
            set_subject: None,
            set_keywords: None,
            strip: false,
            output: None,
        };
        let result = metadata(&path, &args, None, false, false, &CancelToken::never());
        assert!(result.is_err(), "an in-place edit must be asked for");
    }
}
