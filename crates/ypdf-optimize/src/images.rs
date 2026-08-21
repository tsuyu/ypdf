//! Image recompression (spec §4).
//!
//! The rule here is **never make a page worse without making the file
//! smaller**. Every candidate image is decoded, resampled, re-encoded, and then
//! compared: if the new version is not meaningfully smaller, the original is
//! kept. That makes the operation safe to run on a file that has already been
//! optimized, which is the common case.
//!
//! Anything this module does not fully understand is left exactly as it was and
//! counted as skipped. A PDF can carry JPEG 2000, JBIG2, CCITT fax, indexed
//! palettes, CMYK separations, and 1-bit masks; re-encoding one of those from a
//! partial understanding is how a compressor corrupts a document.

use image::{DynamicImage, ImageEncoder, RgbImage, imageops::FilterType};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

/// Why an image was left alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skipped {
    /// A filter this module does not decode.
    UnsupportedFilter,
    /// A colour space or bit depth it does not decode.
    UnsupportedColour,
    /// The image is a stencil mask; re-encoding one as JPEG destroys it.
    Mask,
    /// Already small enough that the work is not worth it.
    TooSmall,
    /// Re-encoding produced something no smaller.
    NoGain,
    /// The image could not be decoded at all.
    Undecodable,
}

impl Skipped {
    /// A short explanation for the report.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::UnsupportedFilter => "unsupported filter",
            Self::UnsupportedColour => "unsupported colour space or bit depth",
            Self::Mask => "stencil mask",
            Self::TooSmall => "already small",
            Self::NoGain => "no size gain",
            Self::Undecodable => "could not be decoded",
        }
    }
}

/// What happened to one image.
#[derive(Clone, Copy, Debug)]
pub enum Outcome {
    /// It was replaced; the sizes are before and after, in bytes.
    Recompressed { before: usize, after: usize },
    /// It was left alone.
    Skipped(Skipped),
}

/// Images below this many bytes are not worth the work: the saving is smaller
/// than the noise, and every re-encode is a chance to lose quality.
const MIN_WORTHWHILE_BYTES: usize = 16 * 1024;

/// A re-encode must save at least this fraction to be worth taking.
const MIN_GAIN: f32 = 0.10;

/// Settings for one pass over the images.
#[derive(Clone, Copy, Debug)]
pub struct ImageSettings {
    /// Target resolution. Images above it are downsampled.
    pub dpi: u32,
    /// JPEG quality, 1-100.
    pub quality: u8,
    /// Re-encode losslessly: resample and re-deflate, but never to JPEG.
    pub lossless: bool,
}

/// Recompress every image in the document that can safely be recompressed.
///
/// Returns one outcome per image encountered.
pub fn optimize_images(doc: &mut Document, settings: ImageSettings) -> Vec<Outcome> {
    let candidates: Vec<ObjectId> = doc
        .objects
        .iter()
        .filter(|(_, object)| is_image(object))
        .map(|(id, _)| *id)
        .collect();

    // Page geometry decides how many pixels an image actually needs; without it
    // "150 dpi" has no meaning. Images used by no page keep their size.
    let displayed = displayed_widths(doc);

    let mut outcomes = Vec::with_capacity(candidates.len());
    for id in candidates {
        let Some(stream) = doc
            .objects
            .get(&id)
            .and_then(|o| o.as_stream().ok())
            .cloned()
        else {
            continue;
        };

        let outcome = match recompress(&stream, displayed.get(&id).copied(), settings) {
            Ok(new_stream) => {
                let before = stream.content.len();
                let after = new_stream.content.len();
                doc.objects.insert(id, Object::Stream(new_stream));
                Outcome::Recompressed { before, after }
            }
            Err(reason) => Outcome::Skipped(reason),
        };
        outcomes.push(outcome);
    }

    outcomes
}

pub(crate) fn is_image(object: &Object) -> bool {
    object.as_stream().is_ok_and(|stream| {
        stream
            .dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .is_ok_and(|s| s == b"Image")
    })
}

