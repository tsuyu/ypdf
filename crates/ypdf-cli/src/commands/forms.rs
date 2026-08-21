//! `forms` — list, fill, clear, flatten, and move form data (spec §16).
//!
//! Reads by default. Every write says what it did *and what it could not do*:
//! a form command that reports success while a field name was misspelled is how
//! an application gets sent in blank.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::json;
use ypdf_core::{CancelToken, Error, Result};
use ypdf_forms::{Format, Report};

use crate::batch::FileReport;
use crate::cli::FormsArgs;
use crate::paths;

/// Run over one file.
pub fn run(
    path: &Path,
    args: &FormsArgs,
    password: Option<&str>,
    many: bool,
    overwrite: bool,
    _cancel: &CancelToken,
) -> Result<FileReport> {
    let mut pdf = super::open_input(path, password)?;

    if !ypdf_forms::has_form(&pdf) {
        // Said plainly rather than reported as an empty list: "no fields" and
        // "not a form" are different answers, and only one of them means the
        // file is not the one you meant.
        if args.is_edit() {
            return Err(Error::Config {
                detail: format!("{} has no form to fill in", path.display()),
                source_path: Some(path.to_path_buf()),
            });
        }
        return Ok(FileReport::text(
            "This document has no form.",
            json!({
                "input": path.display().to_string(),
                "has_form": false,
                "fields": [],
            }),
        ));
    }

    let fields = ypdf_forms::read(&pdf);

    // Exporting is a read, and can be asked for alongside anything else.
    if let Some(target) = &args.export {
        let format = args
            .format
            .map_or_else(|| Format::from_path(target), Into::into);
        let text = ypdf_forms::export(&ypdf_forms::values_of(&fields), format)?;
        paths::guard(target, overwrite)?;
        std::fs::write(target, text).map_err(|e| Error::io(target, e))?;
    }

    if !args.is_edit() {
        let mut report = listing(&fields, path);
        if let Some(target) = &args.export {
            report
                .human
                .push_str(&format!("\n\nData written to {}", target.display()));
        }
        return Ok(report);
    }

    let mut values: BTreeMap<String, String> = BTreeMap::new();
    if let Some(source) = &args.import {
        let text = std::fs::read_to_string(source).map_err(|e| Error::io(source, e))?;
        let format = args
            .format
            .map_or_else(|| Format::from_path(source), Into::into);
        values.extend(ypdf_forms::import(&text, format)?);
    }
    for entry in &args.fill {
        let (name, value) = entry.split_once('=').ok_or_else(|| Error::Config {
            detail: format!("{entry:?} is not a value; expected name=value"),
            source_path: None,
        })?;
        values.insert(name.trim().to_string(), value.to_string());
    }

    let mut report = Report::default();
    if args.clear {
        report = ypdf_forms::clear(&mut pdf)?;
    }
    if !values.is_empty() {
        let filled = ypdf_forms::fill(&mut pdf, &values)?;
        report.filled = filled.filled;
        report.unknown = filled.unknown;
        report.refused = filled.refused;
    }
    if args.flatten {
        report.flattened = ypdf_forms::flatten(&mut pdf)?.flattened;
    }

    let target = match &args.output {
        Some(output) => paths::output_for(output, path, many, "-filled")?,
        None => {
            if !overwrite {
                return Err(Error::Config {
                    detail: format!(
                        "editing {} in place replaces it. Pass --overwrite, or --output to \
                         write elsewhere.",
                        path.display()
                    ),
                    source_path: Some(path.to_path_buf()),
                });
            }
            path.to_path_buf()
        }
    };

    let bytes = pdf.to_bytes()?;
    paths::write(&target, &bytes, true)?;

    let mut human = report.to_human();
    human.push_str(&format!("\nWritten to {}\n", target.display()));

    Ok(FileReport {
        human,
        json: json!({
            "input": path.display().to_string(),
            "output": target.display().to_string(),
            "filled": report.filled,
            "cleared": report.cleared,
            "flattened": report.flattened,
            "unknown": report.unknown,
            "refused": report
                .refused
                .iter()
                .map(|(name, reason)| json!({ "field": name, "reason": reason }))
                .collect::<Vec<_>>(),
            "complete": report.is_complete(),
            "bytes": bytes.len(),
        }),
        bytes_in: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        bytes_out: bytes.len() as u64,
        // A fill that could not do everything asked is worth a non-zero exit:
        // in a script, that is the difference between sending the form and
        // looking at it first.
        flagged: !report.is_complete(),
    })
}

