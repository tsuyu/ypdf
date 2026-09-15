//! Commands that rearrange pages (spec §3).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use ypdf_core::{CancelToken, Error, Result};
use ypdf_doc::{PageSpec, Pdf, SplitMode};

use crate::batch::FileReport;
use crate::cli::{ExtractArgs, MergeArgs, PagesArgs, SplitArgs};
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

/// One rearrangement of a document's pages.
///
/// A run does exactly one of these. Combining them would mean deciding whether
/// `--delete 2 --rotate 90 --pages 3` counts page 3 before or after the
/// deletion, and any answer to that is a trap; two commands in a row say what
/// they mean.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Turn pages clockwise by a number of quarter turns.
    Rotate(i32),
    /// Remove pages.
    Delete,
    /// Repeat pages, so many copies of each.
    Duplicate(u32),
    /// Put the document in this order, a permutation of every page. Held as
    /// written, because resolving `3-` needs a page count and that means
    /// opening the file, which happens later.
    Reorder(String),
    /// Move one page to another position, both 1-based.
    Move(u32, u32),
    /// Reverse the document.
    Reverse,
}

impl Action {
    /// Does this operation act on a chosen set of pages?
    ///
    /// The others are about the document as a whole, and `--pages` alongside
    /// one of them means the person expected something that will not happen.
    const fn takes_pages(&self) -> bool {
        matches!(self, Self::Rotate(_) | Self::Delete | Self::Duplicate(_))
    }
}

/// `pages` — rearrange the pages of one document (spec §3).
pub fn edit(
    path: &Path,
    args: &PagesArgs,
    password: Option<&str>,
    many: bool,
    overwrite: bool,
) -> Result<FileReport> {
    let action = action_of(args)?;
    let target = paths::output_for(&args.output, path, many, "-pages")?;
    // Refuse before the work, not after it.
    paths::guard(&target, overwrite)?;

    let mut pdf = super::open_input(path, password)?;
    let before = pdf.page_count();
    let pages = match &args.pages {
        Some(spec) => PageSpec::parse(spec)?.resolve_unique(before)?,
        None => (1..=before).collect(),
    };

    match &action {
        Action::Rotate(turns) => pdf.rotate(&pages, *turns)?,
        Action::Delete => pdf.delete(&pages)?,
        Action::Duplicate(copies) => {
            // Built as one order over the original numbering rather than by
            // repeating `duplicate`: a second pass would count positions in the
            // document the first pass just changed, and copy the copies.
            let mut order = Vec::with_capacity(pages.len() * (*copies as usize) + before as usize);
            for n in 1..=before {
                order.push(n);
                if pages.binary_search(&n).is_ok() {
                    order.extend(std::iter::repeat_n(n, *copies as usize));
                }
            }
            pdf.extract(&order)?;
        }
        Action::Reorder(spec) => {
            let order = PageSpec::parse(spec)?.resolve(before)?;
            pdf.reorder(&order)?;
        }
        Action::Move(from, to) => pdf.move_page(*from, *to)?,
        Action::Reverse => pdf.reverse(),
    }

    let after = pdf.page_count();
    let bytes = pdf.to_bytes()?;
    paths::write(&target, &bytes, overwrite)?;

    Ok(FileReport {
        human: format!(
            "{} → {} ({after} pages, {})",
            describe(&action, &pages, args),
            target.display(),
            ypdf_optimize::format_size(bytes.len() as u64)
        ),
        json: json!({
            "input": path.display().to_string(),
            "output": target.display().to_string(),
            "action": name_of(&action),
            "pages": if action.takes_pages() { json!(pages) } else { Value::Null },
            "pages_before": before,
            "pages_after": after,
            "bytes": bytes.len(),
        }),
        bytes_in: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        bytes_out: bytes.len() as u64,
        flagged: false,
    })
}

