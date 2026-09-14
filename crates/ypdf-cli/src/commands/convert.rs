//! `markdown` and `docx` — a document in another format (spec §5).
//!
//! Like `ocr`, these are not parallel batch commands: the text layer comes
//! from PDFium, which lives on one dedicated thread, so files are handled in
//! order with one render thread for the whole run.
//!
//! Both formats are writers over the same rebuilt structure, so the commands
//! differ only in what they do with it at the end. Everything the conversion
//! could not work out comes back as warnings, which are printed rather than
//! swallowed: spec §5 asks for the quality to be reported rather than for the
//! layout to be promised.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use ypdf_convert::Warning;
use ypdf_core::{CancelToken, Error, Result};
use ypdf_doc::PageSpec;
use ypdf_render::{PageText, RenderEvent, RenderHandle};

use crate::batch::FileReport;
use crate::cli::{DocxArgs, MarkdownArgs};
use crate::output::Out;
use crate::paths;

/// How long to wait for one page's text layer.
///
/// Extraction is far cheaper than rasterizing, so this only has to be long
/// enough that a huge page does not trip it.
const PAGE_TIMEOUT: Duration = Duration::from_secs(60);

/// Convert every input to Markdown, in order.
pub fn markdown(
    files: &[PathBuf],
    args: &MarkdownArgs,
    password: Option<&str>,
    overwrite: bool,
    cancel: &CancelToken,
    out: &Out,
) -> Result<Vec<(PathBuf, Result<FileReport>)>> {
    each(files, cancel, out, |render, path, many| {
        markdown_file(render, path, args, password, many, overwrite, cancel, out)
    })
}

/// Convert every input to a Word document, in order.
pub fn docx(
    files: &[PathBuf],
    args: &DocxArgs,
    password: Option<&str>,
    overwrite: bool,
    cancel: &CancelToken,
    out: &Out,
) -> Result<Vec<(PathBuf, Result<FileReport>)>> {
    each(files, cancel, out, |render, path, many| {
        docx_file(render, path, args, password, many, overwrite, cancel, out)
    })
}

/// Walk the inputs in order, on one render thread, reporting progress.
fn each(
    files: &[PathBuf],
    cancel: &CancelToken,
    out: &Out,
    mut convert: impl FnMut(&RenderHandle, &Path, bool) -> Result<FileReport>,
) -> Result<Vec<(PathBuf, Result<FileReport>)>> {
    let render = RenderHandle::spawn()?;
    let many = files.len() > 1;

    let mut results = Vec::with_capacity(files.len());
    for (n, path) in files.iter().enumerate() {
        if cancel.is_cancelled() {
            results.push((path.clone(), Err(Error::Cancelled)));
            continue;
        }
        if many {
            out.status(format!("[{}/{}] {}", n + 1, files.len(), path.display()));
        }
        results.push((path.clone(), convert(&render, path, many)));
    }

    Ok(results)
}

#[expect(clippy::too_many_arguments, reason = "one call site, all of it needed")]
fn markdown_file(
    render: &RenderHandle,
    path: &Path,
    args: &MarkdownArgs,
    password: Option<&str>,
    many: bool,
    overwrite: bool,
    cancel: &CancelToken,
    out: &Out,
) -> Result<FileReport> {
    let pages = read_pages(render, path, args.pages.as_ref(), password, cancel)?;
    let converted = ypdf_convert::to_markdown(&pages);
    report_warnings(out, path, &converted.warnings, many);

    let bytes = converted.markdown.as_bytes();
    let Some(destination) = &args.output else {
        // No destination: the Markdown itself is the output, which is what
        // makes this useful at the end of a pipe.
        out.human(&converted.markdown);
        return Ok(report(
            path,
            &converted.warnings,
            pages.len(),
            bytes.len(),
            None,
        ));
    };

    let target = paths::output_for(destination, path, many, ".md")?;
    paths::write(&target, bytes, overwrite)?;
    Ok(report(
        path,
        &converted.warnings,
        pages.len(),
        bytes.len(),
        Some(&target),
    ))
}