fn listing(fields: &[ypdf_forms::Field], path: &Path) -> FileReport {
    let human = if fields.is_empty() {
        "This form has no fields.".to_string()
    } else {
        fields
            .iter()
            .map(|field| {
                let mut line = format!(
                    "{:<24} {:<9} {}",
                    field.name,
                    field.kind.label(),
                    if field.value.is_empty() {
                        "—"
                    } else {
                        &field.value
                    }
                );
                if !field.options.is_empty() {
                    line.push_str(&format!("   [{}]", field.options.join(", ")));
                }
                if field.read_only {
                    line.push_str("   (read-only)");
                }
                if field.required {
                    line.push_str("   (required)");
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    FileReport::text(
        human,
        json!({
            "input": path.display().to_string(),
            "has_form": true,
            "fields": fields,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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

    fn args() -> FormsArgs {
        FormsArgs {
            inputs: Vec::new(),
            output: None,
            fill: Vec::new(),
            import: None,
            export: None,
            format: None,
            clear: false,
            flatten: false,
        }
    }

    #[test]
    fn listing_a_form_does_not_touch_it() {
        let path = fixture("form.pdf");
        let before = std::fs::read(&path).expect("reads");

        let report = run(&path, &args(), None, false, false, &CancelToken::never()).expect("lists");

        assert_eq!(report.json["has_form"], true);
        assert_eq!(report.json["fields"].as_array().map(Vec::len), Some(5));
        assert_eq!(std::fs::read(&path).expect("reads"), before);
    }

    #[test]
    fn a_document_with_no_form_says_so_rather_than_listing_nothing() {
        let report = run(
            &fixture("two-pages.pdf"),
            &args(),
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("runs");
        assert_eq!(report.json["has_form"], false);
        assert!(report.human.contains("no form"));
    }

    #[test]
    fn filling_a_document_with_no_form_is_an_error_rather_than_a_copy() {
        let dir = scratch("forms-no-form");
        let mut args = args();
        args.fill = vec!["name=Ada".into()];
        args.output = Some(dir.join("out.pdf"));

        let result = run(
            &fixture("two-pages.pdf"),
            &args,
            None,
            false,
            false,
            &CancelToken::never(),
        );
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fill_entry_must_look_like_a_value() {
        let dir = scratch("forms-bad-entry");
        let mut args = args();
        args.fill = vec!["just-a-name".into()];
        args.output = Some(dir.join("out.pdf"));

        let error = run(
            &fixture("form.pdf"),
            &args,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect_err("refused");
        assert!(error.report().to_human().contains("name=value"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn filling_writes_the_values_and_reports_completeness() {
        let dir = scratch("forms-fill");
        let mut args = args();
        args.fill = vec!["name=Ada Lovelace".into(), "subscribe=yes".into()];
        args.output = Some(dir.join("out.pdf"));

        let report = run(
            &fixture("form.pdf"),
            &args,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("fills");

        assert_eq!(report.json["filled"], 2);
        assert_eq!(report.json["complete"], true);
        assert!(!report.flagged);

        let pdf = ypdf_doc::Pdf::open(dir.join("out.pdf")).expect("re-opens");
        let fields = ypdf_forms::read(&pdf);
        assert_eq!(
            fields
                .iter()
                .find(|field| field.name == "name")
                .map(|field| field.value.as_str()),
            Some("Ada Lovelace")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_misspelled_field_flags_the_run_rather_than_passing_quietly() {
        // In a script this is the difference between sending the form and
        // looking at it first.
        let dir = scratch("forms-typo");
        let mut args = args();
        args.fill = vec!["nmae=Ada".into()];
        args.output = Some(dir.join("out.pdf"));

        let report = run(
            &fixture("form.pdf"),
            &args,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("runs");

        assert!(report.flagged);
        assert_eq!(report.json["complete"], false);
        assert_eq!(report.json["unknown"][0], "nmae");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn data_can_be_exported_and_imported_again() {
        let dir = scratch("forms-data");

        let mut fill_args = args();
        fill_args.fill = vec!["name=Ada Lovelace".into()];
        fill_args.output = Some(dir.join("filled.pdf"));
        run(
            &fixture("form.pdf"),
            &fill_args,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("fills");

        let mut export_args = args();
        export_args.export = Some(dir.join("data.fdf"));
        run(
            &dir.join("filled.pdf"),
            &export_args,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("exports");
        assert!(dir.join("data.fdf").is_file());

        let mut import_args = args();
        import_args.import = Some(dir.join("data.fdf"));
        import_args.output = Some(dir.join("second.pdf"));
        let report = run(
            &fixture("form.pdf"),
            &import_args,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("imports");

        assert!(report.json["filled"].as_u64().unwrap_or(0) >= 1);
        let pdf = ypdf_doc::Pdf::open(dir.join("second.pdf")).expect("re-opens");
        assert_eq!(
            ypdf_forms::read(&pdf)
                .iter()
                .find(|field| field.name == "name")
                .map(|field| field.value.clone()),
            Some("Ada Lovelace".to_string())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flattening_leaves_a_document_with_no_fields() {
        let dir = scratch("forms-flatten");
        let mut args = args();
        args.fill = vec!["name=Ada".into()];
        args.flatten = true;
        args.output = Some(dir.join("out.pdf"));

        let report = run(
            &fixture("form.pdf"),
            &args,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("flattens");

        assert!(report.json["flattened"].as_u64().unwrap_or(0) > 0);
        let pdf = ypdf_doc::Pdf::open(dir.join("out.pdf")).expect("re-opens");
        assert!(!ypdf_forms::has_form(&pdf));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editing_in_place_is_refused_without_permission() {
        let mut args = args();
        args.fill = vec!["name=Ada".into()];

        let result = run(
            &fixture("form.pdf"),
            &args,
            None,
            false,
            false,
            &CancelToken::never(),
        );
        assert!(result.is_err());
    }
}