/// How wide each image is actually drawn, in points, taken from the page that
/// uses it.
///
/// Only the simple case is handled: an image drawn by a page whose media box
/// gives its width. That is enough to stop a full-page scan being downsampled
/// as though it were a thumbnail, which is the mistake that matters.
fn displayed_widths(doc: &Document) -> std::collections::HashMap<ObjectId, f32> {
    let mut widths = std::collections::HashMap::new();

    for object in doc.objects.values() {
        let Ok(dict) = object.as_dict() else { continue };
        if dict
            .get(b"Type")
            .and_then(Object::as_name)
            .is_ok_and(|t| t == b"Page")
        {
            let page_width = dict
                .get(b"MediaBox")
                .and_then(Object::as_array)
                .ok()
                .and_then(|b| Some(b.get(2)?.as_float().ok()? - b.first()?.as_float().ok()?))
                .unwrap_or(612.0);

            for id in xobject_ids(doc, dict) {
                // Assume the image spans the page. Overestimating the display
                // size only means keeping more pixels than strictly needed,
                // which is the safe direction to be wrong in.
                widths.insert(id, page_width);
            }
        }
    }

    widths
}

pub(crate) fn xobject_ids(doc: &Document, page: &Dictionary) -> Vec<ObjectId> {
    let resources = match page.get(b"Resources") {
        Ok(Object::Reference(id)) => doc.get_dictionary(*id).ok().cloned(),
        Ok(Object::Dictionary(dict)) => Some(dict.clone()),
        _ => None,
    };
    let Some(resources) = resources else {
        return Vec::new();
    };

    let xobjects = match resources.get(b"XObject") {
        Ok(Object::Reference(id)) => doc.get_dictionary(*id).ok().cloned(),
        Ok(Object::Dictionary(dict)) => Some(dict.clone()),
        _ => None,
    };
    let Some(xobjects) = xobjects else {
        return Vec::new();
    };

    xobjects
        .iter()
        .filter_map(|(_, value)| value.as_reference().ok())
        .collect()
}

/// Re-encode one image, or explain why it was left alone.
fn recompress(
    stream: &Stream,
    displayed_width_pt: Option<f32>,
    settings: ImageSettings,
) -> Result<Stream, Skipped> {
    let dict = &stream.dict;

    if dict
        .get(b"ImageMask")
        .and_then(Object::as_bool)
        .unwrap_or(false)
    {
        return Err(Skipped::Mask);
    }
    if stream.content.len() < MIN_WORTHWHILE_BYTES {
        return Err(Skipped::TooSmall);
    }

    let width = dict
        .get(b"Width")
        .and_then(Object::as_i64)
        .map_err(|_| Skipped::UnsupportedColour)?;
    let height = dict
        .get(b"Height")
        .and_then(Object::as_i64)
        .map_err(|_| Skipped::UnsupportedColour)?;
    let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
        return Err(Skipped::UnsupportedColour);
    };

    let image = decode(stream, width, height)?;

    // Downsample only when the image carries more pixels than the page can show
    // at the target resolution.
    let target_width = displayed_width_pt
        .map(|points| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped"
            )]
            let pixels = ((points / 72.0) * settings.dpi as f32).round().max(1.0) as u32;
            pixels
        })
        .unwrap_or(width)
        .min(width);

    let resized = if target_width < width {
        let scale = f64::from(target_width) / f64::from(width);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped"
        )]
        let target_height = ((f64::from(height) * scale).round().max(1.0)) as u32;
        // Lanczos costs more than the alternatives and is the only one that
        // keeps small text in a scan legible after a 2x reduction.
        image.resize_exact(target_width, target_height, FilterType::Lanczos3)
    } else if settings.lossless {
        // Nothing to resample and nothing lossy allowed: re-deflating rarely
        // beats what the producer already did.
        return Err(Skipped::NoGain);
    } else {
        image
    };

    let encoded = if settings.lossless {
        encode_flate(&resized)
    } else {
        encode_jpeg(&resized, settings.quality)
    }
    .ok_or(Skipped::Undecodable)?;

    #[expect(clippy::cast_precision_loss, reason = "size comparison only")]
    let gain = 1.0 - (encoded.data.len() as f32 / stream.content.len() as f32);
    if gain < MIN_GAIN {
        return Err(Skipped::NoGain);
    }

    let mut new_dict = dict.clone();
    new_dict.set("Width", i64::from(resized.width()));
    new_dict.set("Height", i64::from(resized.height()));
    new_dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
    new_dict.set("BitsPerComponent", 8_i64);
    new_dict.set("Filter", Object::Name(encoded.filter.to_vec()));
    new_dict.set(
        "Length",
        i64::try_from(encoded.data.len()).unwrap_or(i64::MAX),
    );
    // These describe the old pixels and would be wrong for the new ones.
    new_dict.remove(b"DecodeParms");
    new_dict.remove(b"Decode");

    Ok(Stream::new(new_dict, encoded.data))
}

