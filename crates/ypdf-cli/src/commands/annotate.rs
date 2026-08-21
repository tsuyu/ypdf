//! `annotate` — list, add, and remove annotations (spec §10).
//!
//! Reads by default, like every other command that can also write. Adding is
//! additive and removing is deletion, so a document can go out for review and
//! come back clean without its pages ever being rewritten.

use std::path::Path;

use serde_json::json;
use ypdf_annot::{Annotation, Colour, Shape};
use ypdf_core::{CancelToken, Error, Result};

use crate::batch::FileReport;
use crate::cli::AnnotateArgs;
use crate::paths;

/// Run over one file.
pub fn run(
    path: &Path,
    args: &AnnotateArgs,
    password: Option<&str>,
    many: bool,
    overwrite: bool,
    _cancel: &CancelToken,
) -> Result<FileReport> {
    let mut pdf = super::open_input(path, password)?;

    if !args.is_edit() {
        let found = ypdf_annot::list(&pdf);
        return Ok(listing(&found, path, 0, None));
    }

    let mut removed = 0;
    if args.remove_all {
        removed += ypdf_annot::remove_where(&mut pdf, |_| true);
    } else if let Some(author) = &args.remove_author {
        let author = author.to_lowercase();
        removed += ypdf_annot::remove_where(&mut pdf, |annotation| {
            annotation.author.to_lowercase() == author
        });
    }

    let colour = match &args.colour {
        Some(text) => Colour::parse(text)?,
        None => Colour::default(),
    };

    let mut added = 0;
    for annotation in build(args, colour)? {
        ypdf_annot::add(&mut pdf, &annotation)?;
        added += 1;
    }

    if added == 0 && removed == 0 {
        return Err(Error::Config {
            detail: "nothing was added or removed".into(),
            source_path: Some(path.to_path_buf()),
        });
    }

    let target = match &args.output {
        Some(output) => paths::output_for(output, path, many, "-annotated")?,
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

    let remaining = ypdf_annot::list(&pdf);
    Ok(listing(&remaining, path, removed, Some((&target, added))))
}

/// Turn the arguments into annotations.
fn build(args: &AnnotateArgs, colour: Colour) -> Result<Vec<Annotation>> {
    let mut out = Vec::new();

    // A function rather than a closure: the note and text-box arms below build
    // their own annotations, and a closure holding `out` would lock it.
    let dress = |page: u32, shape: Shape| -> Annotation {
        let mut annotation = Annotation::new(page, shape)
            .with_colour(colour)
            .with_opacity(args.opacity)
            .with_width(args.width);
        if let Some(comment) = &args.comment {
            annotation = annotation.with_contents(comment.clone());
        }
        if let Some(author) = &args.author {
            annotation = annotation.with_author(author.clone());
        }
        annotation
    };

    for entry in &args.highlight {
        let (page, rect) = area(entry)?;
        out.push(dress(page, Shape::Highlight { quads: vec![rect] }));
    }
    for entry in &args.underline {
        let (page, rect) = area(entry)?;
        out.push(dress(page, Shape::Underline { quads: vec![rect] }));
    }
    for entry in &args.strike {
        let (page, rect) = area(entry)?;
        out.push(dress(page, Shape::StrikeOut { quads: vec![rect] }));
    }
    for entry in &args.rect {
        let (page, rect) = area(entry)?;
        out.push(dress(page, Shape::Rectangle { rect, fill: None }));
    }
    for entry in &args.ellipse {
        let (page, rect) = area(entry)?;
        out.push(dress(page, Shape::Ellipse { rect, fill: None }));
    }
    for entry in &args.line {
        let (page, rect) = area(entry)?;
        out.push(dress(
            page,
            Shape::Line {
                from: [rect[0], rect[1]],
                to: [rect[2], rect[3]],
                arrow: args.arrow,
            },
        ));
    }
    for entry in &args.note {
        let (page, point, text) = point_and_text(entry)?;
        // The note's own text wins over `--comment`: it was typed for this one.
        out.push(dress(page, Shape::Note { at: point }).with_contents(text));
    }
    for entry in &args.text_box {
        let (page, rect, text) = area_and_text(entry)?;
        out.push(dress(page, Shape::TextBox { rect, size: None }).with_contents(text));
    }
    for entry in &args.stamp {
        let (page, rect, text) = area_and_text(entry)?;
        out.push(dress(page, Shape::Stamp { rect, text }));
    }

    Ok(out)
}

fn listing(
    found: &[Annotation],
    path: &Path,
    removed: usize,
    written: Option<(&Path, usize)>,
) -> FileReport {
    let mut human = if found.is_empty() {
        "No annotations.".to_string()
    } else {
        found
            .iter()
            .map(Annotation::describe)
            .collect::<Vec<_>>()
            .join("\n")
    };

    if removed > 0 {
        human.push_str(&format!("\n\n{removed} removed."));
    }
    if let Some((target, added)) = written {
        if added > 0 {
            human.push_str(&format!("\n{added} added."));
        }
        human.push_str(&format!("\nWritten to {}", target.display()));
    }

    FileReport::text(
        human,
        json!({
            "input": path.display().to_string(),
            "count": found.len(),
            "removed": removed,
            "added": written.map_or(0, |(_, added)| added),
            "annotations": found,
            "written_to": written.map(|(target, _)| target.display().to_string()),
        }),
    )
}

/// Parse `page:x0,y0,x1,y1`.
fn area(entry: &str) -> Result<(u32, [f32; 4])> {
    let bad = || Error::Config {
        detail: format!(
            "{entry:?} is not an area; expected page:x0,y0,x1,y1, e.g. 1:72,700,300,714"
        ),
        source_path: None,
    };

    let (page, rest) = entry.split_once(':').ok_or_else(bad)?;
    let page: u32 = page.trim().parse().map_err(|_| bad())?;
    let numbers: Vec<f32> = rest
        .split(',')
        .map(|value| value.trim().parse::<f32>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| bad())?;

    match numbers[..] {
        [x0, y0, x1, y1] => Ok((page, [x0, y0, x1, y1])),
        _ => Err(bad()),
    }
}

/// Parse `page:x0,y0,x1,y1=text`.
fn area_and_text(entry: &str) -> Result<(u32, [f32; 4], String)> {
    let (area_part, text) = entry.split_once('=').ok_or_else(|| Error::Config {
        detail: format!("{entry:?} needs text after an `=`, e.g. 1:100,100,260,150=APPROVED"),
        source_path: None,
    })?;
    let (page, mut rect) = area(area_part)?;
    // Normalized, since someone dragging a box does not always start top-left.
    rect = [
        rect[0].min(rect[2]),
        rect[1].min(rect[3]),
        rect[0].max(rect[2]),
        rect[1].max(rect[3]),
    ];
    Ok((page, rect, text.to_string()))
}

/// Parse `page:x,y=text`.
fn point_and_text(entry: &str) -> Result<(u32, [f32; 2], String)> {
    let bad = || Error::Config {
        detail: format!("{entry:?} is not a note; expected page:x,y=text, e.g. 2:100,700=see this"),
        source_path: None,
    };

    let (position, text) = entry.split_once('=').ok_or_else(bad)?;
    let (page, rest) = position.split_once(':').ok_or_else(bad)?;
    let page: u32 = page.trim().parse().map_err(|_| bad())?;
    let numbers: Vec<f32> = rest
        .split(',')
        .map(|value| value.trim().parse::<f32>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| bad())?;

    match numbers[..] {
        [x, y] => Ok((page, [x, y], text.to_string())),
        _ => Err(bad()),
    }
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

    fn args() -> AnnotateArgs {
        AnnotateArgs {
            inputs: Vec::new(),
            output: None,
            highlight: Vec::new(),
            underline: Vec::new(),
            strike: Vec::new(),
            note: Vec::new(),
            text_box: Vec::new(),
            rect: Vec::new(),
            ellipse: Vec::new(),
            line: Vec::new(),
            stamp: Vec::new(),
            arrow: false,
            comment: None,
            author: None,
            colour: None,
            opacity: 1.0,
            width: 1.5,
            remove_all: false,
            remove_author: None,
        }
    }

    #[test]
    fn an_area_is_read_as_a_page_and_four_numbers() {
        let (page, rect) = area("1:72,700,300,714").expect("parses");
        assert_eq!(page, 1);
        assert_eq!(rect, [72.0, 700.0, 300.0, 714.0]);
    }

    #[test]
    fn a_malformed_area_says_what_one_looks_like() {
        for entry in ["nonsense", "1:1,2,3", "x:1,2,3,4"] {
            let error = area(entry).expect_err("refused");
            assert!(
                error.report().to_human().contains("1:72,700,300,714"),
                "{entry}"
            );
        }
    }

    #[test]
    fn a_note_carries_its_text() {
        let (page, at, text) = point_and_text("2:100,700=look at this").expect("parses");
        assert_eq!(page, 2);
        assert_eq!(at, [100.0, 700.0]);
        assert_eq!(text, "look at this");
    }

    #[test]
    fn a_stamp_area_is_normalized_whichever_way_it_was_given() {
        let (_, rect, text) = area_and_text("1:260,150,100,100=APPROVED").expect("parses");
        assert_eq!(rect, [100.0, 100.0, 260.0, 150.0]);
        assert_eq!(text, "APPROVED");
    }

    #[test]
    fn listing_does_not_touch_the_file() {
        let path = fixture("many-pages.pdf");
        let before = std::fs::read(&path).expect("reads");

        let report = run(&path, &args(), None, false, false, &CancelToken::never()).expect("lists");

        assert_eq!(report.json["count"], 0);
        assert_eq!(std::fs::read(&path).expect("reads"), before);
    }

    #[test]
    fn adding_writes_the_annotations_and_lists_them_back() {
        let dir = scratch("annotate-add");
        let mut args = args();
        args.highlight = vec!["1:72,700,300,714".into()];
        args.note = vec!["2:100,700=check this".into()];
        args.comment = Some("from review".into());
        args.author = Some("Ada".into());
        args.output = Some(dir.join("out.pdf"));

        let report = run(
            &fixture("many-pages.pdf"),
            &args,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("adds");

        assert_eq!(report.json["added"], 2);
        assert_eq!(report.json["count"], 2);

        let pdf = ypdf_doc::Pdf::open(dir.join("out.pdf")).expect("re-opens");
        let found = ypdf_annot::list(&pdf);
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|a| a.author == "Ada"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_reviewers_comments_can_be_cleared_and_the_others_kept() {
        let dir = scratch("annotate-remove");

        let mut first = args();
        first.note = vec!["1:100,700=mine".into()];
        first.author = Some("Ada".into());
        first.output = Some(dir.join("one.pdf"));
        run(
            &fixture("many-pages.pdf"),
            &first,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("adds");

        let mut second = args();
        second.note = vec!["1:140,700=also mine".into()];
        second.author = Some("Grace".into());
        second.output = Some(dir.join("two.pdf"));
        run(
            &dir.join("one.pdf"),
            &second,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("adds");

        let mut third = args();
        third.remove_author = Some("ada".into());
        third.output = Some(dir.join("three.pdf"));
        let report = run(
            &dir.join("two.pdf"),
            &third,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("removes");

        assert_eq!(report.json["removed"], 1);
        assert_eq!(report.json["count"], 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_edit_that_would_do_nothing_is_refused() {
        // Writing a copy identical to the input and calling it done sends
        // someone looking for the problem in the wrong place.
        let dir = scratch("annotate-nothing");
        let mut args = args();
        args.remove_author = Some("nobody".into());
        args.output = Some(dir.join("out.pdf"));

        let result = run(
            &fixture("many-pages.pdf"),
            &args,
            None,
            false,
            false,
            &CancelToken::never(),
        );
        assert!(result.is_err());
        assert!(!dir.join("out.pdf").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
