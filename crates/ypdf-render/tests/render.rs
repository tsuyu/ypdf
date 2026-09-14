// Integration tests compile as their own crate, so `cfg(test)` is not set and
// the clippy.toml allowance for tests does not reach here. A panic is the
// failure report in a test.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! End-to-end tests against the real PDFium library.
//!
//! These need the vendored binary; run `scripts/fetch-pdfium.ps1` or
//! `scripts/fetch-pdfium.sh` first.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use ypdf_render::{
    LinkTarget, Priority, QuarterTurns, RenderEvent, RenderHandle, RenderRequest, SearchOptions,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

/// There is one render thread per process by design, so the tests take turns.
///
/// The guard comes first in the returned tuple on purpose: locals drop in
/// reverse declaration order, so `let (_gate, render) = handle();` tears the
/// render thread down *before* releasing the gate. The other order lets the
/// next test spawn while this one is still shutting down.
static GATE: Mutex<()> = Mutex::new(());

/// Spawn the render thread, failing loudly with the search paths if PDFium is
/// missing — that message is the whole point of the binding diagnostic.
fn handle() -> (MutexGuard<'static, ()>, RenderHandle) {
    // A panicking test poisons the gate; the lock is still usable and the next
    // test should run, not cascade.
    let guard = GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match RenderHandle::spawn() {
        Ok(handle) => (guard, handle),
        Err(e) => panic!("{}", e.report().to_human()),
    }
}

/// Collect events until `f` returns a value or the deadline passes.
fn until<T>(handle: &RenderHandle, mut f: impl FnMut(RenderEvent) -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if let Some(event) = handle.recv_event()
            && let Some(value) = f(event)
        {
            return value;
        }
    }
    panic!("timed out waiting for a render event");
}

#[test]
fn opens_a_document_and_reports_its_pages() {
    let (_gate, render) = handle();
    let doc = render.open(fixture("two-pages.pdf"), None);

    let info = until(&render, |event| match event {
        RenderEvent::Opened { doc: d, info } if d == doc => Some(info),
        RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
        _ => None,
    });

    assert_eq!(info.page_count, 2);
    assert_eq!(info.page_sizes_pt.len(), 2);
    // US Letter, as the fixture declares.
    assert_eq!(info.page_sizes_pt[0], (612.0, 792.0));
    let aspect = info.aspect(0).expect("page 0 exists");
    assert!((aspect - 612.0 / 792.0).abs() < 1e-6);
}

#[test]
fn renders_a_page_to_rgba() {
    let (_gate, render) = handle();
    let doc = render.open(fixture("two-pages.pdf"), None);

    until(&render, |event| match event {
        RenderEvent::Opened { doc: d, .. } if d == doc => Some(()),
        RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
        _ => None,
    });

    render.request(RenderRequest {
        doc,
        page: 0,
        target_width: 600,
        rotation: QuarterTurns(0),
        priority: Priority::Visible,
        generation: 1,
    });

    let page = until(&render, |event| match event {
        RenderEvent::Page(page) => Some(page),
        RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
        _ => None,
    });

    assert_eq!(page.page, 0);
    assert_eq!(page.width, 600);
    // Letter aspect: 600 * 792/612 ≈ 776.
    assert!(
        (775..=778).contains(&page.height),
        "unexpected height {}",
        page.height
    );
    assert_eq!(page.rgba.len() as u32, page.width * page.height * 4);

    // The fixture is black text on white; the top-left corner is page margin.
    assert_eq!(&page.rgba[0..4], &[255, 255, 255, 255]);
    // And the text must have put some dark pixels somewhere.
    assert!(
        page.rgba
            .chunks_exact(4)
            .any(|px| px[0] < 128 && px[1] < 128 && px[2] < 128),
        "rendered page has no dark pixels; text did not draw"
    );
}

#[test]
fn rotating_a_quarter_turn_swaps_the_aspect() {
    let (_gate, render) = handle();
    let doc = render.open(fixture("two-pages.pdf"), None);
    until(&render, |event| match event {
        RenderEvent::Opened { doc: d, .. } if d == doc => Some(()),
        _ => None,
    });

    render.request(RenderRequest {
        doc,
        page: 0,
        target_width: 600,
        rotation: QuarterTurns(1),
        priority: Priority::Visible,
        generation: 1,
    });

    let page = until(&render, |event| match event {
        RenderEvent::Page(page) => Some(page),
        RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
        _ => None,
    });

    // Landscape now: 600 wide, 600 * 612/792 ≈ 464 tall.
    assert_eq!(page.width, 600);
    assert!(page.height < page.width, "rotation did not swap the aspect");
}

