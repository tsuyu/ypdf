//! Commands that rearrange pages (spec §3).

use std::path::PathBuf;

use serde_json::json;
use ypdf_core::{CancelToken, Error, Result};
use ypdf_doc::{PageSpec, Pdf, SplitMode};

use crate::batch::FileReport;
use crate::cli::{ExtractArgs, MergeArgs, SplitArgs};
use crate::output::Out;
use crate::paths;

/// `merge` — many documents into one (spec §3.1).
///
/// Not a batch command: it is many inputs and one output by definition, so it
/// reports once rather than per file.
pub fn merge(
    files: &[PathBuf],
    args: &MergeArgs,
    password: Option<&str>,
    overwrite: bool,
    cancel: &CancelToken,
    out: &Out,
) -> Result<FileReport> {
    if files.len() < 2 {
        return Err(Error::Config {
            detail: "merge needs at least two documents".into(),
            source_path: None,
        });
    }
    paths::guard(&args.output, overwrite)?;

    let mut documents = Vec::with_capacity(files.len());
    let mut bytes_in = 0;
    for (n, path) in files.iter().enumerate() {
        cancel.check()?;
        out.note(format!(
            "[{}/{}] reading {}",
            n + 1,
            files.len(),
            path.display()
        ));
        bytes_in += std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        documents.push(super::open_input(path, password)?);
    }

    let pages_each: Vec<u32> = documents.iter().map(Pdf::page_count).collect();
    let mut merged = Pdf::merge(&documents)?;
    let bytes = merged.to_bytes()?;
    paths::write(&args.output, &bytes, overwrite)?;

    let total: u32 = pages_each.iter().sum();
    Ok(FileReport {
        human: format!(
            "Merged {} documents, {total} pages → {} ({})",
            files.len(),
            args.output.display(),
            ypdf_optimize::format_size(bytes.len() as u64)
        ),
        json: json!({
            "inputs": files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "pages_per_input": pages_each,
            "pages": total,
            "output": args.output.display().to_string(),
            "bytes": bytes.len(),
        }),
        bytes_in,
        bytes_out: bytes.len() as u64,
        flagged: false,
    })
}

/// `split` — one document into several (spec §3.2).
pub fn split(
    args: &SplitArgs,
    password: Option<&str>,
    overwrite: bool,
    cancel: &CancelToken,
    out: &Out,
) -> Result<FileReport> {
    let pdf = super::open_input(&args.input, password)?;
    let mode = mode_from(args)?;
    let pieces = ypdf_doc::split(&pdf, &mode)?;

    if pieces.is_empty() {
        return Err(Error::Config {
            detail: format!("{} produced no pieces", args.input.display()),
            source_path: Some(args.input.clone()),
        });
    }

    paths::ensure_dir(&args.output)?;
    let stem = paths::stem_of(&args.input);

    let mut written = Vec::with_capacity(pieces.len());
    let mut bytes_out = 0_u64;
    for (n, mut piece) in pieces.into_iter().enumerate() {
        cancel.check()?;
        let target = args.output.join(format!("{stem}-{}.pdf", piece.name));
        let bytes = piece.pdf.to_bytes()?;
        paths::write(&target, &bytes, overwrite)?;
        bytes_out += bytes.len() as u64;
        out.note(format!(
            "[{}] {} ({} pages)",
            n + 1,
            target.display(),
            piece.pages.len()
        ));
        written.push(json!({
            "path": target.display().to_string(),
            "pages": piece.pages,
            "bytes": bytes.len(),
        }));
    }

    Ok(FileReport {
        human: format!(
            "Split {} into {} files in {}",
            args.input.display(),
            written.len(),
            args.output.display()
        ),
        json: json!({
            "input": args.input.display().to_string(),
            "output_dir": args.output.display().to_string(),
            "files": written,
        }),
        bytes_in: std::fs::metadata(&args.input).map(|m| m.len()).unwrap_or(0),
        bytes_out,
        flagged: false,
    })
}

/// Which way to cut. Defaults to one file per page, which is what someone who
/// gave no flag at all almost always meant.
fn mode_from(args: &SplitArgs) -> Result<SplitMode> {
    if let Some(ranges) = &args.pages {
        let specs = ranges
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PageSpec::parse)
            .collect::<Result<Vec<_>>>()?;
        if specs.is_empty() {
            return Err(Error::InvalidPageRange {
                spec: ranges.clone(),
            });
        }
        return Ok(SplitMode::Ranges(specs));
    }
    if let Some(n) = args.every {
        if n == 0 {
            return Err(Error::Config {
                detail: "--every needs a page count above zero".into(),
                source_path: None,
            });
        }
        return Ok(SplitMode::EveryN(n));
    }
    if args.bookmarks {
        return Ok(SplitMode::Bookmarks);
    }
    Ok(SplitMode::EachPage)
}

