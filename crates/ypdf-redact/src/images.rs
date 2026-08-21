//! Removing pixels from an image that a redaction covers.
//!
//! A scan is one big image, so redacting a scanned document means editing the
//! image itself. Anything less is a black rectangle drawn on top, which is
//! precisely what spec §11 forbids: the pixels underneath survive, and a reader
//! that ignores the drawing order shows them.
//!
//! So the overlapping region is decoded, filled, and re-encoded. Two rules keep
//! that honest:
//!
//! * an image that cannot be decoded is **not** left in place — the caller drops
//!   the draw entirely, because a partial redaction of an image is no redaction;
//! * an image drawn on more than one page is copied first, so redacting page 4
//!   does not silently blank the same logo on page 1.

use flate2::{Compression, write::ZlibEncoder};
use image::{DynamicImage, GenericImageView, RgbImage};
use lopdf::{Document, Object, ObjectId, Stream};
use std::io::Write as _;

use crate::geometry::{Matrix, Rect};

/// What happened to one image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Pixels inside the redaction were replaced.
    Redacted,
    /// The image could not be decoded, so the caller must drop the draw.
    Undecodable,
    /// The redaction and the image do not actually overlap.
    NoOverlap,
}

/// Black out the part of `image_id` that `redaction` covers.
///
/// `matrix` is the transform that placed the image on the page, so the unit
/// square it occupies maps to `bounds`.
pub fn redact_region(
    document: &mut Document,
    image_id: ObjectId,
    matrix: Matrix,
    redaction: Rect,
    bounds: Rect,
) -> Outcome {
    let Some(overlap) = bounds.intersection(redaction) else {
        return Outcome::NoOverlap;
    };
    let Some(inverse) = matrix.invert() else {
        // Scaled to nothing: it draws nothing, so there is nothing to remove.
        return Outcome::NoOverlap;
    };

    let Some(stream) = document
        .get_object(image_id)
        .ok()
        .and_then(|object| object.as_stream().ok())
        .cloned()
    else {
        return Outcome::Undecodable;
    };

    let Some(mut decoded) = decode(&stream) else {
        return Outcome::Undecodable;
    };

    let (width, height) = decoded.dimensions();
    if width == 0 || height == 0 {
        return Outcome::Undecodable;
    }

    // The overlap, in the image's own pixel coordinates. Image space runs from
    // the top down, page space from the bottom up, so y is flipped.
    let corners = [
        inverse.apply(overlap.x0, overlap.y0),
        inverse.apply(overlap.x1, overlap.y0),
        inverse.apply(overlap.x1, overlap.y1),
        inverse.apply(overlap.x0, overlap.y1),
    ];
    let Some(unit) = Rect::around(&corners) else {
        return Outcome::NoOverlap;
    };

    #[expect(clippy::cast_precision_loss, reason = "pixel counts fit in f32")]
    let (fw, fh) = (width as f32, height as f32);
    let x0 = (unit.x0 * fw).floor().max(0.0);
    let x1 = (unit.x1 * fw).ceil().min(fw);
    // Flip: the top of the image is v = 1 in unit space.
    let y0 = ((1.0 - unit.y1) * fh).floor().max(0.0);
    let y1 = ((1.0 - unit.y0) * fh).ceil().min(fh);

    if x1 <= x0 || y1 <= y0 {
        return Outcome::NoOverlap;
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to the image above"
    )]
    let (x0, x1, y0, y1) = (x0 as u32, x1 as u32, y0 as u32, y1 as u32);

    let mut rgb = decoded.to_rgb8();
    for y in y0..y1.min(height) {
        for x in x0..x1.min(width) {
            rgb.put_pixel(x, y, image::Rgb([0, 0, 0]));
        }
    }
    decoded = DynamicImage::ImageRgb8(rgb);

    let Some(bytes) = encode(&decoded) else {
        return Outcome::Undecodable;
    };

    let mut dict = stream.dict.clone();
    dict.set("Width", i64::from(decoded.width()));
    dict.set("Height", i64::from(decoded.height()));
    dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
    dict.set("BitsPerComponent", 8_i64);
    dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
    dict.set("Length", i64::try_from(bytes.len()).unwrap_or(i64::MAX));
    // These describe the pixels that were there before.
    dict.remove(b"DecodeParms");
    dict.remove(b"Decode");
    // A soft mask would let the original show through the black.
    dict.remove(b"SMask");
    dict.remove(b"Mask");

    document
        .objects
        .insert(image_id, Object::Stream(Stream::new(dict, bytes)));
    Outcome::Redacted
}

