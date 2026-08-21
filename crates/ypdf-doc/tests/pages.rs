// Integration tests compile as their own crate, so `cfg(test)` is not set and
// the clippy.toml allowance for tests does not reach here. A panic is the
// failure report in a test.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Page operations, verified by reading the result back.
//!
//! Every test saves the edited document and re-opens it. Anything less would
//! pass on a document that lopdf holds happily in memory but writes out
//! corrupt, which is the failure mode that matters.

use std::path::PathBuf;

use ypdf_doc::{PageSpec, Pdf, SplitMode, split};

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

/// Save and re-open, so every assertion is about a document that survived a
/// write.
fn round_trip(pdf: &mut Pdf) -> Pdf {
    let bytes = match pdf.to_bytes() {
        Ok(bytes) => bytes,
        Err(e) => panic!("{}", e.report().to_human()),
    };
    match Pdf::from_bytes(&bytes) {
        Ok(pdf) => pdf,
        Err(e) => panic!(
            "re-opening the saved document failed: {}",
            e.report().to_human()
        ),
    }
}

/// The text of each page, used to prove pages moved rather than just counted.
fn page_texts(pdf: &mut Pdf) -> Vec<String> {
    let bytes = pdf.to_bytes().expect("serializes");
    let doc = lopdf::Document::load_mem(&bytes).expect("re-opens");
    (1..=doc.get_pages().len() as u32)
        .map(|page| {
            doc.extract_text(&[page])
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .collect()
}

#[test]
fn opens_and_counts_pages() {
    let pdf = open("many-pages.pdf");
    assert_eq!(pdf.page_count(), 12);
    assert!(!pdf.is_empty());
    assert!(pdf.path().is_some());
}

#[test]
fn extract_keeps_only_the_pages_asked_for_in_that_order() {
    let mut pdf = open("many-pages.pdf");
    pdf.extract(&[5, 1, 2]).expect("extracts");
    let mut result = round_trip(&mut pdf);

    assert_eq!(result.page_count(), 3);
    let texts = page_texts(&mut result);
    assert!(
        texts[0].contains('5'),
        "first page should be page 5, got {:?}",
        texts[0]
    );
    assert!(
        texts[1].contains('1'),
        "second page should be page 1, got {:?}",
        texts[1]
    );
}

#[test]
fn delete_removes_pages_and_keeps_the_rest_in_order() {
    let mut pdf = open("many-pages.pdf");
    pdf.delete(&[2, 3]).expect("deletes");
    let mut result = round_trip(&mut pdf);

    assert_eq!(result.page_count(), 10);
    let texts = page_texts(&mut result);
    assert!(texts[0].contains('1'));
    assert!(
        texts[1].contains('4'),
        "page 4 should follow page 1, got {:?}",
        texts[1]
    );
}

#[test]
fn deleting_every_page_is_refused() {
    let mut pdf = open("two-pages.pdf");
    let err = pdf.delete(&[1, 2]).expect_err("must refuse");
    assert_eq!(err.code(), "E_PAGE_SPEC");
    assert_eq!(pdf.page_count(), 2, "the document must be untouched");
}

#[test]
fn duplicate_places_each_copy_after_its_original() {
    let mut pdf = open("many-pages.pdf");
    pdf.duplicate(&[2]).expect("duplicates");
    let mut result = round_trip(&mut pdf);

    assert_eq!(result.page_count(), 13);
    let texts = page_texts(&mut result);
    assert_eq!(texts[1], texts[2], "the copy must sit next to the original");
}

#[test]
fn reverse_turns_the_document_around() {
    let mut pdf = open("many-pages.pdf");
    let before = page_texts(&mut pdf);
    pdf.reverse();
    let mut result = round_trip(&mut pdf);

    let after = page_texts(&mut result);
    assert_eq!(after.len(), before.len());
    assert_eq!(after[0], before[before.len() - 1]);
    assert_eq!(after[after.len() - 1], before[0]);
}

#[test]
fn reorder_requires_a_complete_permutation() {
    let mut pdf = open("two-pages.pdf");
    assert!(pdf.reorder(&[2, 1]).is_ok());
    // Too few, repeated, and out of range are all rejected.
    assert!(pdf.reorder(&[1]).is_err());
    assert!(pdf.reorder(&[1, 1]).is_err());
    assert!(pdf.reorder(&[1, 5]).is_err());
    assert_eq!(pdf.page_count(), 2);
}

#[test]
fn move_page_is_a_drag_in_the_sidebar() {
    let mut pdf = open("many-pages.pdf");
    let before = page_texts(&mut pdf);
    pdf.move_page(1, 3).expect("moves");
    let mut result = round_trip(&mut pdf);

    let after = page_texts(&mut result);
    assert_eq!(
        after.len(),
        before.len(),
        "moving must not change the page count"
    );
    assert_eq!(after[2], before[0], "page 1 should now be third");
    assert_eq!(after[0], before[1]);
}

#[test]
fn rotation_accumulates_and_survives_a_save() {
    let mut pdf = open("two-pages.pdf");
    pdf.rotate(&[1], 1).expect("rotates");
    pdf.rotate(&[1], 1).expect("rotates again");
    let bytes = pdf.to_bytes().expect("serializes");

    let doc = lopdf::Document::load_mem(&bytes).expect("re-opens");
    let page_id = *doc.get_pages().get(&1).expect("page 1");
    let rotate = doc
        .get_dictionary(page_id)
        .expect("page dictionary")
        .get(b"Rotate")
        .expect("a /Rotate entry")
        .as_i64()
        .expect("an integer");
    assert_eq!(rotate, 180, "two quarter turns is a half turn");
}

#[test]
fn rotation_wraps_rather_than_growing() {
    let mut pdf = open("two-pages.pdf");
    pdf.rotate(&[1], 5).expect("rotates");
    let bytes = pdf.to_bytes().expect("serializes");
    let doc = lopdf::Document::load_mem(&bytes).expect("re-opens");
    let page_id = *doc.get_pages().get(&1).expect("page 1");
    let rotate = doc
        .get_dictionary(page_id)
        .expect("dict")
        .get(b"Rotate")
        .expect("rotate")
        .as_i64();
    assert_eq!(rotate.expect("integer"), 90);
}

#[test]
fn merge_appends_documents_in_order() {
    let a = open("two-pages.pdf");
    let b = open("linked.pdf");
    let mut merged = Pdf::merge(&[a, b]).expect("merges");
    let mut result = round_trip(&mut merged);

    assert_eq!(result.page_count(), 4);
    let texts = page_texts(&mut result);
    assert!(texts[0].contains("Page One"), "got {:?}", texts[0]);
    assert!(
        texts[2].contains("quick brown fox"),
        "the second document follows: {:?}",
        texts[2]
    );
}

#[test]
fn merging_the_same_document_twice_does_not_collide() {
    // Both copies share object ids; importing must renumber, or the second
    // silently overwrites the first.
    let mut merged = Pdf::merge(&[open("two-pages.pdf"), open("two-pages.pdf")]).expect("merges");
    let mut result = round_trip(&mut merged);
    assert_eq!(result.page_count(), 4);

    let texts = page_texts(&mut result);
    assert_eq!(
        texts[0], texts[2],
        "the two copies must be identical, not corrupt"
    );
    assert!(texts[0].contains("Page One"));
}

#[test]
fn merge_of_nothing_is_an_error_not_an_empty_document() {
    assert!(Pdf::merge(&[]).is_err());
}

#[test]
fn insert_from_places_pages_at_a_position() {
    let mut target = open("many-pages.pdf");
    let source = open("two-pages.pdf");
    target.insert_from(&source, 2).expect("inserts");
    let mut result = round_trip(&mut target);

    assert_eq!(result.page_count(), 14);
    let texts = page_texts(&mut result);
    assert!(
        texts[1].contains("Page One"),
        "inserted page should be second, got {:?}",
        texts[1]
    );
}

#[test]
fn replace_swaps_a_run_of_pages() {
    let mut target = open("many-pages.pdf");
    let source = open("two-pages.pdf");
    target.replace(&[1, 2, 3], &source).expect("replaces");
    let mut result = round_trip(&mut target);

    // Three pages out, two in.
    assert_eq!(result.page_count(), 11);
    let texts = page_texts(&mut result);
    assert!(texts[0].contains("Page One"), "got {:?}", texts[0]);
    assert!(
        texts[2].contains('4'),
        "the original page 4 should follow, got {:?}",
        texts[2]
    );
}

#[test]
fn split_by_page_yields_one_document_each() {
    let pdf = open("many-pages.pdf");
    let pieces = split(&pdf, &SplitMode::EachPage).expect("splits");
    assert_eq!(pieces.len(), 12);
    assert_eq!(pieces[0].pdf.page_count(), 1);
    assert_eq!(pieces[0].name, "page-1");
    assert_eq!(pieces[11].pages, vec![12]);
}

#[test]
fn split_every_n_covers_every_page_including_a_short_last_piece() {
    let pdf = open("many-pages.pdf");
    let pieces = split(&pdf, &SplitMode::EveryN(5)).expect("splits");
    assert_eq!(pieces.len(), 3);
    assert_eq!(pieces[0].pages, (1..=5).collect::<Vec<_>>());
    assert_eq!(
        pieces[2].pages,
        vec![11, 12],
        "the remainder is its own piece"
    );
    assert_eq!(pieces[2].name, "pages-11-12");

    let total: usize = pieces.iter().map(|p| p.pages.len()).sum();
    assert_eq!(total, 12, "no page may be dropped");
}

#[test]
fn split_by_zero_is_refused() {
    let pdf = open("many-pages.pdf");
    assert!(split(&pdf, &SplitMode::EveryN(0)).is_err());
}

#[test]
fn split_by_ranges_follows_the_expressions() {
    let pdf = open("many-pages.pdf");
    let specs = vec![
        PageSpec::parse("1-3").expect("parses"),
        PageSpec::parse("10-").expect("parses"),
    ];
    let mut pieces = split(&pdf, &SplitMode::Ranges(specs)).expect("splits");

    assert_eq!(pieces.len(), 2);
    assert_eq!(pieces[0].pdf.page_count(), 3);
    assert_eq!(pieces[1].pages, vec![10, 11, 12]);

    let texts = page_texts(&mut pieces[1].pdf);
    assert!(texts[0].contains("10"), "got {:?}", texts[0]);
}

#[test]
fn split_by_bookmarks_on_an_unbookmarked_file_gives_one_piece() {
    // Refusing here would fail on exactly the files people try it on first.
    let pdf = open("many-pages.pdf");
    let pieces = split(&pdf, &SplitMode::Bookmarks).expect("splits");
    assert_eq!(pieces.len(), 1);
    assert_eq!(pieces[0].pdf.page_count(), 12);
}

#[test]
fn split_by_bookmarks_cuts_at_each_top_level_bookmark() {
    let pdf = open("outlined.pdf");
    let pieces = split(&pdf, &SplitMode::Bookmarks).expect("splits");

    // The fixture bookmarks pages 1, 3 and 5 of a six-page document.
    assert_eq!(pieces.len(), 3);
    assert_eq!(pieces[0].pages, vec![1, 2]);
    assert_eq!(pieces[1].pages, vec![3, 4]);
    assert_eq!(pieces[2].pages, vec![5, 6]);
}

#[test]
fn a_missing_file_reports_io_not_a_parse_failure() {
    let err = Pdf::open(fixture("does-not-exist.pdf")).expect_err("must fail");
    assert_eq!(err.code(), "E_IO");
}

#[test]
fn a_page_beyond_the_end_is_rejected() {
    let mut pdf = open("two-pages.pdf");
    let err = pdf.extract(&[99]).expect_err("must fail");
    assert_eq!(err.code(), "E_PAGE_RANGE");
}

#[test]
fn edits_compose_and_the_result_still_opens() {
    let mut pdf = open("many-pages.pdf");
    pdf.delete(&[1]).expect("delete");
    pdf.duplicate(&[1]).expect("duplicate");
    pdf.rotate(&[1], 1).expect("rotate");
    pdf.reverse();
    let result = round_trip(&mut pdf);
    // 12 pages, one deleted, one duplicated.
    assert_eq!(result.page_count(), 12);
}