/// Work out which single operation was asked for.
pub fn action_of(args: &PagesArgs) -> Result<Action> {
    let mut chosen: Vec<Action> = Vec::new();

    if let Some(degrees) = args.rotate {
        if degrees % 90 != 0 {
            return Err(Error::Config {
                detail: format!("--rotate is in quarter turns: {degrees} is not a multiple of 90"),
                source_path: None,
            });
        }
        chosen.push(Action::Rotate(degrees / 90));
    }
    if args.delete {
        chosen.push(Action::Delete);
    }
    if args.duplicate {
        if args.copies == 0 {
            return Err(Error::Config {
                detail: "--copies needs a value above zero".into(),
                source_path: None,
            });
        }
        chosen.push(Action::Duplicate(args.copies));
    }
    if let Some(order) = &args.order {
        // Parsed here so a nonsense order fails before any file is opened; the
        // page numbers themselves are checked against the document later.
        PageSpec::parse(order)?;
        chosen.push(Action::Reorder(order.clone()));
    }
    if let Some(spec) = &args.move_page {
        let (from, to) = parse_move(spec)?;
        chosen.push(Action::Move(from, to));
    }
    if args.reverse {
        chosen.push(Action::Reverse);
    }

    let mut chosen = chosen.into_iter();
    let Some(action) = chosen.next() else {
        return Err(Error::Config {
            detail: "pages needs an operation: --rotate, --delete, --duplicate, \
                     --order, --move, or --reverse"
                .into(),
            source_path: None,
        });
    };
    if chosen.next().is_some() {
        return Err(Error::Config {
            detail: "one operation at a time: page numbers would mean different \
                     things before and after the first one"
                .into(),
            source_path: None,
        });
    }

    if args.pages.is_some() && !action.takes_pages() {
        return Err(Error::Config {
            detail: format!(
                "--pages does not apply to --{}: it acts on the whole document",
                name_of(&action)
            ),
            source_path: None,
        });
    }
    if args.copies != 1 && !matches!(action, Action::Duplicate(_)) {
        return Err(Error::Config {
            detail: "--copies only applies to --duplicate".into(),
            source_path: None,
        });
    }

    Ok(action)
}

/// `FROM:TO`, both 1-based.
fn parse_move(spec: &str) -> Result<(u32, u32)> {
    let bad = || Error::Config {
        detail: format!("--move wants `FROM:TO`, both page numbers, not {spec:?}"),
        source_path: None,
    };
    let (from, to) = spec.split_once(':').ok_or_else(bad)?;
    let from: u32 = from.trim().parse().map_err(|_| bad())?;
    let to: u32 = to.trim().parse().map_err(|_| bad())?;
    if from == 0 || to == 0 {
        return Err(bad());
    }
    Ok((from, to))
}

const fn name_of(action: &Action) -> &'static str {
    match action {
        Action::Rotate(_) => "rotate",
        Action::Delete => "delete",
        Action::Duplicate(_) => "duplicate",
        Action::Reorder(_) => "order",
        Action::Move(_, _) => "move",
        Action::Reverse => "reverse",
    }
}

