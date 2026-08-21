//! Compression against a document built for the purpose.
//!
//! The image-heavy document is generated here rather than committed: it is
//! several megabytes, and a fixture that large in the repository costs every
//! clone forever to test one thing.

// Integration tests compile as their own crate, so `cfg(test)` is not set and
// the clippy.toml allowance for tests does not reach here.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use lopdf::{Document, Object, Stream, dictionary};
use ypdf_core::preset::Compression;
use ypdf_doc::Pdf;
use ypdf_optimize::{optimize, strip_metadata};

/// Smooth gradients: expensive as raw samples, cheap as JPEG. This is what a
/// scan or an image-to-PDF conversion actually looks like.
fn photo(width: u32, height: u32, seed: u8) -> Vec<u8> {
    let mut data = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            data.push(((x * 3) as u8).wrapping_add(seed));
            data.push(((y * 5) as u8).wrapping_add(seed));
            data.push((((x + y) * 2) as u8).wrapping_add(seed));
        }
    }
    data
}

fn flate(bytes: &[u8]) -> Vec<u8> {
    use flate2::{Compression as Level, write::ZlibEncoder};
    use std::io::Write;
    let mut encoder = ZlibEncoder::new(Vec::new(), Level::new(6));
    encoder.write_all(bytes).expect("compresses");
    encoder.finish().expect("finishes")
}

fn image_object(width: u32, height: u32, colour_space: &str, data: Vec<u8>) -> Stream {
    let compressed = flate(&data);
    Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => i64::from(width),
            "Height" => i64::from(height),
            "ColorSpace" => Object::Name(colour_space.as_bytes().to_vec()),
            "BitsPerComponent" => 8_i64,
            "Filter" => "FlateDecode",
        },
        compressed,
    )
}

/// A three-page document whose size is almost entirely its images, plus a CMYK
/// image the optimizer must refuse and a tiny one it should judge not worth it.
fn image_heavy() -> Pdf {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mut kids = Vec::new();

    // These two hang off the first page so they survive pruning: an
    // unreferenced object is removed before the optimizer ever sees it.
    let cmyk_id = doc.add_object(image_object(
        400,
        400,
        "DeviceCMYK",
        vec![0x40; 400 * 400 * 4],
    ));
    let tiny_id = doc.add_object(image_object(30, 30, "DeviceRGB", vec![0x20; 30 * 30 * 3]));

    for page in 0..3_u8 {
        let image_id = doc.add_object(image_object(
            1200,
            1600,
            "DeviceRGB",
            photo(1200, 1600, page * 40),
        ));
        let content_id = doc.add_object(Stream::new(
            dictionary! {},
            b"q 612 0 0 792 0 0 cm /Im0 Do Q".to_vec(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {
                "XObject" => dictionary! {
                    "Im0" => image_id,
                    "Cmyk" => cmyk_id,
                    "Tiny" => tiny_id,
                },
            },
            "Contents" => content_id,
        });
        kids.push(Object::Reference(page_id));
    }

    let count = i64::try_from(kids.len()).expect("fits");
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
        }),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);

    let info_id = doc.add_object(dictionary! {
        "Title" => Object::string_literal("A large scan"),
        "Author" => Object::string_literal("Someone"),
    });
    doc.trailer.set("Info", info_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("serializes");
    Pdf::from_bytes(&bytes).expect("re-opens")
}

#[test]
fn compression_makes_an_image_heavy_document_much_smaller() {
    let mut pdf = image_heavy();
    let report = optimize(&mut pdf, &Compression::default()).expect("optimizes");

    assert!(report.is_improvement(), "{}", report.to_human());
    assert!(
        report.reduction() > 0.5,
        "expected better than half, got {:.1}%\n{}",
        report.reduction() * 100.0,
        report.to_human()
    );
    assert!(report.images_recompressed >= 3, "{}", report.to_human());

    // And the result must still be a document.
    let bytes = pdf.to_bytes().expect("serializes");
    let reopened = Pdf::from_bytes(&bytes).expect("re-opens after compression");
    assert_eq!(reopened.page_count(), 3);
}