#[expect(clippy::too_many_arguments, reason = "one call site, all of it needed")]
fn docx_file(
    render: &RenderHandle,
    path: &Path,
    args: &DocxArgs,
    password: Option<&str>,
    many: bool,
    overwrite: bool,
    cancel: &CancelToken,
    out: &Out,
) -> Result<FileReport> {
    let pages = read_pages(render, path, args.pages.as_ref(), password, cancel)?;
    let converted = ypdf_convert::to_docx(&pages);
    report_warnings(out, path, &converted.warnings, many);

    // No stdout fallback here, unlike Markdown: a `.docx` is a ZIP, and
    // spraying one down a terminal helps nobody.
    let target = paths::output_for(&args.output, path, many, ".docx")?;
    paths::write(&target, &converted.bytes, overwrite)?;
    Ok(report(
        path,
        &converted.warnings,
        pages.len(),
        converted.bytes.len(),
        Some(&target),
    ))
}

/// Read the text layer of every page asked for.
fn read_pages(
    render: &RenderHandle,
    path: &Path,
    pages: Option<&String>,
    password: Option<&str>,
    cancel: &CancelToken,
) -> Result<Vec<PageText>> {
    let doc = render.open(path.to_path_buf(), password.map(ToString::to_string));
    let info = wait_for_open(render)?;

    let total = u32::try_from(info.page_count).unwrap_or(0);
    let wanted: Vec<u32> = match pages {
        Some(spec) => PageSpec::parse(spec)?.resolve(total)?,
        None => (1..=total).collect(),
    };

    let mut out = Vec::with_capacity(wanted.len());
    for number in &wanted {
        // Checked per page, not just per file: a thousand-page document must
        // still answer to Ctrl-C partway through.
        cancel.check()?;
        let Ok(index) = i32::try_from(number.saturating_sub(1)) else {
            continue;
        };
        render.analyze(doc, index);
        out.push(wait_for_text(render, index)?);
    }
    render.close(doc);

    Ok(out)
}

/// Print what the conversion could not work out.
fn report_warnings(out: &Out, path: &Path, warnings: &[Warning], many: bool) {
    for warning in warnings {
        let message = warning.message();
        if many {
            out.note(format!("{}: {message}", path.display()));
        } else {
            out.note(message);
        }
    }
}

fn report(
    path: &Path,
    warnings: &[Warning],
    pages: usize,
    bytes_out: usize,
    target: Option<&Path>,
) -> FileReport {
    let messages: Vec<String> = warnings.iter().map(Warning::message).collect();
    let human = match target {
        Some(target) => format!(
            "{pages} page(s) → {} ({} warning(s))",
            target.display(),
            messages.len()
        ),
        None => format!("{pages} page(s) converted ({} warning(s))", messages.len()),
    };

    FileReport {
        human,
        json: json!({
            "input": path.display().to_string(),
            "output": target.map(|t| t.display().to_string()),
            "pages": pages,
            "bytes": bytes_out,
            "warnings": messages,
        }),
        bytes_in: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        bytes_out: bytes_out as u64,
        // Anything the converter could not work out is worth a caller's
        // attention, which is exactly what `flagged` means.
        flagged: !warnings.is_empty(),
    }
}

fn wait_for_open(render: &RenderHandle) -> Result<ypdf_render::DocumentInfo> {
    loop {
        match render.recv_event() {
            Some(RenderEvent::Opened { info, .. }) => return Ok(*info),
            Some(RenderEvent::Failed { error, .. }) => return Err(error),
            Some(_) => {}
            None => {
                return Err(Error::Config {
                    detail: "the render thread stopped before the document opened".into(),
                    source_path: None,
                });
            }
        }
    }
}

fn wait_for_text(render: &RenderHandle, page: i32) -> Result<PageText> {
    let deadline = std::time::Instant::now() + PAGE_TIMEOUT;
    loop {
        if std::time::Instant::now() > deadline {
            return Err(Error::Config {
                detail: format!("page {} timed out while reading its text", page + 1),
                source_path: None,
            });
        }
        match render.recv_event() {
            Some(RenderEvent::Analyzed { analysis, .. }) if analysis.page == page => {
                return Ok(analysis.text);
            }
            Some(RenderEvent::Failed { error, .. }) => return Err(error),
            Some(_) => {}
            None => {
                return Err(Error::Config {
                    detail: "the render thread stopped before the text arrived".into(),
                    source_path: None,
                });
            }
        }
    }
}
