//! Pulling images back out of a document (spec §6, §22).
//!
//! The rule here is the mirror of the one in [`crate::images`]: **never
//! re-encode something that is already a file**. A DCTDecode stream is a JPEG
//! byte for byte, so it is written out untouched — decoding and re-encoding it
//! would lose quality to no purpose. Everything else is decoded and written as
//! PNG, which is lossless.
//!
//! Anything undecodable is counted with a reason rather than written as a
//! broken file.

use std::collections::BTreeMap;

use lopdf::{Object, ObjectId, Stream};
use ypdf_doc::Pdf;

use crate::images::{self, Skipped};

/// One image taken out of a document.
#[derive(Clone, Debug)]
pub struct ExtractedImage {
    /// 1-based page it is drawn on, if it is drawn on exactly one.
    pub page: Option<u32>,
    /// Position within that page, 1-based, so names are stable and ordered.
    pub index: usize,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// `jpg` or `png`.
    pub extension: &'static str,
    /// The file, ready to write.
    pub bytes: Vec<u8>,
}

impl ExtractedImage {
    /// A stable file name: `page-003-img-01.jpg`.
    ///
    /// Zero-padded so a directory listing sorts the way the document reads.
    #[must_use]
    pub fn file_name(&self) -> String {
        match self.page {
            Some(page) => format!("page-{:03}-img-{:02}.{}", page, self.index, self.extension),
            None => format!("unplaced-img-{:03}.{}", self.index, self.extension),
        }
    }
}

/// Every image in a document, plus a count of what could not be taken out.
#[derive(Clone, Debug, Default)]
pub struct ImageInventory {
    /// The images, in page order.
    pub images: Vec<ExtractedImage>,
    /// Images left behind, by reason.
    pub skipped: BTreeMap<&'static str, usize>,
}

impl ImageInventory {
    /// Total bytes across every extracted image.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.images
            .iter()
            .map(|image| image.bytes.len() as u64)
            .sum()
    }
}

/// Take every image out of `pdf`.
///
/// Nothing is written: where the files go is a front-end decision.
#[must_use]
pub fn extract_images(pdf: &Pdf) -> ImageInventory {
    let doc = pdf.raw();
    let mut inventory = ImageInventory::default();
    let mut seen: Vec<ObjectId> = Vec::new();

    // Page order first, so names run with the document rather than with
    // whatever order the objects happen to sit in the file.
    for (page_number, page_id) in pdf.page_ids().iter().enumerate() {
        let Ok(page) = doc.get_dictionary(*page_id) else {
            continue;
        };

        let mut index = 0;
        for id in images::xobject_ids(doc, page) {
            let Some(object) = doc.objects.get(&id) else {
                continue;
            };
            if !images::is_image(object) {
                continue;
            }
            // An image used on several pages is written once, named for the
            // first page that draws it.
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            index += 1;

            let Ok(stream) = object.as_stream() else {
                continue;
            };
            let page_number = u32::try_from(page_number + 1).ok();
            match to_file(stream) {
                Ok((extension, bytes, width, height)) => inventory.images.push(ExtractedImage {
                    page: page_number,
                    index,
                    width,
                    height,
                    extension,
                    bytes,
                }),
                Err(reason) => *inventory.skipped.entry(reason.describe()).or_insert(0) += 1,
            }
        }
    }

    inventory
}

/// Turn one image stream into file bytes.
fn to_file(stream: &Stream) -> Result<(&'static str, Vec<u8>, u32, u32), Skipped> {
    let dict = &stream.dict;

    let width = dict
        .get(b"Width")
        .and_then(Object::as_i64)
        .map_err(|_| Skipped::Undecodable)?;
    let height = dict
        .get(b"Height")
        .and_then(Object::as_i64)
        .map_err(|_| Skipped::Undecodable)?;
    let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
        return Err(Skipped::Undecodable);
    };

    // A DCTDecode stream is a JPEG file already. Copying it is both faster and
    // better than any round trip through a decoder.
    if images::filter_names(dict).last().map(String::as_str) == Some("DCTDecode") {
        return Ok(("jpg", stream.content.clone(), width, height));
    }

    let image = images::decode(stream, width, height)?;
    let mut bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .map_err(|_| Skipped::Undecodable)?;
    Ok(("png", bytes, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_sort_the_way_the_document_reads() {
        let mut names: Vec<String> = [(9_u32, 1_usize), (10, 1), (2, 3), (2, 1)]
            .into_iter()
            .map(|(page, index)| {
                ExtractedImage {
                    page: Some(page),
                    index,
                    width: 1,
                    height: 1,
                    extension: "png",
                    bytes: Vec::new(),
                }
                .file_name()
            })
            .collect();
        names.sort();

        assert_eq!(
            names,
            vec![
                "page-002-img-01.png",
                "page-002-img-03.png",
                "page-009-img-01.png",
                "page-010-img-01.png",
            ]
        );
    }

    #[test]
    fn an_image_on_no_page_still_gets_a_name() {
        let image = ExtractedImage {
            page: None,
            index: 4,
            width: 1,
            height: 1,
            extension: "jpg",
            bytes: Vec::new(),
        };
        assert_eq!(image.file_name(), "unplaced-img-004.jpg");
    }
}
