//! `redact` — remove content, by rectangle or by search (spec §11).
//!
//! Like `ocr`, this runs its own sequential loop rather than the parallel batch
//! runner: finding text on a page means rasterizing nothing but asking PDFium
//! where the characters are, and PDFium lives on one thread by design.
//!
//! One rule specific to this command: **a search that matches nothing is an
//! error.** Someone who asks to redact an account number and is told "done"
//! will send the file. If the pattern was wrong, silence is the most dangerous
//! possible answer.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use ypdf_core::{CancelToken, Error, Result};
use ypdf_doc::PageSpec;
use ypdf_redact::{Rect, Redaction, Settings};
use ypdf_render::{RenderEvent, RenderHandle, SearchOptions};

use crate::batch::FileReport;
use crate::cli::RedactArgs;
use crate::output::Out;
use crate::paths;

/// How long to wait for one page's text.
const PAGE_TIMEOUT: Duration = Duration::from_secs(60);

/// Run over every input, in order.
pub fn run(
    files: &[PathBuf],
    args: &RedactArgs,
    password: Option<&str>,
    overwrite: bool,
    cancel: &CancelToken,
    out: &Out,
) -> Result<Vec<(PathBuf, Result<FileReport>)>> {
    if args.find.is_empty() && args.rect.is_empty() {
        return Err(Error::Config {
            detail: "give --find or --rect; redacting nothing is not a redaction".into(),
            source_path: None,
        });
    }

    // Parsed before anything is opened: a malformed rectangle is the same
    // answer for every file.
    let explicit = parse_rects(&args.rect)?;

    let render = if args.find.is_empty() {
        None
    } else {
        Some(RenderHandle::spawn()?)
    };

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
            render.as_ref(),
            path,
            args,
            &explicit,
            password,
            many,
            overwrite,
            cancel,
            out,
        );
        results.push((path.clone(), result));
    }

    Ok(results)
}

#[expect(clippy::too_many_arguments, reason = "one call site, all of it needed")]
fn one_file(
    render: Option<&RenderHandle>,
    path: &Path,
    args: &RedactArgs,
    explicit: &[Redaction],
    password: Option<&str>,
    many: bool,
    overwrite: bool,
    cancel: &CancelToken,
    out: &Out,
) -> Result<FileReport> {
    let target = paths::output_for(&args.output, path, many, "-redacted")?;
    paths::guard(&target, overwrite)?;

    let mut pdf = super::open_input(path, password)?;
    let count = pdf.page_count();
    let pages: Vec<u32> = match &args.pages {
        Some(spec) => PageSpec::parse(spec)?.resolve_unique(count)?,
        None => (1..=count).collect(),
    };

    let mut redactions: Vec<Redaction> = explicit
        .iter()
        .filter(|redaction| pages.contains(&redaction.page))
        .cloned()
        .collect();

    let mut matches = 0;
    if let Some(render) = render {
        let found = search(render, path, password, &pages, args, cancel, out)?;
        matches = found.len();
        redactions.extend(found);
    }

    // Nothing to do is not success. It is the case where someone believes a
    // secret was removed and it was not.
    if redactions.is_empty() {
        return Err(Error::Config {
            detail: format!(
                "nothing matched in {}. The document was not changed.",
                path.display()
            ),
            source_path: Some(path.to_path_buf()),
        });
    }

    let settings = Settings {
        draw_cover: !args.no_cover,
        ..Settings::default()
    };
    let report = ypdf_redact::redact(&mut pdf, &redactions, &settings)?;

    let bytes = pdf.to_bytes()?;
    paths::write(&target, &bytes, overwrite)?;

    let mut human = report.to_human();
    human.push_str(&format!("\nWritten to {}\n", target.display()));

    Ok(FileReport {
        human,
        json: json!({
            "input": path.display().to_string(),
            "output": target.display().to_string(),
            "matches": matches,
            "areas": redactions.len(),
            "pages": report.pages,
            "glyphs_removed": report.glyphs,
            "coarse_runs": report.coarse_runs,
            "images_redacted": report.images_redacted,
            "images_removed": report.images_removed,
            "paths_removed": report.paths,
            "annotations_removed": report.annotations,
            "bytes": bytes.len(),
        }),
        bytes_in: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        bytes_out: bytes.len() as u64,
        flagged: false,
    })
}

