//! `ocr` — make a scan searchable (spec §7).
//!
//! Not a parallel batch command, unlike everything else that takes several
//! files. PDFium lives on one dedicated thread by design, so pages are
//! rasterized one at a time; the files are still processed in order, with one
//! render thread for the whole run rather than one per file.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use ypdf_core::{CancelToken, Error, Result};
use ypdf_doc::PageSpec;
use ypdf_ocr::{OcrSettings, Tesseract};
use ypdf_render::{Priority, QuarterTurns, RenderEvent, RenderHandle, RenderRequest};

use crate::batch::FileReport;
use crate::cli::OcrArgs;
use crate::output::Out;
use crate::paths;

/// How long to wait for one page to rasterize before giving up.
///
/// Generous: a 600 dpi A0 page is a real thing. The point is to fail with a
/// message rather than hang a batch overnight.
const PAGE_TIMEOUT: Duration = Duration::from_secs(120);

/// Run OCR over every input, in order.
pub fn run(
    files: &[PathBuf],
    args: &OcrArgs,
    password: Option<&str>,
    overwrite: bool,
    cancel: &CancelToken,
    out: &Out,
) -> Result<Vec<(PathBuf, Result<FileReport>)>> {
    // Find the engine once, before any work: "Tesseract is not installed" is
    // the same answer for every file, and finding it out on file 900 of 1,000
    // helps nobody.
    let engine = match &args.tesseract {
        Some(path) => Tesseract::at(path),
        None => Tesseract::find()?,
    };
    engine.check_languages(&args.lang)?;
    out.note(format!(
        "using {} ({})",
        engine.path().display(),
        engine.version().unwrap_or_else(|_| "unknown".into())
    ));

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
        let result = one_file(
            &engine, &render, path, args, password, many, overwrite, cancel, out,
        );
        results.push((path.clone(), result));
    }

    Ok(results)
}

#[expect(clippy::too_many_arguments, reason = "one call site, all of it needed")]
fn one_file(
    engine: &Tesseract,
    render: &RenderHandle,
    path: &Path,
    args: &OcrArgs,
    password: Option<&str>,
    many: bool,
    overwrite: bool,
    cancel: &CancelToken,
    out: &Out,
) -> Result<FileReport> {
    let target = paths::output_for(&args.output, path, many, "-ocr")?;
    paths::guard(&target, overwrite)?;

    let mut pdf = super::open_input(path, password)?;
    let pages = match &args.pages {
        Some(spec) => PageSpec::parse(spec)?.resolve_unique(pdf.page_count())?,
        None => (1..=pdf.page_count()).collect(),
    };

    let doc = render.open(path.to_path_buf(), password.map(ToString::to_string));
    let info = wait_for_open(render, cancel)?;

    let settings = OcrSettings {
        language: args.lang.clone(),
        min_confidence: args.min_confidence,
        skip_pages_with_text: !args.force,
    };

    let mut words = 0;
    let mut unencodable = 0;
    let mut recognized = 0;
    let mut skipped = 0;
    let mut confidence_total = 0.0_f32;

    for page in &pages {
        cancel.check()?;
        let index = page - 1;
        // The renderer counts pages with a signed index; the document layer
        // counts them from one. Converting once, here, keeps the mismatch from
        // spreading.
        let Ok(render_index) = i32::try_from(index) else {
            continue;
        };

        let Some((width_pt, _)) = info.page_sizes_pt.get(index as usize).copied() else {
            continue;
        };
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped below"
        )]
        let target_width = ((width_pt / 72.0) * args.dpi as f32).round().max(1.0) as u32;

        render.request(RenderRequest {
            doc,
            page: render_index,
            target_width: target_width.min(20_000),
            rotation: QuarterTurns(0),
            priority: Priority::Visible,
            generation: 0,
        });
        let raster = wait_for_page(render, render_index, cancel)?;

        let report = ypdf_ocr::ocr_page(
            engine,
            &mut pdf,
            index,
            &raster.rgba,
            (raster.width, raster.height),
            &settings,
        )?;

        if report.skipped_has_text {
            skipped += 1;
            out.note(format!("  page {page}: already has text"));
            continue;
        }

        recognized += 1;
        words += report.words;
        unencodable += report.unencodable;
        confidence_total += report.confidence;
        out.note(format!(
            "  page {page}: {} words, {:.0}% confidence",
            report.words, report.confidence
        ));
    }

    render.close(doc);

    let bytes = pdf.to_bytes()?;
    paths::write(&target, &bytes, overwrite)?;

    #[expect(clippy::cast_precision_loss, reason = "page counts are small")]
    let mean_confidence = if recognized > 0 {
        confidence_total / recognized as f32
    } else {
        0.0
    };

    let mut human = format!(
        "{words} words over {recognized} page(s) → {} ({})",
        target.display(),
        ypdf_optimize::format_size(bytes.len() as u64)
    );
    if skipped > 0 {
        human.push_str(&format!(
            "\n{skipped} page(s) already had text and were left alone."
        ));
    }
    if unencodable > 0 {
        // Saying nothing here would leave someone believing the whole page is
        // searchable when part of it is not.
        human.push_str(&format!(
            "\n{unencodable} word(s) could not be written: the text layer font covers Latin \
             scripts only."
        ));
    }
    if recognized > 0 && words == 0 {
        human.push_str("\nNothing was recognized. Check --lang and the scan quality.");
    }

    Ok(FileReport {
        human,
        json: json!({
            "input": path.display().to_string(),
            "output": target.display().to_string(),
            "language": args.lang,
            "dpi": args.dpi,
            "pages_recognized": recognized,
            "pages_skipped_with_text": skipped,
            "words": words,
            "words_unencodable": unencodable,
            "mean_confidence": mean_confidence,
            "bytes": bytes.len(),
        }),
        bytes_in: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        bytes_out: bytes.len() as u64,
        flagged: false,
    })
}

/// Wait for the document to open.
fn wait_for_open(render: &RenderHandle, cancel: &CancelToken) -> Result<ypdf_render::DocumentInfo> {
    let deadline = std::time::Instant::now() + PAGE_TIMEOUT;
    while std::time::Instant::now() < deadline {
        cancel.check()?;
        match render.recv_event() {
            Some(RenderEvent::Opened { info, .. }) => return Ok(*info),
            Some(RenderEvent::Failed { error, .. }) => return Err(error),
            _ => {}
        }
    }
    Err(Error::Backend {
        backend: "pdfium",
        detail: "timed out opening the document".into(),
    })
}

/// Wait for one page's raster.
fn wait_for_page(
    render: &RenderHandle,
    page: i32,
    cancel: &CancelToken,
) -> Result<ypdf_render::RenderedPage> {
    let deadline = std::time::Instant::now() + PAGE_TIMEOUT;
    while std::time::Instant::now() < deadline {
        cancel.check()?;
        match render.recv_event() {
            Some(RenderEvent::Page(rendered)) if rendered.page == page => return Ok(*rendered),
            Some(RenderEvent::Failed { error, .. }) => return Err(error),
            _ => {}
        }
    }
    Err(Error::Backend {
        backend: "pdfium",
        detail: format!("timed out rendering page {}", page + 1),
    })
}