/// Copy an image object, so editing it on one page does not change another.
pub fn duplicate(document: &mut Document, image_id: ObjectId) -> Option<ObjectId> {
    let object = document.get_object(image_id).ok()?.clone();
    Some(document.add_object(object))
}

/// How many pages draw this image.
///
/// An image used once can be edited where it is; one used twice has to be
/// copied first, or redacting page 4 would blank the same logo on page 1.
pub fn draw_count(document: &Document, image_id: ObjectId) -> usize {
    document
        .objects
        .values()
        .filter(|object| {
            let Ok(dict) = object.as_dict() else {
                return false;
            };
            let Ok(resources) = dict.get(b"Resources") else {
                return false;
            };
            let resources = match resources {
                Object::Reference(id) => match document.get_dictionary(*id) {
                    Ok(dict) => dict,
                    Err(_) => return false,
                },
                Object::Dictionary(dict) => dict,
                _ => return false,
            };
            resources
                .get(b"XObject")
                .and_then(Object::as_dict)
                .is_ok_and(|xobjects| {
                    xobjects
                        .iter()
                        .any(|(_, value)| value.as_reference().ok() == Some(image_id))
                })
        })
        .count()
}

/// Decode an image stream into pixels.
///
/// The same formats `ypdf-optimize` understands, and for the same reason:
/// anything else is refused rather than guessed at.
fn decode(stream: &Stream) -> Option<DynamicImage> {
    let filters: Vec<String> = match stream.dict.get(b"Filter") {
        Ok(Object::Name(name)) => vec![String::from_utf8_lossy(name).into_owned()],
        Ok(Object::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_name().ok())
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect(),
        _ => Vec::new(),
    };

    let width = u32::try_from(stream.dict.get(b"Width").and_then(Object::as_i64).ok()?).ok()?;
    let height = u32::try_from(stream.dict.get(b"Height").and_then(Object::as_i64).ok()?).ok()?;

    match filters.last().map(String::as_str).unwrap_or("") {
        "DCTDecode" => {
            image::load_from_memory_with_format(&stream.content, image::ImageFormat::Jpeg).ok()
        }
        "FlateDecode" | "LZWDecode" | "RunLengthDecode" | "" => {
            let raw = stream.decompressed_content().ok()?;
            let bits = stream
                .dict
                .get(b"BitsPerComponent")
                .and_then(Object::as_i64)
                .unwrap_or(8);
            if bits != 8 {
                return None;
            }

            let colour = stream
                .dict
                .get(b"ColorSpace")
                .and_then(Object::as_name)
                .map(|name| String::from_utf8_lossy(name).into_owned())
                .unwrap_or_default();
            let pixels = (width as usize) * (height as usize);

            match colour.as_str() {
                "DeviceRGB" | "CalRGB" => {
                    RgbImage::from_raw(width, height, raw.get(..pixels * 3)?.to_vec())
                        .map(DynamicImage::ImageRgb8)
                }
                "DeviceGray" | "CalGray" => {
                    image::GrayImage::from_raw(width, height, raw.get(..pixels)?.to_vec())
                        .map(DynamicImage::ImageLuma8)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Store the edited pixels losslessly.
///
/// Deflated RGB rather than JPEG: re-encoding a redacted region as JPEG would
/// leave ringing artefacts around the black box that trace the shape of what
/// was removed.
fn encode(image: &DynamicImage) -> Option<Vec<u8>> {
    let rgb = image.to_rgb8();
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(rgb.as_raw()).ok()?;
    encoder.finish().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    /// A document with one image drawn over the whole of a 100×100 page.
    fn document() -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.7");

        let mut pixels = RgbImage::new(10, 10);
        for pixel in pixels.pixels_mut() {
            *pixel = image::Rgb([255, 255, 255]);
        }
        let mut raw = Vec::new();
        for pixel in pixels.pixels() {
            raw.extend_from_slice(&pixel.0);
        }
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&raw).expect("compresses");
        let content = encoder.finish().expect("finishes");

        let image_id = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 10_i64,
                "Height" => 10_i64,
                "ColorSpace" => "DeviceRGB",
                "BitsPerComponent" => 8_i64,
                "Filter" => "FlateDecode",
            },
            content,
        ));
        (doc, image_id)
    }

    fn pixels(document: &Document, image_id: ObjectId) -> DynamicImage {
        let stream = document
            .get_object(image_id)
            .and_then(Object::as_stream)
            .expect("a stream");
        decode(stream).expect("decodes")
    }

    #[test]
    fn the_covered_pixels_are_replaced_in_the_image_itself() {
        // Not a rectangle drawn on top: the stored pixels change.
        let (mut doc, image_id) = document();
        let matrix = Matrix::new(100.0, 0.0, 0.0, 100.0, 0.0, 0.0);

        let outcome = redact_region(
            &mut doc,
            image_id,
            matrix,
            // The top half of the page.
            Rect::new(0.0, 50.0, 100.0, 100.0),
            Rect::new(0.0, 0.0, 100.0, 100.0),
        );
        assert_eq!(outcome, Outcome::Redacted);

        let after = pixels(&doc, image_id);
        // Image space runs from the top, so the top half is rows 0-4.
        assert_eq!(after.get_pixel(5, 1).0[..3], [0, 0, 0], "top must be gone");
        assert_eq!(
            after.get_pixel(5, 8).0[..3],
            [255, 255, 255],
            "the bottom must be untouched"
        );
    }

    #[test]
    fn a_redaction_that_misses_the_image_changes_nothing() {
        let (mut doc, image_id) = document();
        let before = pixels(&doc, image_id).to_rgb8().into_raw();

        let outcome = redact_region(
            &mut doc,
            image_id,
            Matrix::new(50.0, 0.0, 0.0, 50.0, 0.0, 0.0),
            Rect::new(200.0, 200.0, 300.0, 300.0),
            Rect::new(0.0, 0.0, 50.0, 50.0),
        );

        assert_eq!(outcome, Outcome::NoOverlap);
        assert_eq!(pixels(&doc, image_id).to_rgb8().into_raw(), before);
    }

    #[test]
    fn an_image_the_crate_cannot_decode_is_reported_rather_than_left_alone() {
        // Leaving it would be a redaction that did not happen.
        let mut doc = Document::with_version("1.7");
        let image_id = doc.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 10_i64,
                "Height" => 10_i64,
                "ColorSpace" => "DeviceCMYK",
                "BitsPerComponent" => 8_i64,
                "Filter" => "JPXDecode",
            },
            vec![0; 64],
        ));

        let outcome = redact_region(
            &mut doc,
            image_id,
            Matrix::new(100.0, 0.0, 0.0, 100.0, 0.0, 0.0),
            Rect::new(0.0, 0.0, 50.0, 50.0),
            Rect::new(0.0, 0.0, 100.0, 100.0),
        );
        assert_eq!(outcome, Outcome::Undecodable);
    }

    #[test]
    fn a_soft_mask_is_dropped_so_nothing_shows_through_the_black() {
        let (mut doc, image_id) = document();
        if let Ok(Object::Stream(stream)) = doc.get_object_mut(image_id) {
            stream.dict.set("SMask", Object::Reference((99, 0)));
        }

        redact_region(
            &mut doc,
            image_id,
            Matrix::new(100.0, 0.0, 0.0, 100.0, 0.0, 0.0),
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Rect::new(0.0, 0.0, 100.0, 100.0),
        );

        let stream = doc
            .get_object(image_id)
            .and_then(Object::as_stream)
            .expect("a stream");
        assert!(stream.dict.get(b"SMask").is_err());
    }

    #[test]
    fn a_copy_is_a_separate_object() {
        let (mut doc, image_id) = document();
        let copy = duplicate(&mut doc, image_id).expect("copies");
        assert_ne!(copy, image_id);

        redact_region(
            &mut doc,
            copy,
            Matrix::new(100.0, 0.0, 0.0, 100.0, 0.0, 0.0),
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Rect::new(0.0, 0.0, 100.0, 100.0),
        );

        // The original is untouched: redacting one page must not blank another.
        assert_eq!(
            pixels(&doc, image_id).get_pixel(5, 5).0[..3],
            [255, 255, 255]
        );
        assert_eq!(pixels(&doc, copy).get_pixel(5, 5).0[..3], [0, 0, 0]);
    }
}
