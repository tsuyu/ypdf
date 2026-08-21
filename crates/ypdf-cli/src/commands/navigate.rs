//! `bookmarks` and `links` — reading and editing how a document is navigated
//! (spec §17, §18).
//!
//! Both commands read by default and only write when asked to. A tool that
//! rewrites the outline of every file it is pointed at, because listing and
//! editing share a command, is a tool people learn to be afraid of.

use std::path::Path;

use serde_json::json;
use ypdf_core::{CancelToken, Error, Result};
use ypdf_outline::{Bookmark, Link, Target, bookmarks, links};

use crate::batch::FileReport;
use crate::cli::{BookmarksArgs, LinksArgs};
use crate::paths;

/// `bookmarks`.
pub fn bookmarks_command(
    path: &Path,
    args: &BookmarksArgs,
    password: Option<&str>,
    many: bool,
    overwrite: bool,
    _cancel: &CancelToken,
) -> Result<FileReport> {
    let mut pdf = super::open_input(path, password)?;
    let tree = bookmarks::read(&pdf);

    // Reading: print what is there and touch nothing.
    if !args.is_edit() {
        return Ok(report(&tree, path, None));
    }

    let new_tree = if args.clear {
        Vec::new()
    } else if let Some(source) = &args.set {
        let text = std::fs::read_to_string(source).map_err(|e| Error::io(source, e))?;
        serde_json::from_str::<Vec<Bookmark>>(&text).map_err(|e| Error::Config {
            detail: format!("{} is not a bookmark tree: {e}", source.display()),
            source_path: Some(source.clone()),
        })?
    } else {
        let mut tree = tree.clone();
        for entry in &args.add {
            tree.push(parse_bookmark(entry, pdf.page_count())?);
        }
        tree
    };

    let written = bookmarks::write(&mut pdf, &new_tree)?;

    let target = match &args.output {
        Some(output) => paths::output_for(output, path, many, "")?,
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

    Ok(report(&new_tree, path, Some((&target, written))))
}

fn report(tree: &[Bookmark], path: &Path, written: Option<(&Path, usize)>) -> FileReport {
    let flat = bookmarks::flatten(tree);

    let mut human = if flat.is_empty() {
        "No bookmarks.".to_string()
    } else {
        flat.iter()
            .map(|(depth, bookmark)| {
                format!(
                    "{}{}{}",
                    "  ".repeat(*depth),
                    bookmark.title,
                    bookmark
                        .page
                        .map_or_else(String::new, |page| format!("  (page {page})"))
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    if let Some((target, count)) = written {
        human.push_str(&format!(
            "\n\n{count} bookmark(s) written to {}",
            target.display()
        ));
    }

    FileReport::text(
        human,
        json!({
            "input": path.display().to_string(),
            "count": flat.len(),
            "bookmarks": tree,
            "written_to": written.map(|(target, _)| target.display().to_string()),
        }),
    )
}

/// Parse `Title=page` or `Title:page`.
fn parse_bookmark(entry: &str, pages: u32) -> Result<Bookmark> {
    let bad = || Error::Config {
        detail: format!("{entry:?} is not a bookmark; expected \"Title=page\", e.g. \"Results=7\""),
        source_path: None,
    };

    let (title, page) = entry.rsplit_once(['=', ':']).ok_or_else(bad)?;
    let page: u32 = page.trim().parse().map_err(|_| bad())?;
    if page == 0 || page > pages {
        return Err(Error::PageOutOfRange {
            requested: page,
            pages,
        });
    }
    if title.trim().is_empty() {
        return Err(bad());
    }

    Ok(Bookmark::new(title.trim(), page))
}

/// `links`.
pub fn links_command(
    path: &Path,
    args: &LinksArgs,
    password: Option<&str>,
    many: bool,
    overwrite: bool,
    _cancel: &CancelToken,
) -> Result<FileReport> {
    let mut pdf = super::open_input(path, password)?;

    if !args.is_edit() {
        let found = links::list(&pdf);
        return Ok(links_report(&found, path, 0, None));
    }

    let mut removed = 0;
    if args.remove_all {
        removed += links::remove_where(&mut pdf, |_| true);
    } else if let Some(needle) = &args.remove_matching {
        let needle = needle.to_lowercase();
        removed += links::remove_where(&mut pdf, |link| {
            link.target.describe().to_lowercase().contains(&needle)
        });
    }
    if args.remove_external {
        removed += links::remove_where(&mut pdf, |link| link.target.leaves_the_document());
    }

    for entry in &args.add {
        links::add(&mut pdf, &parse_link(entry)?)?;
    }

    let target = match &args.output {
        Some(output) => paths::output_for(output, path, many, "")?,
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

    let remaining = links::list(&pdf);
    Ok(links_report(&remaining, path, removed, Some(&target)))
}

fn links_report(found: &[Link], path: &Path, removed: usize, written: Option<&Path>) -> FileReport {
    let mut human = if found.is_empty() {
        "No links.".to_string()
    } else {
        found
            .iter()
            .map(|link| {
                // Printed as plain text, never as something a terminal turns
                // into a clickable link: this list exists so someone can read
                // where a document wants to send them.
                format!("page {:>3}  {}", link.page, link.target.describe())
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    if removed > 0 {
        human.push_str(&format!("\n\n{removed} link(s) removed."));
    }
    if let Some(target) = written {
        human.push_str(&format!("\nWritten to {}", target.display()));
    }

    FileReport::text(
        human,
        json!({
            "input": path.display().to_string(),
            "count": found.len(),
            "removed": removed,
            "links": found,
            "written_to": written.map(|target| target.display().to_string()),
        }),
    )
}

/// Parse `page:x0,y0,x1,y1=target`.
fn parse_link(entry: &str) -> Result<Link> {
    let bad = || Error::Config {
        detail: format!(
            "{entry:?} is not a link; expected page:x0,y0,x1,y1=target, e.g. \
             1:100,700,200,720=https://example.org"
        ),
        source_path: None,
    };

    let (area, destination) = entry.split_once('=').ok_or_else(bad)?;
    let (page, numbers) = area.split_once(':').ok_or_else(bad)?;
    let page: u32 = page.trim().parse().map_err(|_| bad())?;

    let values: Vec<f32> = numbers
        .split(',')
        .map(|value| value.trim().parse::<f32>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| bad())?;
    let rect = match values[..] {
        [x0, y0, x1, y1] => [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)],
        _ => return Err(bad()),
    };

    let destination = destination.trim();
    let target = if let Some(address) = destination.strip_prefix("mailto:") {
        Target::Email {
            address: address.to_string(),
        }
    } else if let Some(page) = destination.strip_prefix("page:") {
        Target::Page {
            page: page.trim().parse().map_err(|_| bad())?,
        }
    } else if destination.contains("://") {
        Target::Url {
            url: destination.to_string(),
        }
    } else {
        return Err(Error::Config {
            detail: format!(
                "{destination:?} is not a link target; use a URL, mailto:someone@example.org, \
                 or page:4"
            ),
            source_path: None,
        });
    };

    Ok(Link { page, rect, target })
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

    fn bookmark_args() -> BookmarksArgs {
        BookmarksArgs {
            inputs: Vec::new(),
            output: None,
            add: Vec::new(),
            set: None,
            clear: false,
        }
    }

    fn link_args() -> LinksArgs {
        LinksArgs {
            inputs: Vec::new(),
            output: None,
            add: Vec::new(),
            remove_all: false,
            remove_external: false,
            remove_matching: None,
        }
    }

    #[test]
    fn a_bookmark_entry_is_title_and_page() {
        let bookmark = parse_bookmark("Results=7", 12).expect("parses");
        assert_eq!(bookmark.title, "Results");
        assert_eq!(bookmark.page, Some(7));
    }

    #[test]
    fn a_title_containing_a_colon_still_parses() {
        // "Chapter 2: Methods=5" splits at the last separator, not the first.
        let bookmark = parse_bookmark("Chapter 2: Methods=5", 12).expect("parses");
        assert_eq!(bookmark.title, "Chapter 2: Methods");
        assert_eq!(bookmark.page, Some(5));
    }

    #[test]
    fn a_bookmark_past_the_end_is_refused() {
        let error = parse_bookmark("Nowhere=99", 12).expect_err("refused");
        assert_eq!(error.code(), "E_PAGE_RANGE");
    }

    #[test]
    fn a_link_entry_carries_an_area_and_a_target() {
        let link = parse_link("1:100,700,200,720=https://example.org").expect("parses");
        assert_eq!(link.page, 1);
        assert_eq!(link.rect, [100.0, 700.0, 200.0, 720.0]);
        assert_eq!(
            link.target,
            Target::Url {
                url: "https://example.org".into()
            }
        );
    }

    #[test]
    fn an_internal_target_and_an_email_are_recognized() {
        assert_eq!(
            parse_link("1:0,0,10,10=page:4").expect("parses").target,
            Target::Page { page: 4 }
        );
        assert_eq!(
            parse_link("1:0,0,10,10=mailto:a@b.test")
                .expect("parses")
                .target,
            Target::Email {
                address: "a@b.test".into()
            }
        );
    }

    #[test]
    fn a_target_that_is_not_a_link_says_what_one_looks_like() {
        let error = parse_link("1:0,0,10,10=somewhere").expect_err("refused");
        assert!(error.report().to_human().contains("page:4"));
    }

    #[test]
    fn listing_bookmarks_does_not_touch_the_file() {
        let path = fixture("outlined.pdf");
        let before = std::fs::read(&path).expect("reads");

        let report = bookmarks_command(
            &path,
            &bookmark_args(),
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("lists");

        assert!(report.json["count"].as_u64().unwrap_or(0) > 0);
        assert!(report.json["written_to"].is_null());
        assert_eq!(std::fs::read(&path).expect("reads"), before);
    }

    #[test]
    fn editing_bookmarks_in_place_is_refused_without_permission() {
        let mut args = bookmark_args();
        args.add = vec!["Appendix=2".into()];

        let result = bookmarks_command(
            &fixture("two-pages.pdf"),
            &args,
            None,
            false,
            false,
            &CancelToken::never(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn added_bookmarks_are_written_and_read_back() {
        let dir = scratch("bookmarks");
        let mut args = bookmark_args();
        args.add = vec!["Introduction=1".into(), "Results=7".into()];
        args.output = Some(dir.join("out.pdf"));

        let report = bookmarks_command(
            &fixture("many-pages.pdf"),
            &args,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("writes");

        assert_eq!(report.json["count"], 2);
        let pdf = ypdf_doc::Pdf::open(dir.join("out.pdf")).expect("re-opens");
        let tree = bookmarks::read(&pdf);
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[1].page, Some(7));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn listing_links_does_not_touch_the_file() {
        let path = fixture("linked.pdf");
        let before = std::fs::read(&path).expect("reads");

        let report = links_command(
            &path,
            &link_args(),
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("lists");

        assert!(report.json["count"].as_u64().unwrap_or(0) > 0);
        assert_eq!(std::fs::read(&path).expect("reads"), before);
    }

    #[test]
    fn external_links_can_be_stripped_while_internal_ones_stay() {
        let dir = scratch("links-strip");
        let mut args = link_args();
        args.remove_external = true;
        args.output = Some(dir.join("out.pdf"));

        let report = links_command(
            &fixture("hostile.pdf"),
            &args,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("writes");

        assert!(report.json["removed"].as_u64().unwrap_or(0) > 0);
        let pdf = ypdf_doc::Pdf::open(dir.join("out.pdf")).expect("re-opens");
        assert!(
            links::list(&pdf)
                .iter()
                .all(|link| !link.target.leaves_the_document())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