/// `extract` — keep only the pages named (spec §3.3).
pub fn extract(
    args: &ExtractArgs,
    password: Option<&str>,
    overwrite: bool,
    _cancel: &CancelToken,
) -> Result<FileReport> {
    paths::guard(&args.output, overwrite)?;

    let mut pdf = super::open_input(&args.input, password)?;
    let pages = PageSpec::parse(&args.pages)?.resolve_unique(pdf.page_count())?;
    pdf.extract(&pages)?;

    let bytes = pdf.to_bytes()?;
    paths::write(&args.output, &bytes, overwrite)?;

    Ok(FileReport {
        human: format!(
            "Extracted {} pages → {} ({})",
            pages.len(),
            args.output.display(),
            ypdf_optimize::format_size(bytes.len() as u64)
        ),
        json: json!({
            "input": args.input.display().to_string(),
            "pages": pages,
            "output": args.output.display().to_string(),
            "bytes": bytes.len(),
        }),
        bytes_in: std::fs::metadata(&args.input).map(|m| m.len()).unwrap_or(0),
        bytes_out: bytes.len() as u64,
        flagged: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn out() -> Out {
        Out::new(&crate::cli::Global {
            json: false,
            quiet: true,
            verbose: false,
            config: None,
            overwrite: false,
            workers: None,
            password: None,
        })
    }

    #[test]
    fn no_flag_splits_into_single_pages() {
        let args = SplitArgs {
            input: fixture("two-pages.pdf"),
            output: PathBuf::new(),
            pages: None,
            every: None,
            each_page: false,
            bookmarks: false,
        };
        assert_eq!(mode_from(&args).expect("a mode"), SplitMode::EachPage);
    }

    #[test]
    fn a_range_list_becomes_one_piece_per_range() {
        let args = SplitArgs {
            input: fixture("many-pages.pdf"),
            output: PathBuf::new(),
            pages: Some("1-3,4-6".into()),
            every: None,
            each_page: false,
            bookmarks: false,
        };
        let SplitMode::Ranges(specs) = mode_from(&args).expect("a mode") else {
            panic!("expected ranges");
        };
        assert_eq!(specs.len(), 2);
    }

    #[test]
    fn splitting_writes_one_file_per_page() {
        let dir = scratch("split");
        let args = SplitArgs {
            input: fixture("two-pages.pdf"),
            output: dir.clone(),
            pages: None,
            every: None,
            each_page: true,
            bookmarks: false,
        };
        let report = split(&args, None, false, &CancelToken::never(), &out()).expect("splits");

        let files = report.json["files"].as_array().expect("an array");
        assert_eq!(files.len(), 2);
        for file in files {
            let path = PathBuf::from(file["path"].as_str().expect("a path"));
            assert_eq!(Pdf::open(&path).expect("re-opens").page_count(), 1);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extracting_keeps_only_the_pages_asked_for() {
        let dir = scratch("extract");
        let target = dir.join("out.pdf");
        let args = ExtractArgs {
            input: fixture("many-pages.pdf"),
            pages: "2-4".into(),
            output: target.clone(),
        };
        extract(&args, None, false, &CancelToken::never()).expect("extracts");

        assert_eq!(Pdf::open(&target).expect("re-opens").page_count(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extracting_refuses_to_replace_an_existing_file() {
        let dir = scratch("extract-guard");
        let target = dir.join("out.pdf");
        std::fs::write(&target, b"not a pdf").expect("writes");

        let args = ExtractArgs {
            input: fixture("many-pages.pdf"),
            pages: "1".into(),
            output: target.clone(),
        };
        assert!(extract(&args, None, false, &CancelToken::never()).is_err());
        assert_eq!(
            std::fs::read(&target).expect("still there"),
            b"not a pdf",
            "the existing file must be untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merging_two_documents_adds_their_pages() {
        let dir = scratch("merge");
        let target = dir.join("merged.pdf");
        let files = vec![fixture("two-pages.pdf"), fixture("many-pages.pdf")];
        let args = MergeArgs {
            inputs: Vec::new(),
            output: target.clone(),
        };
        let report =
            merge(&files, &args, None, false, &CancelToken::never(), &out()).expect("merges");

        assert_eq!(report.json["pages"], 14);
        assert_eq!(Pdf::open(&target).expect("re-opens").page_count(), 14);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merging_one_document_is_refused() {
        let args = MergeArgs {
            inputs: Vec::new(),
            output: PathBuf::from("out.pdf"),
        };
        let result = merge(
            &[fixture("two-pages.pdf")],
            &args,
            None,
            false,
            &CancelToken::never(),
            &out(),
        );
        assert!(result.is_err(), "one input is not a merge");
    }
}