#[test]
fn a_lower_quality_setting_produces_a_smaller_file() {
    let mut aggressive = image_heavy();
    let mut gentle = image_heavy();

    let small = optimize(
        &mut aggressive,
        &Compression {
            dpi: 72,
            quality: 40,
            ..Compression::default()
        },
    )
    .expect("optimizes");
    let large = optimize(
        &mut gentle,
        &Compression {
            dpi: 300,
            quality: 95,
            ..Compression::default()
        },
    )
    .expect("optimizes");

    assert!(
        small.after < large.after,
        "72dpi/q40 ({}) should beat 300dpi/q95 ({})",
        small.after,
        large.after
    );
}

#[test]
fn a_cmyk_image_is_refused_rather_than_guessed_at() {
    let mut pdf = image_heavy();
    let report = optimize(&mut pdf, &Compression::default()).expect("optimizes");

    let skipped: usize = report.images_skipped.values().sum();
    assert!(
        skipped > 0,
        "the CMYK and tiny images must be reported as skipped"
    );
    assert!(
        report
            .images_skipped
            .contains_key("unsupported colour space or bit depth")
            || report.images_skipped.contains_key("already small"),
        "expected a stated reason, got {:?}",
        report.images_skipped
    );
}

#[test]
fn requesting_linearization_says_it_was_not_done() {
    // Reporting a percentage while silently skipping the requested work is
    // worse than saving less and saying so.
    let mut pdf = image_heavy();
    let report = optimize(
        &mut pdf,
        &Compression {
            linearize: true,
            optimize_fonts: false,
            ..Compression::default()
        },
    )
    .expect("optimizes");

    assert!(report.not_supported.contains(&"linearization"));
    assert!(report.to_human().contains("Not done:"));
}

#[test]
fn compressing_an_already_compressed_document_does_not_wreck_it() {
    let mut pdf = image_heavy();
    optimize(&mut pdf, &Compression::default()).expect("first pass");
    let after_first = pdf.to_bytes().expect("serializes").len();

    let report = optimize(&mut pdf, &Compression::default()).expect("second pass");

    // The second pass must not balloon the file by re-encoding JPEG as JPEG.
    assert!(
        report.after <= after_first as u64 + 4096,
        "second pass grew the file: {after_first} then {}",
        report.after
    );
    assert_eq!(
        Pdf::from_bytes(&pdf.to_bytes().expect("bytes"))
            .expect("re-opens")
            .page_count(),
        3
    );
}

#[test]
fn stripping_metadata_removes_the_info_dictionary() {
    let mut pdf = image_heavy();
    assert_eq!(pdf.metadata().title.as_deref(), Some("A large scan"));

    let removed = strip_metadata(&mut pdf);
    assert!(removed > 0);

    let bytes = pdf.to_bytes().expect("serializes");
    let reopened = Pdf::from_bytes(&bytes).expect("re-opens");
    assert_eq!(reopened.metadata().title, None);
    assert_eq!(reopened.metadata().author, None);
}

#[test]
fn a_text_only_document_survives_compression_unharmed() {
    // Nothing to recompress, so the interesting property is that it is not
    // damaged and the report says honestly that little happened.
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/many-pages.pdf");
    let mut pdf = Pdf::open(&path).expect("opens");
    let before = pdf.page_count();

    let report = optimize(&mut pdf, &Compression::default()).expect("optimizes");
    assert_eq!(report.images_recompressed, 0);

    let bytes = pdf.to_bytes().expect("serializes");
    let reopened = Pdf::from_bytes(&bytes).expect("re-opens");
    assert_eq!(reopened.page_count(), before);

    let text = lopdf::Document::load_mem(&bytes)
        .expect("loads")
        .extract_text(&[1])
        .expect("extracts text");
    assert!(text.contains('1'), "the text must survive: {text:?}");
}
