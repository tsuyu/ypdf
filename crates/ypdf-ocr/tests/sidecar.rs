//! The sidecar path, end to end, without Tesseract installed.
//!
//! Tesseract is a process that takes an image path and prints TSV. That is a
//! small enough contract to stand in for, so these tests point the engine at a
//! script that prints canned TSV and check everything around it: discovery,
//! invocation, parsing, coordinate mapping, and whether the words come back out
//! of the finished document.
//!
//! What this deliberately does not test is recognition quality — that belongs
//! to Tesseract, and a fake cannot say anything about it. Tests that need the
//! real binary are marked `#[ignore]` and named so the reason is visible when
//! they are skipped.

// Integration tests compile as their own crate, so `cfg(test)` is not set and
// the clippy.toml allowance for tests does not reach here.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use ypdf_doc::Pdf;
use ypdf_ocr::{OcrSettings, Tesseract};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("ypdf-ocr-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// Two words on one line, in a 1224x1584 image.
const CANNED_TSV: &str =
    "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext
5\t1\t1\t1\t1\t1\t100\t120\t180\t40\t96.5\tQuarterly
5\t1\t1\t1\t1\t2\t300\t120\t120\t40\t95.1\tReport
5\t1\t1\t1\t2\t1\t100\t200\t90\t36\t9.0\trnaybe
";

/// Write a script that behaves enough like Tesseract for the plumbing.
///
/// It answers `--version` and `--list-langs` because the engine asks both
/// before doing any work, and prints the canned TSV for anything else.
fn stub(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        let path = dir.join("tesseract.bat");
        let script = format!(
            "@echo off\r\n\
             if \"%1\"==\"--version\" (echo tesseract 5.3.3-stub & exit /b 0)\r\n\
             if \"%1\"==\"--list-langs\" (echo List of available languages ^(2^): & echo eng & echo msa & exit /b 0)\r\n\
             {}\r\n\
             exit /b 0\r\n",
            CANNED_TSV
                .lines()
                .map(|line| format!("echo {line}"))
                .collect::<Vec<_>>()
                .join("\r\n")
        );
        std::fs::write(&path, script).expect("writes the stub");
        path
    } else {
        let path = dir.join("tesseract.sh");
        let script = format!(
            "#!/bin/sh\n\
             case \"$1\" in\n\
             --version) echo 'tesseract 5.3.3-stub'; exit 0;;\n\
             --list-langs) printf 'List of available languages (2):\\neng\\nmsa\\n'; exit 0;;\n\
             esac\n\
             cat <<'TSV'\n{CANNED_TSV}TSV\n"
        );
        std::fs::write(&path, script).expect("writes the stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("makes it executable");
        }
        path
    }
}

/// On Windows a `.bat` cannot be executed directly by `Command` in every
/// configuration, so the stub tests run through the shell wrapper the engine
/// already uses: the path is handed to `Command::new`, which does resolve
/// `.bat` through the Windows loader.
fn stub_engine(dir: &Path) -> Tesseract {
    Tesseract::at(stub(dir))
}