/// What happened, in the words the person used to ask for it.
fn describe(action: &Action, pages: &[u32], args: &PagesArgs) -> String {
    let count = pages.len();
    match action {
        Action::Rotate(turns) => format!(
            "Rotated {count} page(s) by {}°",
            (turns * 90).rem_euclid(360)
        ),
        Action::Delete => format!("Deleted {count} page(s)"),
        Action::Duplicate(copies) => {
            format!("Duplicated {count} page(s) {copies} time(s)")
        }
        Action::Reorder(_) => format!(
            "Reordered to {}",
            args.order.as_deref().unwrap_or("a new order")
        ),
        Action::Move(from, to) => format!("Moved page {from} to position {to}"),
        Action::Reverse => "Reversed the document".to_string(),
    }
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

    fn pages_args(input: &str, output: &Path) -> PagesArgs {
        PagesArgs {
            inputs: vec![input.to_string()],
            output: output.to_path_buf(),
            pages: None,
            rotate: None,
            delete: false,
            duplicate: false,
            copies: 1,
            order: None,
            move_page: None,
            reverse: false,
        }
    }

    /// The `/Rotate` each page carries, in page order.
    fn rotations(path: &Path) -> Vec<i64> {
        let pdf = Pdf::open(path).expect("re-opens");
        pdf.page_ids()
            .iter()
            .map(|id| {
                pdf.raw()
                    .get_dictionary(*id)
                    .ok()
                    .and_then(|d| d.get(b"Rotate").ok())
                    .and_then(|o| o.as_i64().ok())
                    .unwrap_or(0)
            })
            .collect()
    }

    /// Page object ids in order, which is what every reordering changes.
    fn order_of(path: &Path) -> Vec<(u32, u16)> {
        Pdf::open(path).expect("re-opens").page_ids().to_vec()
    }

    #[test]
    fn rotating_turns_only_the_pages_named() {
        let dir = scratch("pages-rotate");
        let target = dir.join("out.pdf");
        let mut args = pages_args("two-pages.pdf", &target);
        args.rotate = Some(90);
        args.pages = Some("1".into());

        edit(&fixture("two-pages.pdf"), &args, None, false, false).expect("rotates");

        assert_eq!(rotations(&target), vec![90, 0], "page 2 was not asked for");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_adds_to_what_the_page_already_had() {
        let dir = scratch("pages-rotate-twice");
        let once = dir.join("once.pdf");
        let twice = dir.join("twice.pdf");

        let mut args = pages_args("two-pages.pdf", &once);
        args.rotate = Some(270);
        edit(&fixture("two-pages.pdf"), &args, None, false, false).expect("rotates");

        let mut again = pages_args("once.pdf", &twice);
        again.rotate = Some(180);
        edit(&once, &again, None, false, false).expect("rotates again");

        assert_eq!(rotations(&twice), vec![90, 90], "270 + 180 = 450 = 90");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_negative_rotation_turns_the_other_way() {
        let dir = scratch("pages-rotate-back");
        let target = dir.join("out.pdf");
        let mut args = pages_args("two-pages.pdf", &target);
        args.rotate = Some(-90);

        edit(&fixture("two-pages.pdf"), &args, None, false, false).expect("rotates");

        assert_eq!(rotations(&target), vec![270, 270]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_leaves_the_rest() {
        let dir = scratch("pages-delete");
        let target = dir.join("out.pdf");
        let mut args = pages_args("many-pages.pdf", &target);
        args.delete = true;
        args.pages = Some("2-4".into());

        let report = edit(&fixture("many-pages.pdf"), &args, None, false, false).expect("deletes");

        assert_eq!(report.json["pages_before"], 12);
        assert_eq!(report.json["pages_after"], 9);
        assert_eq!(Pdf::open(&target).expect("re-opens").page_count(), 9);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_every_page_is_refused() {
        let dir = scratch("pages-delete-all");
        let target = dir.join("out.pdf");
        let mut args = pages_args("two-pages.pdf", &target);
        args.delete = true;

        assert!(
            edit(&fixture("two-pages.pdf"), &args, None, false, false).is_err(),
            "a document with no pages is not a document"
        );
        assert!(!target.exists(), "nothing is written when it is refused");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicating_repeats_each_page_as_many_times_as_asked() {
        let dir = scratch("pages-duplicate");
        let target = dir.join("out.pdf");
        let before = order_of(&fixture("two-pages.pdf"));
        let mut args = pages_args("two-pages.pdf", &target);
        args.duplicate = true;
        args.copies = 2;

        edit(&fixture("two-pages.pdf"), &args, None, false, false).expect("duplicates");

        // Both pages asked for, so both are tripled. Copying the copies would
        // give four of page one and two of page two instead.
        assert_eq!(
            order_of(&target),
            vec![
                before[0], before[0], before[0], before[1], before[1], before[1]
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reordering_puts_the_pages_where_it_says() {
        let dir = scratch("pages-order");
        let target = dir.join("out.pdf");
        let before = order_of(&fixture("many-pages.pdf"));

        let mut args = pages_args("many-pages.pdf", &target);
        args.order = Some("12,1-11".into());
        edit(&fixture("many-pages.pdf"), &args, None, false, false).expect("reorders");

        let after = order_of(&target);
        assert_eq!(after[0], before[11], "the last page came first");
        assert_eq!(after[1], before[0]);
        assert_eq!(after.len(), before.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reordering_that_drops_a_page_is_refused() {
        let dir = scratch("pages-order-short");
        let target = dir.join("out.pdf");
        let mut args = pages_args("many-pages.pdf", &target);
        args.order = Some("2,1".into());

        assert!(
            edit(&fixture("many-pages.pdf"), &args, None, false, false).is_err(),
            "a reordering must account for every page"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn moving_a_page_is_a_drag_in_the_sidebar() {
        let dir = scratch("pages-move");
        let target = dir.join("out.pdf");
        let before = order_of(&fixture("many-pages.pdf"));

        let mut args = pages_args("many-pages.pdf", &target);
        args.move_page = Some("5:1".into());
        edit(&fixture("many-pages.pdf"), &args, None, false, false).expect("moves");

        let after = order_of(&target);
        assert_eq!(after[0], before[4]);
        assert_eq!(after[1], before[0]);
        assert_eq!(after.len(), before.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reversing_turns_the_document_around() {
        let dir = scratch("pages-reverse");
        let target = dir.join("out.pdf");
        let before = order_of(&fixture("many-pages.pdf"));

        let mut args = pages_args("many-pages.pdf", &target);
        args.reverse = true;
        edit(&fixture("many-pages.pdf"), &args, None, false, false).expect("reverses");

        let mut expected = before;
        expected.reverse();
        assert_eq!(order_of(&target), expected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_edit_refuses_to_replace_an_existing_file() {
        let dir = scratch("pages-guard");
        let target = dir.join("out.pdf");
        std::fs::write(&target, b"not a pdf").expect("writes");

        let mut args = pages_args("two-pages.pdf", &target);
        args.reverse = true;

        assert!(edit(&fixture("two-pages.pdf"), &args, None, false, false).is_err());
        assert_eq!(
            std::fs::read(&target).expect("still there"),
            b"not a pdf",
            "the existing file must be untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn naming_no_operation_is_an_error() {
        let args = pages_args("two-pages.pdf", &PathBuf::from("out.pdf"));
        let error = action_of(&args).expect_err("nothing to do");
        let reason = error.reason().unwrap_or_default();
        assert!(
            reason.contains("--rotate"),
            "the error should list what is on offer: {reason}"
        );
    }

    #[test]
    fn naming_two_operations_is_an_error() {
        let mut args = pages_args("two-pages.pdf", &PathBuf::from("out.pdf"));
        args.reverse = true;
        args.delete = true;
        assert!(
            action_of(&args).is_err(),
            "page numbers would mean different things either side of the first one"
        );
    }

    #[test]
    fn a_rotation_that_is_not_a_quarter_turn_is_refused() {
        let mut args = pages_args("two-pages.pdf", &PathBuf::from("out.pdf"));
        args.rotate = Some(45);
        let error = action_of(&args).expect_err("not a quarter turn");
        let reason = error.reason().unwrap_or_default();
        assert!(reason.contains("90"), "{reason}");
    }

    #[test]
    fn pages_alongside_a_whole_document_operation_is_refused() {
        let mut args = pages_args("two-pages.pdf", &PathBuf::from("out.pdf"));
        args.reverse = true;
        args.pages = Some("1".into());
        assert!(
            action_of(&args).is_err(),
            "silently ignoring --pages would be worse than refusing it"
        );
    }

    #[test]
    fn copies_without_duplicate_is_refused() {
        let mut args = pages_args("two-pages.pdf", &PathBuf::from("out.pdf"));
        args.reverse = true;
        args.copies = 3;
        assert!(action_of(&args).is_err());
    }

    #[test]
    fn a_move_needs_two_page_numbers() {
        assert!(parse_move("3:1").is_ok());
        for bad in ["3", "3:", ":1", "a:b", "0:1", "3:0"] {
            assert!(parse_move(bad).is_err(), "{bad} is not a move");
        }
    }

    #[test]
    fn several_inputs_write_one_file_each_into_a_directory() {
        let dir = scratch("pages-many");
        let mut args = pages_args("two-pages.pdf", &dir);
        args.reverse = true;

        for name in ["two-pages.pdf", "many-pages.pdf"] {
            edit(&fixture(name), &args, None, true, false).expect("reverses");
        }

        assert!(dir.join("two-pages-pages.pdf").exists());
        assert!(dir.join("many-pages-pages.pdf").exists());
        let _ = std::fs::remove_dir_all(&dir);
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