/// Find every occurrence of every `--find` term, and turn them into areas.
fn search(
    render: &RenderHandle,
    path: &Path,
    password: Option<&str>,
    pages: &[u32],
    args: &RedactArgs,
    cancel: &CancelToken,
    out: &Out,
) -> Result<Vec<Redaction>> {
    let doc = render.open(path.to_path_buf(), password.map(ToString::to_string));
    wait_for_open(render, cancel)?;

    let options = SearchOptions {
        case_sensitive: args.case_sensitive,
        whole_word: args.whole_words,
    };

    let mut found = Vec::new();
    let mut per_term = vec![0_usize; args.find.len()];

    for page in pages {
        cancel.check()?;
        let Ok(index) = i32::try_from(page - 1) else {
            continue;
        };

        render.analyze(doc, index);
        let analysis = wait_for_analysis(render, index, cancel)?;

        for (term_index, term) in args.find.iter().enumerate() {
            for found_match in analysis.text.search(term, options) {
                for rect in analysis.text.rects_for(found_match.start, found_match.len) {
                    found.push(Redaction {
                        page: *page,
                        rect: Rect::new(rect.left, rect.bottom, rect.right, rect.top),
                    });
                }
                per_term[term_index] += 1;
            }
        }
    }

    render.close(doc);

    // A term that matched nothing is reported by name. "Nothing was found" is
    // useless when four patterns were given and one of them was a typo.
    let missing: Vec<&String> = args
        .find
        .iter()
        .zip(&per_term)
        .filter(|(_, count)| **count == 0)
        .map(|(term, _)| term)
        .collect();

    if !missing.is_empty() && !args.allow_no_matches {
        return Err(Error::Config {
            detail: format!(
                "no match for: {}. Nothing was written. Pass --allow-no-matches if that is expected.",
                missing
                    .iter()
                    .map(|term| format!("{term:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            source_path: Some(path.to_path_buf()),
        });
    }
    for term in missing {
        out.status(format!("no match for {term:?}"));
    }

    Ok(found)
}

/// Parse `page:x0,y0,x1,y1`.
fn parse_rects(specs: &[String]) -> Result<Vec<Redaction>> {
    specs
        .iter()
        .map(|spec| {
            let bad = || Error::Config {
                detail: format!(
                    "{spec:?} is not a redaction area; expected page:x0,y0,x1,y1 in points, \
                     e.g. 1:100,650,210,662"
                ),
                source_path: None,
            };

            let (page, rest) = spec.split_once(':').ok_or_else(bad)?;
            let page: u32 = page.trim().parse().map_err(|_| bad())?;
            let numbers: Vec<f32> = rest
                .split(',')
                .map(|value| value.trim().parse::<f32>())
                .collect::<std::result::Result<_, _>>()
                .map_err(|_| bad())?;

            match numbers[..] {
                [x0, y0, x1, y1] => Ok(Redaction {
                    page,
                    rect: Rect::new(x0, y0, x1, y1),
                }),
                _ => Err(bad()),
            }
        })
        .collect()
}

fn wait_for_open(render: &RenderHandle, cancel: &CancelToken) -> Result<()> {
    let deadline = std::time::Instant::now() + PAGE_TIMEOUT;
    while std::time::Instant::now() < deadline {
        cancel.check()?;
        match render.recv_event() {
            Some(RenderEvent::Opened { .. }) => return Ok(()),
            Some(RenderEvent::Failed { error, .. }) => return Err(error),
            _ => {}
        }
    }
    Err(Error::Backend {
        backend: "pdfium",
        detail: "timed out opening the document".into(),
    })
}

fn wait_for_analysis(
    render: &RenderHandle,
    page: i32,
    cancel: &CancelToken,
) -> Result<ypdf_render::PageAnalysis> {
    let deadline = std::time::Instant::now() + PAGE_TIMEOUT;
    while std::time::Instant::now() < deadline {
        cancel.check()?;
        match render.recv_event() {
            Some(RenderEvent::Analyzed { analysis, .. }) if analysis.page == page => {
                return Ok(*analysis);
            }
            Some(RenderEvent::Failed { error, .. }) => return Err(error),
            _ => {}
        }
    }
    Err(Error::Backend {
        backend: "pdfium",
        detail: format!("timed out reading page {}", page + 1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_area_is_read_as_page_and_four_points() {
        let parsed = parse_rects(&["2:100,650,210,662".to_string()]).expect("parses");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].page, 2);
        assert_eq!(parsed[0].rect, Rect::new(100.0, 650.0, 210.0, 662.0));
    }

    #[test]
    fn a_malformed_area_says_what_one_looks_like() {
        for spec in ["nonsense", "1:1,2,3", "x:1,2,3,4", "1;1,2,3,4"] {
            let error = parse_rects(&[spec.to_string()]).expect_err("refused");
            assert!(
                error.report().to_human().contains("1:100,650,210,662"),
                "{spec}"
            );
        }
    }

    #[test]
    fn several_areas_are_all_read() {
        let parsed =
            parse_rects(&["1:0,0,10,10".to_string(), "3:20,20,30,30".to_string()]).expect("parses");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].page, 3);
    }
}