/// Decode an image stream into pixels.
pub(crate) fn decode(stream: &Stream, width: u32, height: u32) -> Result<DynamicImage, Skipped> {
    let filters = filter_names(&stream.dict);
    let last = filters.last().map(String::as_str).unwrap_or("");

    match last {
        // Already JPEG: hand the bytes to the decoder as they are.
        "DCTDecode" => {
            image::load_from_memory_with_format(&stream.content, image::ImageFormat::Jpeg)
                .map_err(|_| Skipped::Undecodable)
        }

        // Raw samples behind a general-purpose compressor.
        "FlateDecode" | "LZWDecode" | "RunLengthDecode" | "" => {
            let raw = stream
                .decompressed_content()
                .map_err(|_| Skipped::Undecodable)?;
            raw_to_image(&stream.dict, &raw, width, height)
        }

        // JPX, JBIG2, CCITTFax: each needs its own decoder, and guessing at one
        // is how a compressor destroys a document.
        _ => Err(Skipped::UnsupportedFilter),
    }
}

/// Build an image from raw PDF samples.
fn raw_to_image(
    dict: &Dictionary,
    raw: &[u8],
    width: u32,
    height: u32,
) -> Result<DynamicImage, Skipped> {
    let bits = dict
        .get(b"BitsPerComponent")
        .and_then(Object::as_i64)
        .unwrap_or(8);
    if bits != 8 {
        return Err(Skipped::UnsupportedColour);
    }

    let colour = dict
        .get(b"ColorSpace")
        .and_then(Object::as_name)
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .unwrap_or_default();

    let pixels = (width as usize) * (height as usize);
    match colour.as_str() {
        "DeviceRGB" | "CalRGB" => {
            if raw.len() < pixels * 3 {
                return Err(Skipped::Undecodable);
            }
            RgbImage::from_raw(width, height, raw[..pixels * 3].to_vec())
                .map(DynamicImage::ImageRgb8)
                .ok_or(Skipped::Undecodable)
        }
        "DeviceGray" | "CalGray" => {
            if raw.len() < pixels {
                return Err(Skipped::Undecodable);
            }
            image::GrayImage::from_raw(width, height, raw[..pixels].to_vec())
                .map(DynamicImage::ImageLuma8)
                .ok_or(Skipped::Undecodable)
        }
        // Indexed palettes, CMYK separations, ICC-based spaces: each needs its
        // own conversion, and a wrong guess changes every colour on the page.
        _ => Err(Skipped::UnsupportedColour),
    }
}

pub(crate) fn filter_names(dict: &Dictionary) -> Vec<String> {
    match dict.get(b"Filter") {
        Ok(Object::Name(name)) => vec![String::from_utf8_lossy(name).into_owned()],
        Ok(Object::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_name().ok())
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect(),
        _ => Vec::new(),
    }
}

struct Encoded {
    data: Vec<u8>,
    filter: &'static [u8],
}

fn encode_jpeg(image: &DynamicImage, quality: u8) -> Option<Encoded> {
    let rgb = image.to_rgb8();
    let mut data = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, quality.clamp(1, 100))
        .write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .ok()?;
    Some(Encoded {
        data,
        filter: b"DCTDecode",
    })
}