#[test]
fn a_missing_file_is_an_io_error_not_a_parse_error() {
    let (_gate, render) = handle();
    let doc = render.open(fixture("does-not-exist.pdf"), None);

    let error = until(&render, |event| match event {
        RenderEvent::Failed { doc: d, error, .. } if d == doc => Some(error),
        _ => None,
    });

    assert_eq!(error.code(), "E_IO");
    assert!(error.path().is_some(), "the error must name the file");
}

#[test]
fn a_page_beyond_the_end_is_rejected_by_range() {
    let (_gate, render) = handle();
    let doc = render.open(fixture("two-pages.pdf"), None);
    until(&render, |event| match event {
        RenderEvent::Opened { doc: d, .. } if d == doc => Some(()),
        _ => None,
    });

    render.request(RenderRequest {
        doc,
        page: 99,
        target_width: 200,
        rotation: QuarterTurns(0),
        priority: Priority::Visible,
        generation: 1,
    });

    let error = until(&render, |event| match event {
        RenderEvent::Failed {
            error,
            page: Some(99),
            ..
        } => Some(error),
        _ => None,
    });

    assert_eq!(error.code(), "E_PAGE_RANGE");
}

#[test]
fn closing_a_document_is_acknowledged() {
    let (_gate, render) = handle();
    let doc = render.open(fixture("two-pages.pdf"), None);
    until(&render, |event| match event {
        RenderEvent::Opened { doc: d, .. } if d == doc => Some(()),
        _ => None,
    });

    render.close(doc);
    until(&render, |event| match event {
        RenderEvent::Closed { doc: d } if d == doc => Some(()),
        _ => None,
    });
}

#[test]
fn extracts_the_text_layer_with_character_boxes() {
    let (_gate, render) = handle();
    let doc = render.open(fixture("linked.pdf"), None);
    until(&render, |event| match event {
        RenderEvent::Opened { doc: d, .. } if d == doc => Some(()),
        RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
        _ => None,
    });

    render.analyze(doc, 0);
    let analysis = until(&render, |event| match event {
        RenderEvent::Analyzed { analysis, .. } => Some(analysis),
        RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
        _ => None,
    });

    let text = analysis.text.text();
    assert!(text.contains("quick brown fox"), "got: {text}");
    assert!(!analysis.text.is_empty());

    // Every character must carry a usable box, or highlighting and selection
    // silently break.
    let f = analysis
        .text
        .chars
        .iter()
        .find(|c| c.ch == 'q')
        .expect("a 'q' on the page");
    assert!(f.rect.right > f.rect.left);
    assert!(f.rect.top > f.rect.bottom);
}

#[test]
fn the_text_layer_carries_font_size_and_face() {
    let (_gate, render) = handle();
    let doc = render.open(fixture("linked.pdf"), None);
    until(&render, |event| match event {
        RenderEvent::Opened { doc: d, .. } if d == doc => Some(()),
        RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
        _ => None,
    });

    render.analyze(doc, 0);
    let analysis = until(&render, |event| match event {
        RenderEvent::Analyzed { analysis, .. } => Some(analysis),
        RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
        _ => None,
    });

    let text = &analysis.text;
    assert!(
        !text.fonts.is_empty(),
        "a page with text is drawn in at least one face"
    );

    // The fixture sets its first line at 18pt and its last at 14pt, so the
    // sizes have to survive extraction for a heading to be tellable from body
    // text later on.
    let sizes: Vec<f32> = text.chars.iter().map(|c| c.size).collect();
    assert!(
        sizes.iter().all(|s| *s > 0.0),
        "every character needs a drawn size"
    );
    let largest = sizes.iter().copied().fold(f32::MIN, f32::max);
    let smallest = sizes.iter().copied().fold(f32::MAX, f32::min);
    assert!(
        largest > smallest,
        "this fixture mixes 18pt and 14pt: got a single size {largest}"
    );

    // Every character must point at a face that exists, or the table is worse
    // than useless.
    for i in 0..text.chars.len() {
        assert!(
            text.face_of(i).is_some(),
            "character {i} points outside the font table"
        );
    }
    assert!(
        !text.fonts[0].name.is_empty(),
        "a face without a name tells us nothing"
    );
}