#[test]
fn the_engine_reports_the_version_the_binary_prints() {
    let dir = scratch("version");
    let engine = stub_engine(&dir);

    let version = engine.version().expect("the stub answers --version");
    assert!(version.contains("5.3.3-stub"), "{version}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn installed_languages_are_checked_before_any_work_is_done() {
    let dir = scratch("languages");
    let engine = stub_engine(&dir);

    assert!(engine.check_languages("eng").is_ok());
    assert!(engine.check_languages("eng+msa").is_ok());

    // A missing pack fails with the language named, not with a message about
    // traineddata files.
    let error = engine
        .check_languages("eng+jpn")
        .expect_err("jpn is not installed");
    let human = error.report().to_human();
    assert!(human.contains("jpn"), "{human}");
    assert!(human.contains("eng"), "{human}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_binary_says_where_it_looked() {
    // The whole value of this error is that someone can act on it.
    let error = Tesseract::at(std::env::temp_dir().join("no-such-tesseract"))
        .version()
        .expect_err("no such binary");
    assert_eq!(error.code(), "E_IO");
}

#[test]
fn recognized_words_end_up_in_the_document_and_come_back_out() {
    // The point of the whole feature: a scan that can be searched afterwards.
    let dir = scratch("round-trip");
    let engine = stub_engine(&dir);

    let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
    // 1224x1584 RGBA, white. Twice the page in each direction.
    let rgba = vec![255_u8; 1224 * 1584 * 4];

    let report = ypdf_ocr::ocr_page(
        &engine,
        &mut pdf,
        0,
        &rgba,
        (1224, 1584),
        &OcrSettings {
            language: "eng".into(),
            min_confidence: 40.0,
            // The fixture has real text; this test is about the layer.
            skip_pages_with_text: false,
        },
    )
    .expect("recognizes");

    assert_eq!(report.words, 2, "the low-confidence word must be dropped");
    assert!(report.confidence > 90.0, "{}", report.confidence);

    let bytes = pdf.to_bytes().expect("serializes");
    let text = lopdf::Document::load_mem(&bytes)
        .expect("re-opens")
        .extract_text(&[1])
        .expect("extracts");

    assert!(text.contains("Quarterly"), "{text:?}");
    assert!(text.contains("Report"), "{text:?}");
    assert!(
        !text.contains("rnaybe"),
        "low confidence leaked in: {text:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_page_still_shows_exactly_what_it_showed_before() {
    // The scan is never modified. The original content stream has to survive
    // untouched, whatever the recognition produced.
    let dir = scratch("untouched");
    let engine = stub_engine(&dir);

    let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
    let page_id = pdf.page_ids()[0];
    let before = pdf.raw().get_page_content(page_id);

    ypdf_ocr::ocr_page(
        &engine,
        &mut pdf,
        0,
        &vec![255_u8; 1224 * 1584 * 4],
        (1224, 1584),
        &OcrSettings {
            skip_pages_with_text: false,
            ..OcrSettings::default()
        },
    )
    .expect("recognizes");

    let after = pdf.raw().get_page_content(page_id);
    assert!(
        after.len() > before.len(),
        "the layer should have been added"
    );
    assert!(
        after.starts_with(&before),
        "the original drawing must still come first, unchanged"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_page_that_already_has_text_is_left_alone_by_default() {
    // Laying a guess over real text gives search two answers for the same
    // words, and the guess is the worse one.
    let dir = scratch("skip");
    let engine = stub_engine(&dir);

    let mut pdf = Pdf::open(fixture("many-pages.pdf")).expect("opens");
    let page_id = pdf.page_ids()[0];
    let before = pdf.raw().get_page_content(page_id);

    let report = ypdf_ocr::ocr_page(
        &engine,
        &mut pdf,
        0,
        &vec![255_u8; 612 * 792 * 4],
        (612, 792),
        &OcrSettings::default(),
    )
    .expect("runs");

    assert!(report.skipped_has_text);
    assert_eq!(report.words, 0);
    assert_eq!(
        pdf.raw().get_page_content(page_id),
        before,
        "nothing may have been written"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failing_engine_is_reported_rather_than_silently_producing_nothing() {
    let dir = scratch("failing");
    let path = if cfg!(windows) {
        let path = dir.join("failing.bat");
        std::fs::write(&path, "@echo off\r\necho boom 1>&2\r\nexit /b 1\r\n").expect("writes");
        path
    } else {
        let path = dir.join("failing.sh");
        std::fs::write(&path, "#!/bin/sh\necho boom >&2\nexit 1\n").expect("writes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("makes it executable");
        }
        path
    };

    let engine = Tesseract::at(path);
    let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
    let error = ypdf_ocr::ocr_page(
        &engine,
        &mut pdf,
        0,
        &vec![255_u8; 612 * 792 * 4],
        (612, 792),
        &OcrSettings {
            skip_pages_with_text: false,
            ..OcrSettings::default()
        },
    )
    .expect_err("the engine failed");

    assert_eq!(error.code(), "E_BACKEND");
    assert!(error.report().to_human().contains("boom"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "needs a real Tesseract installation; run with --ignored once one is present"]
fn a_real_tesseract_is_found_and_usable() {
    let engine = Tesseract::find().expect("Tesseract is installed");
    let version = engine.version().expect("it answers");
    assert!(version.to_lowercase().contains("tesseract"), "{version}");
    assert!(!engine.languages().expect("languages").is_empty());
}