fn encode_flate(image: &DynamicImage) -> Option<Encoded> {
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    let rgb = image.to_rgb8();
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(rgb.as_raw()).ok()?;
    Some(Encoded {
        data: encoder.finish().ok()?,
        filter: b"FlateDecode",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_stream(width: u32, height: u32, bytes: usize) -> Stream {
        let mut dict = Dictionary::new();
        dict.set("Type", Object::Name(b"XObject".to_vec()));
        dict.set("Subtype", Object::Name(b"Image".to_vec()));
        dict.set("Width", i64::from(width));
        dict.set("Height", i64::from(height));
        dict.set("BitsPerComponent", 8_i64);
        dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
        Stream::new(dict, vec![0x7F; bytes])
    }

    #[test]
    fn small_images_are_left_alone() {
        let stream = image_stream(4, 4, 100);
        let settings = ImageSettings {
            dpi: 150,
            quality: 80,
            lossless: false,
        };
        assert!(matches!(
            recompress(&stream, Some(612.0), settings),
            Err(Skipped::TooSmall)
        ));
    }

    #[test]
    fn stencil_masks_are_never_touched() {
        // Re-encoding a 1-bit mask as JPEG destroys it.
        let mut stream = image_stream(1000, 1000, 100_000);
        stream.dict.set("ImageMask", true);
        let settings = ImageSettings {
            dpi: 150,
            quality: 80,
            lossless: false,
        };
        assert!(matches!(
            recompress(&stream, Some(612.0), settings),
            Err(Skipped::Mask)
        ));
    }

    #[test]
    fn unknown_filters_are_left_alone() {
        let mut stream = image_stream(1000, 1000, 100_000);
        stream
            .dict
            .set("Filter", Object::Name(b"JPXDecode".to_vec()));
        let settings = ImageSettings {
            dpi: 150,
            quality: 80,
            lossless: false,
        };
        assert!(matches!(
            recompress(&stream, Some(612.0), settings),
            Err(Skipped::UnsupportedFilter)
        ));
    }

    #[test]
    fn unsupported_colour_spaces_are_left_alone() {
        let mut stream = image_stream(1000, 1000, 100_000);
        stream
            .dict
            .set("ColorSpace", Object::Name(b"DeviceCMYK".to_vec()));
        let settings = ImageSettings {
            dpi: 150,
            quality: 80,
            lossless: false,
        };
        assert!(matches!(
            recompress(&stream, Some(612.0), settings),
            Err(Skipped::UnsupportedColour)
        ));
    }

    #[test]
    fn sixteen_bit_samples_are_left_alone() {
        let mut stream = image_stream(1000, 1000, 100_000);
        stream.dict.set("BitsPerComponent", 16_i64);
        let settings = ImageSettings {
            dpi: 150,
            quality: 80,
            lossless: false,
        };
        assert!(matches!(
            recompress(&stream, Some(612.0), settings),
            Err(Skipped::UnsupportedColour)
        ));
    }

    #[test]
    fn a_real_photo_shrinks_and_keeps_its_shape() {
        // A gradient compresses poorly as raw samples and well as JPEG.
        let (w, h) = (900_u32, 600_u32);
        let mut pixels = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                pixels.push((x % 256) as u8);
                pixels.push((y % 256) as u8);
                pixels.push(((x + y) % 256) as u8);
            }
        }
        let mut dict = Dictionary::new();
        dict.set("Subtype", Object::Name(b"Image".to_vec()));
        dict.set("Width", i64::from(w));
        dict.set("Height", i64::from(h));
        dict.set("BitsPerComponent", 8_i64);
        dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
        let stream = Stream::new(dict, pixels);

        let settings = ImageSettings {
            dpi: 150,
            quality: 70,
            lossless: false,
        };
        let out = recompress(&stream, Some(612.0), settings).expect("recompresses");

        assert!(
            out.content.len() < stream.content.len() / 2,
            "expected a real saving"
        );
        assert_eq!(
            out.dict
                .get(b"Filter")
                .and_then(Object::as_name)
                .expect("a filter"),
            b"DCTDecode"
        );
        // 612pt at 150dpi is 1275px, wider than the image, so no downsampling.
        assert_eq!(
            out.dict
                .get(b"Width")
                .and_then(Object::as_i64)
                .expect("width"),
            900
        );
    }

    #[test]
    fn an_oversized_image_is_downsampled_to_the_target_resolution() {
        let (w, h) = (4000_u32, 2000_u32);
        let pixels = vec![0x40_u8; (w * h * 3) as usize];
        let mut dict = Dictionary::new();
        dict.set("Subtype", Object::Name(b"Image".to_vec()));
        dict.set("Width", i64::from(w));
        dict.set("Height", i64::from(h));
        dict.set("BitsPerComponent", 8_i64);
        dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
        let stream = Stream::new(dict, pixels);

        // 612pt wide at 150dpi is 1275 pixels.
        let settings = ImageSettings {
            dpi: 150,
            quality: 80,
            lossless: false,
        };
        let out = recompress(&stream, Some(612.0), settings).expect("recompresses");

        let width = out
            .dict
            .get(b"Width")
            .and_then(Object::as_i64)
            .expect("width");
        let height = out
            .dict
            .get(b"Height")
            .and_then(Object::as_i64)
            .expect("height");
        assert_eq!(width, 1275);
        assert_eq!(height, 638, "the aspect ratio must survive");
    }

    #[test]
    fn skip_reasons_all_describe_themselves() {
        for reason in [
            Skipped::UnsupportedFilter,
            Skipped::UnsupportedColour,
            Skipped::Mask,
            Skipped::TooSmall,
            Skipped::NoGain,
            Skipped::Undecodable,
        ] {
            assert!(!reason.describe().is_empty());
        }
    }
}