#[test]
fn search_finds_matches_and_locates_them_on_the_page() {
    let (_gate, render) = handle();
    let doc = render.open(fixture("linked.pdf"), None);
    until(&render, |event| match event {
        RenderEvent::Opened { doc: d, .. } if d == doc => Some(()),
        _ => None,
    });
    render.analyze(doc, 0);
    let analysis = until(&render, |event| match event {
        RenderEvent::Analyzed { analysis, .. } => Some(analysis),
        RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
        _ => None,
    });

    let loose = SearchOptions::default();
    // "fox" appears in "fox", "Fox", "foxy" — case-insensitive, substrings count.
    let hits = analysis.text.search("fox", loose);
    assert!(hits.len() >= 3, "expected several hits, got {}", hits.len());
    assert!(
        hits.iter().all(|h| !h.rects.is_empty()),
        "every hit needs a rectangle"
    );

    let strict = SearchOptions {
        case_sensitive: true,
        whole_word: true,
    };
    let strict_hits = analysis.text.search("fox", strict);
    assert!(
        strict_hits.len() < hits.len(),
        "whole-word + case must narrow the result"
    );
    for hit in &strict_hits {
        assert_eq!(analysis.text.text_range(hit.start, hit.len), "fox");
    }
}

#[test]
fn reads_both_external_and_internal_links() {
    let (_gate, render) = handle();
    let doc = render.open(fixture("linked.pdf"), None);
    until(&render, |event| match event {
        RenderEvent::Opened { doc: d, .. } if d == doc => Some(()),
        _ => None,
    });
    render.analyze(doc, 0);
    let analysis = until(&render, |event| match event {
        RenderEvent::Analyzed { analysis, .. } => Some(analysis),
        RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
        _ => None,
    });

    assert_eq!(
        analysis.links.len(),
        2,
        "fixture has one URI and one internal link"
    );

    let url = analysis
        .links
        .iter()
        .find_map(|l| match &l.target {
            LinkTarget::Url(url) => Some(url.clone()),
            _ => None,
        })
        .expect("a URI link");
    assert_eq!(url, "https://example.org/docs");

    let page = analysis
        .links
        .iter()
        .find_map(|l| match l.target {
            LinkTarget::Page(page) => Some(page),
            _ => None,
        })
        .expect("an internal link");
    assert_eq!(page, 1, "the internal link points at page two");
}

#[test]
fn a_page_with_no_text_reports_empty_rather_than_failing() {
    // The signal that a page is a scan and wants OCR (spec §7).
    let (_gate, render) = handle();
    let doc = render.open(fixture("two-pages.pdf"), None);
    until(&render, |event| match event {
        RenderEvent::Opened { doc: d, .. } if d == doc => Some(()),
        _ => None,
    });

    render.analyze(doc, 1);
    let analysis = until(&render, |event| match event {
        RenderEvent::Analyzed { analysis, .. } => Some(analysis),
        RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
        _ => None,
    });
    assert_eq!(analysis.page, 1);
    assert!(analysis.links.is_empty());
}

#[test]
fn reopening_replaces_the_document_under_the_same_id() {
    // This is the path an edit takes: ypdf-doc rewrites the file in memory and
    // the viewer swaps it in without losing the tab.
    let (_gate, render) = handle();
    let doc = render.open(fixture("two-pages.pdf"), None);

    let info = until(&render, |event| match event {
        RenderEvent::Opened { doc: d, info } if d == doc => Some(info),
        RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
        _ => None,
    });
    assert_eq!(info.page_count, 2);

    let edited = std::fs::read(fixture("many-pages.pdf")).expect("fixture readable");
    render.reopen(doc, edited);

    let info = until(&render, |event| match event {
        RenderEvent::Opened { doc: d, info } if d == doc => Some(info),
        RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
        _ => None,
    });
    assert_eq!(info.page_count, 12, "the id now refers to the new contents");

    // And the new contents actually render.
    render.request(RenderRequest {
        doc,
        page: 11,
        target_width: 200,
        rotation: QuarterTurns(0),
        priority: Priority::Visible,
        generation: 2,
    });
    let page = until(&render, |event| match event {
        RenderEvent::Page(page) => Some(page),
        RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
        _ => None,
    });
    assert_eq!(page.page, 11, "page 12 did not exist before the reopen");
}

#[test]
fn reopening_with_junk_reports_a_parse_failure() {
    let (_gate, render) = handle();
    let doc = render.open(fixture("two-pages.pdf"), None);
    until(&render, |event| match event {
        RenderEvent::Opened { doc: d, .. } if d == doc => Some(()),
        _ => None,
    });

    render.reopen(doc, b"not a pdf at all".to_vec());
    let error = until(&render, |event| match event {
        RenderEvent::Failed { doc: d, error, .. } if d == doc => Some(error),
        _ => None,
    });
    assert!(error.code().starts_with("E_PARSE"), "got {}", error.code());
}

