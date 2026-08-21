//! Turning an image file into a PDF image XObject.
//!
//! Two paths, and the split matters for quality:
//!
//! * a **JPEG** is already a DCTDecode stream, so its bytes go in untouched —
//!   decoding and re-encoding one would lose quality for nothing;
//! * anything else is decoded and stored as deflated RGB, which is lossless.
//!
//! Transparency is carried through as an `/SMask`. A logo watermark is mostly
//! transparent, and a logo pasted onto a white rectangle over someone's page is
//! not a watermark — it is a hole in the document.

use flate2::{Compression, write::ZlibEncoder};
use lopdf::{Document, ObjectId, Stream, dictionary};
use std::io::Write as _;
use ypdf_core::{Error, Result};

/// How the image was stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    /// Kept as the original JPEG.
    Jpeg,
    /// Decoded and stored as deflated RGB.
    Rgb,
    /// Deflated RGB with an alpha mask.
    RgbWithAlpha,
}

/// An image placed in a document.
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    /// The XObject.
    pub id: ObjectId,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// How it was stored.
    pub kind: ImageKind,
}

/// Add `bytes` to `document` as an image XObject.
pub fn place(document: &mut Document, bytes: &[u8]) -> Result<Placed> {
    let format = image::guess_format(bytes).map_err(|e| Error::Config {
        detail: format!("this file is not an image yPDF can read: {e}"),
        source_path: None,
    })?;

    let decoded =
        image::load_from_memory_with_format(bytes, format).map_err(|e| Error::Config {
            detail: format!("the image could not be decoded: {e}"),
            source_path: None,
        })?;

    let width = decoded.width();
    let height = decoded.height();
    if width == 0 || height == 0 {
        return Err(Error::Config {
            detail: "the image has no pixels".into(),
            source_path: None,
        });
    }

    // A JPEG has no alpha, so its bytes can go straight in.
    if format == image::ImageFormat::Jpeg {
        let id = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => i64::from(width),
                "Height" => i64::from(height),
                "ColorSpace" => "DeviceRGB",
                "BitsPerComponent" => 8_i64,
                "Filter" => "DCTDecode",
            },
            bytes.to_vec(),
        ));
        return Ok(Placed {
            id,
            width,
            height,
            kind: ImageKind::Jpeg,
        });
    }

    let rgba = decoded.to_rgba8();
    let has_alpha = rgba.pixels().any(|pixel| pixel.0[3] != 255);

    let mut colour = Vec::with_capacity((width as usize) * (height as usize) * 3);
    let mut alpha = Vec::with_capacity((width as usize) * (height as usize));
    for pixel in rgba.pixels() {
        colour.extend_from_slice(&pixel.0[..3]);
        alpha.push(pixel.0[3]);
    }

    let mut dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => i64::from(width),
        "Height" => i64::from(height),
        "ColorSpace" => "DeviceRGB",
        "BitsPerComponent" => 8_i64,
        "Filter" => "FlateDecode",
    };

    if has_alpha {
        // Without this the transparent parts of a logo are drawn as black.
        let mask = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => i64::from(width),
                "Height" => i64::from(height),
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8_i64,
                "Filter" => "FlateDecode",
            },
            deflate(&alpha)?,
        ));
        dict.set("SMask", mask);
    }

    let id = document.add_object(Stream::new(dict, deflate(&colour)?));
    Ok(Placed {
        id,
        width,
        height,
        kind: if has_alpha {
            ImageKind::RgbWithAlpha
        } else {
            ImageKind::Rgb
        },
    })
}

fn deflate(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(bytes).map_err(|e| Error::Backend {
        backend: "flate2",
        detail: e.to_string(),
    })?;
    encoder.finish().map_err(|e| Error::Backend {
        backend: "flate2",
        detail: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::Object;

    fn png(width: u32, height: u32, alpha: u8) -> Vec<u8> {
        let mut buffer = image::RgbaImage::new(width, height);
        for pixel in buffer.pixels_mut() {
            *pixel = image::Rgba([200, 30, 30, alpha]);
        }
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(buffer)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("encodes");
        out
    }

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let buffer = image::RgbImage::new(width, height);
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(buffer)
            .write_to(
                &mut std::io::Cursor::new(&mut out),
                image::ImageFormat::Jpeg,
            )
            .expect("encodes");
        out
    }

    #[test]
    fn a_transparent_png_gets_a_soft_mask() {
        // Without it, every transparent pixel of a logo is drawn black.
        let mut doc = Document::with_version("1.7");
        let placed = place(&mut doc, &png(8, 8, 128)).expect("places");

        assert_eq!(placed.kind, ImageKind::RgbWithAlpha);
        let dict = doc
            .get_object(placed.id)
            .and_then(Object::as_stream)
            .expect("a stream");
        assert!(dict.dict.get(b"SMask").is_ok());
    }

    #[test]
    fn an_opaque_png_does_not_carry_a_mask_it_does_not_need() {
        let mut doc = Document::with_version("1.7");
        let placed = place(&mut doc, &png(8, 8, 255)).expect("places");

        assert_eq!(placed.kind, ImageKind::Rgb);
        let stream = doc
            .get_object(placed.id)
            .and_then(Object::as_stream)
            .expect("a stream");
        assert!(stream.dict.get(b"SMask").is_err());
    }

    #[test]
    fn a_jpeg_goes_in_byte_for_byte() {
        // It is already a DCTDecode stream; a round trip would lose quality to
        // no purpose.
        let bytes = jpeg(16, 16);
        let mut doc = Document::with_version("1.7");
        let placed = place(&mut doc, &bytes).expect("places");

        assert_eq!(placed.kind, ImageKind::Jpeg);
        let stream = doc
            .get_object(placed.id)
            .and_then(Object::as_stream)
            .expect("a stream");
        assert_eq!(stream.content, bytes);
    }

    #[test]
    fn the_dimensions_are_carried_through() {
        let mut doc = Document::with_version("1.7");
        let placed = place(&mut doc, &png(12, 5, 255)).expect("places");
        assert_eq!((placed.width, placed.height), (12, 5));
    }

    #[test]
    fn something_that_is_not_an_image_is_refused_with_a_readable_reason() {
        let mut doc = Document::with_version("1.7");
        let error = place(&mut doc, b"this is not an image").expect_err("refused");
        assert!(error.report().to_human().contains("not an image"));
    }
}