/// The viewer has to be able to open what the Protect dialog writes.
///
/// Two independent implementations meet here: `ypdf-crypt` writes the
/// encryption through lopdf, and PDFium reads it. A file only this project can
/// open would be worse than no encryption feature at all, so both ciphers are
/// checked against the real library.
#[test]
fn pdfium_opens_what_ypdf_crypt_writes() {
    use ypdf_crypt::{Algorithm, EncryptSettings, Permissions};
    use ypdf_doc::Pdf;

    let (_gate, render) = handle();
    let dir = std::env::temp_dir().join("ypdf-render-protected");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");

    for (algorithm, name) in [
        (Algorithm::Aes256, "aes256.pdf"),
        (Algorithm::Aes128, "aes128.pdf"),
    ] {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("fixture opens");
        let bytes = ypdf_crypt::encrypt(
            &mut pdf,
            &EncryptSettings {
                algorithm,
                user_password: "open-me".into(),
                owner_password: "owner".into(),
                permissions: Permissions::read_only(),
            },
        )
        .expect("encrypts");

        let path = dir.join(name);
        std::fs::write(&path, &bytes).expect("writes");

        // Without the password PDFium must refuse, and say which problem it is.
        let doc = render.open(path.clone(), None);
        let error = until(&render, |event| match event {
            RenderEvent::Failed { doc: d, error, .. } if d == doc => Some(error),
            RenderEvent::Opened { doc: d, .. } if d == doc => {
                panic!("{name} opened with no password")
            }
            _ => None,
        });
        assert_eq!(error.code(), "E_PASSWORD_REQUIRED", "{name}");
        render.close(doc);

        // With it, the pages are there.
        let doc = render.open(path, Some("open-me".to_string()));
        let info = until(&render, |event| match event {
            RenderEvent::Opened { doc: d, info } if d == doc => Some(info),
            RenderEvent::Failed { doc: d, error, .. } if d == doc => {
                panic!("{name}: {}", error.report().to_human())
            }
            _ => None,
        });
        assert_eq!(info.page_count, 2, "{name}");
        render.close(doc);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// A watermark has to be visible, and only where it was put.
///
/// The content stream can be well-formed and still draw nothing — a bad matrix,
/// a missing resource, an opacity of zero. The only way to know is to rasterize
/// it and look at the pixels, which is what a reader will do.
#[test]
fn pdfium_draws_the_watermark_ypdf_writes() {
    use ypdf_doc::Pdf;
    use ypdf_watermark::{Placement, Position, Watermark};

    let (_gate, render) = handle();
    let dir = std::env::temp_dir().join("ypdf-render-watermark");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");

    /// Rasterize page 0 and return the pixels.
    fn raster(render: &RenderHandle, path: &std::path::Path) -> (Vec<u8>, u32, u32) {
        let doc = render.open(path.to_path_buf(), None);
        until(render, |event| match event {
            RenderEvent::Opened { doc: d, .. } if d == doc => Some(()),
            RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
            _ => None,
        });
        render.request(RenderRequest {
            doc,
            page: 0,
            target_width: 400,
            rotation: QuarterTurns(0),
            priority: Priority::Visible,
            generation: 0,
        });
        let page = until(render, |event| match event {
            RenderEvent::Page(page) if page.doc == doc && page.page == 0 => Some(page),
            RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
            _ => None,
        });
        render.close(doc);
        (page.rgba.clone(), page.width, page.height)
    }

    /// How many pixels differ between two rasters of the same size.
    fn differing(a: &[u8], b: &[u8]) -> usize {
        a.chunks_exact(4)
            .zip(b.chunks_exact(4))
            .filter(|(left, right)| left != right)
            .count()
    }

    let plain_path = dir.join("plain.pdf");
    let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
    std::fs::write(&plain_path, pdf.to_bytes().expect("serializes")).expect("writes");
    let (plain, width, height) = raster(&render, &plain_path);
    let total = (width as usize) * (height as usize);

    // A solid diagonal stamp must change a real share of the page.
    let stamped_path = dir.join("stamped.pdf");
    let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
    ypdf_watermark::apply(
        &mut pdf,
        &[1],
        &Watermark::text("CONFIDENTIAL")
            .with_opacity(1.0)
            .with_position(Position::Center),
    )
    .expect("stamps");
    std::fs::write(&stamped_path, pdf.to_bytes().expect("serializes")).expect("writes");
    let (stamped, _, _) = raster(&render, &stamped_path);

    let changed = differing(&plain, &stamped);
    assert!(
        changed > total / 100,
        "the watermark drew almost nothing: {changed} of {total} pixels"
    );

    // Page 2 was not asked for, so it must be identical.
    let untouched_path = dir.join("untouched.pdf");
    let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
    ypdf_watermark::apply(&mut pdf, &[2], &Watermark::text("CONFIDENTIAL"))
        .expect("stamps page two");
    std::fs::write(&untouched_path, pdf.to_bytes().expect("serializes")).expect("writes");
    let (other, _, _) = raster(&render, &untouched_path);
    assert_eq!(
        differing(&plain, &other),
        0,
        "stamping page 2 changed page 1"
    );

    // A background watermark still shows: the fixture's page is not opaque.
    let under_path = dir.join("under.pdf");
    let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
    ypdf_watermark::apply(
        &mut pdf,
        &[1],
        &Watermark::text("DRAFT")
            .with_opacity(1.0)
            .with_placement(Placement::Under),
    )
    .expect("stamps");
    std::fs::write(&under_path, pdf.to_bytes().expect("serializes")).expect("writes");
    let (under, _, _) = raster(&render, &under_path);
    assert!(
        differing(&plain, &under) > total / 100,
        "a background watermark drew nothing"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A filled field has to be visible, not just present in the data.
///
/// Writing `/V` without a matching appearance stream is the classic form bug:
/// the value is in the file, and the page prints blank. The only way to know
/// which happened is to rasterize the page.
#[test]
fn pdfium_draws_the_form_values_ypdf_writes() {
    use std::collections::BTreeMap;
    use ypdf_doc::Pdf;

    let (_gate, render) = handle();
    let dir = std::env::temp_dir().join("ypdf-render-forms");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");

    fn raster(render: &RenderHandle, path: &std::path::Path) -> Vec<u8> {
        let doc = render.open(path.to_path_buf(), None);
        until(render, |event| match event {
            RenderEvent::Opened { doc: d, .. } if d == doc => Some(()),
            RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
            _ => None,
        });
        render.request(RenderRequest {
            doc,
            page: 0,
            target_width: 500,
            rotation: QuarterTurns(0),
            priority: Priority::Visible,
            generation: 0,
        });
        let page = until(render, |event| match event {
            RenderEvent::Page(page) if page.doc == doc && page.page == 0 => Some(page),
            RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
            _ => None,
        });
        render.close(doc);
        page.rgba.clone()
    }

    fn differing(a: &[u8], b: &[u8]) -> usize {
        a.chunks_exact(4)
            .zip(b.chunks_exact(4))
            .filter(|(left, right)| left != right)
            .count()
    }

    let blank_path = dir.join("blank.pdf");
    let mut pdf = Pdf::open(fixture("form.pdf")).expect("opens");
    std::fs::write(&blank_path, pdf.to_bytes().expect("serializes")).expect("writes");
    let blank = raster(&render, &blank_path);

    // A filled text field and a ticked checkbox both have to show.
    let filled_path = dir.join("filled.pdf");
    let mut pdf = Pdf::open(fixture("form.pdf")).expect("opens");
    let values = BTreeMap::from([
        ("name".to_string(), "Ada Lovelace".to_string()),
        ("subscribe".to_string(), "yes".to_string()),
    ]);
    let report = ypdf_forms::fill(&mut pdf, &values).expect("fills");
    assert!(report.is_complete(), "{report:?}");
    std::fs::write(&filled_path, pdf.to_bytes().expect("serializes")).expect("writes");
    let filled = raster(&render, &filled_path);

    assert!(
        differing(&blank, &filled) > 200,
        "the filled form drew nothing new"
    );

    // Flattening must not lose what was drawn: the page keeps the same marks
    // with no fields left to edit.
    let flat_path = dir.join("flat.pdf");
    let mut pdf = Pdf::open(&filled_path).expect("opens");
    ypdf_forms::flatten(&mut pdf).expect("flattens");
    std::fs::write(&flat_path, pdf.to_bytes().expect("serializes")).expect("writes");
    let flattened = raster(&render, &flat_path);

    assert!(
        differing(&blank, &flattened) > 200,
        "flattening lost the values"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Annotations have to be visible, and a highlight has to keep the words.
///
/// The second half is the one worth a test: a highlight drawn opaque is a
/// redaction that says "highlight". Multiply blending is what keeps the text
/// underneath readable, and the only way to check is to look at the pixels.
#[test]
fn pdfium_draws_the_annotations_ypdf_writes() {
    use ypdf_annot::{Annotation, Colour, Shape};
    use ypdf_doc::Pdf;

    let (_gate, render) = handle();
    let dir = std::env::temp_dir().join("ypdf-render-annots");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");

    fn raster(render: &RenderHandle, path: &std::path::Path) -> Vec<u8> {
        let doc = render.open(path.to_path_buf(), None);
        until(render, |event| match event {
            RenderEvent::Opened { doc: d, .. } if d == doc => Some(()),
            RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
            _ => None,
        });
        render.request(RenderRequest {
            doc,
            page: 0,
            target_width: 500,
            rotation: QuarterTurns(0),
            priority: Priority::Visible,
            generation: 0,
        });
        let page = until(render, |event| match event {
            RenderEvent::Page(page) if page.doc == doc && page.page == 0 => Some(page),
            RenderEvent::Failed { error, .. } => panic!("{}", error.report().to_human()),
            _ => None,
        });
        render.close(doc);
        page.rgba.clone()
    }

    fn differing(a: &[u8], b: &[u8]) -> usize {
        a.chunks_exact(4)
            .zip(b.chunks_exact(4))
            .filter(|(left, right)| left != right)
            .count()
    }

    /// How many pixels are dark enough to be ink.
    fn ink(rgba: &[u8]) -> usize {
        rgba.chunks_exact(4)
            .filter(|pixel| pixel[0] < 100 && pixel[1] < 100 && pixel[2] < 100)
            .count()
    }

    let plain_path = dir.join("plain.pdf");
    let mut pdf = Pdf::open(fixture("many-pages.pdf")).expect("opens");
    std::fs::write(&plain_path, pdf.to_bytes().expect("serializes")).expect("writes");
    let plain = raster(&render, &plain_path);
    let plain_ink = ink(&plain);
    assert!(plain_ink > 0, "the fixture should have some text on page 1");

    // One of every kind, all on page 1.
    let marked_path = dir.join("marked.pdf");
    let mut pdf = Pdf::open(fixture("many-pages.pdf")).expect("opens");
    for annotation in [
        Annotation::new(
            1,
            Shape::Rectangle {
                rect: [100.0, 300.0, 300.0, 400.0],
                fill: None,
            },
        )
        .with_colour(Colour(1.0, 0.0, 0.0)),
        Annotation::new(
            1,
            Shape::Ink {
                strokes: vec![vec![[100.0, 200.0], [200.0, 260.0], [300.0, 200.0]]],
            },
        ),
        Annotation::new(
            1,
            Shape::Line {
                from: [100.0, 150.0],
                to: [300.0, 150.0],
                arrow: true,
            },
        ),
        Annotation::new(
            1,
            Shape::Stamp {
                rect: [350.0, 100.0, 520.0, 150.0],
                text: "APPROVED".into(),
            },
        ),
    ] {
        ypdf_annot::add(&mut pdf, &annotation).expect("adds");
    }
    std::fs::write(&marked_path, pdf.to_bytes().expect("serializes")).expect("writes");
    let marked = raster(&render, &marked_path);

    assert!(
        differing(&plain, &marked) > 500,
        "the annotations drew almost nothing"
    );

    // A highlight over the page's own text: the words must survive it.
    let highlighted_path = dir.join("highlighted.pdf");
    let mut pdf = Pdf::open(fixture("many-pages.pdf")).expect("opens");
    ypdf_annot::add(
        &mut pdf,
        &Annotation::new(
            1,
            Shape::Highlight {
                // Over the whole upper half of the page, text included.
                quads: vec![[36.0, 600.0, 576.0, 760.0]],
            },
        ),
    )
    .expect("adds");
    std::fs::write(&highlighted_path, pdf.to_bytes().expect("serializes")).expect("writes");
    let highlighted = raster(&render, &highlighted_path);

    assert!(
        differing(&plain, &highlighted) > 500,
        "the highlight drew nothing"
    );
    assert!(
        ink(&highlighted) >= plain_ink,
        "the highlight covered the text it was meant to mark: {} ink before, {} after",
        plain_ink,
        ink(&highlighted)
    );

    let _ = std::fs::remove_dir_all(&dir);
}
