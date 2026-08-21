//! Redaction: removing content, not covering it (spec §11).
//!
//! The spec is explicit that a black rectangle drawn over the page is not
//! redaction, and it is right: the text is still in the file, `Ctrl+A` still
//! copies it, and every "redacted document" scandal of the last twenty years
//! has been exactly that mistake. So this crate takes things out.
//!
//! For each redaction rectangle, on each page:
//!
//! * **glyphs inside it are deleted from the content stream**, replaced by the
//!   kerning that advances past them so the surviving text does not move;
//! * **pixels inside it are overwritten in the image itself**, decoded, filled,
//!   and re-encoded;
//! * **paths drawn entirely inside it are dropped**;
//! * **annotations that overlap it are deleted**, since a link or a comment
//!   carries its own text;
//! * an **opaque rectangle is drawn last**, over the area.
//!
//! That last step is the one to be careful about describing. It is not the
//! redaction — it is what covers vector artwork that crosses the boundary and
//! cannot be split, and it makes the redaction visible as a deliberate act. If
//! it were the whole mechanism this crate would be the thing spec §11 forbids.
//!
//! Two things this **cannot** do, both reported rather than hidden:
//!
//! * text in a font it cannot measure is removed a whole show operation at a
//!   time, which takes more than was asked for;
//! * an image it cannot decode has its draw removed entirely, so the picture
//!   disappears rather than being partly cleaned.
//!
//! Redaction is not undoable. It is applied to a copy, and the original file is
//! never written to.

mod content;
mod fonts;
mod geometry;
mod images;

use lopdf::content::{Content, Operation};
use lopdf::{Object, Stream, dictionary};
use ypdf_core::{Error, Result};
use ypdf_doc::Pdf;

pub use geometry::{Matrix, Rect};

/// What to take out, and from where.
#[derive(Clone, Debug)]
pub struct Redaction {
    /// 1-based page number.
    pub page: u32,
    /// The area to clear, in PDF points from the bottom-left of the page.
    pub rect: Rect,
}

/// How a redaction should be carried out.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// Draw an opaque box over each redacted area.
    ///
    /// On by default. It covers vector artwork that could not be split, and it
    /// makes the redaction visible — a page that has quietly lost a sentence is
    /// worse than one that shows where something was taken out.
    pub draw_cover: bool,
    /// The cover colour, as red / green / blue in 0.0-1.0.
    pub cover_colour: (f32, f32, f32),
    /// How far outside the rectangle a glyph may start and still be removed.
    ///
    /// Glyph boxes are computed from metrics and are approximate; a rectangle
    /// drawn snugly around a word should still take all of it.
    pub margin: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            draw_cover: true,
            cover_colour: (0.0, 0.0, 0.0),
            margin: 1.0,
        }
    }
}

/// What a redaction removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Pages that changed.
    pub pages: usize,
    /// Glyphs deleted individually.
    pub glyphs: usize,
    /// Show operations dropped whole because their font could not be measured.
    ///
    /// Non-zero means more text was removed than the rectangles covered.
    pub coarse_runs: usize,
    /// Images whose pixels were overwritten.
    pub images_redacted: usize,
    /// Images whose draw was removed because they could not be decoded.
    ///
    /// Non-zero means a picture disappeared from the page.
    pub images_removed: usize,
    /// Paths dropped.
    pub paths: usize,
    /// Annotations deleted.
    pub annotations: usize,
}

impl Report {
    /// Did anything come out?
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.glyphs == 0
            && self.coarse_runs == 0
            && self.images_redacted == 0
            && self.images_removed == 0
            && self.paths == 0
            && self.annotations == 0
    }

    /// A summary for a person, including what it could not do precisely.
    #[must_use]
    pub fn to_human(&self) -> String {
        let mut out = format!(
            "Pages:       {}\nGlyphs:      {} removed\n",
            self.pages, self.glyphs
        );
        if self.images_redacted > 0 {
            out.push_str(&format!("Images:      {} cleared\n", self.images_redacted));
        }
        if self.paths > 0 {
            out.push_str(&format!("Drawings:    {} removed\n", self.paths));
        }
        if self.annotations > 0 {
            out.push_str(&format!("Annotations: {} removed\n", self.annotations));
        }
        if self.coarse_runs > 0 {
            out.push_str(&format!(
                "\nNote: {} text run(s) used a font this build cannot measure, so the whole\n\
                 run was removed. More text came out than the rectangles covered.\n",
                self.coarse_runs
            ));
        }
        if self.images_removed > 0 {
            out.push_str(&format!(
                "\nNote: {} image(s) could not be decoded, so the whole image was removed\n\
                 rather than partly cleared.\n",
                self.images_removed
            ));
        }
        out
    }
}

/// Apply `redactions` to `pdf`.
///
/// The document is modified in place; nothing is written to disk. Redaction
/// cannot be undone, so the caller is expected to be working on a copy.
pub fn redact(pdf: &mut Pdf, redactions: &[Redaction], settings: &Settings) -> Result<Report> {
    let count = pdf.page_count();
    for redaction in redactions {
        if redaction.page == 0 || redaction.page > count {
            return Err(Error::PageOutOfRange {
                requested: redaction.page,
                pages: count,
            });
        }
        if redaction.rect.width() <= 0.0 || redaction.rect.height() <= 0.0 {
            return Err(Error::Config {
                detail: "a redaction rectangle with no area would remove nothing".into(),
                source_path: None,
            });
        }
    }

    let mut report = Report::default();

    for page in 1..=count {
        let rects: Vec<Rect> = redactions
            .iter()
            .filter(|redaction| redaction.page == page)
            .map(|redaction| redaction.rect.grown(settings.margin))
            .collect();
        if rects.is_empty() {
            continue;
        }

        let Some(page_id) = pdf.page_ids().get((page - 1) as usize).copied() else {
            continue;
        };

        let fonts = fonts::Font::read_page(pdf.raw(), page_id);
        let raw = pdf.raw().get_page_content(page_id);
        let decoded = Content::decode(&raw).map_err(|e| {
            Error::parse(ypdf_core::ParseFailure::Other {
                detail: format!("page {page} content could not be parsed: {e}"),
            })
        })?;

        let walk = content::walk(pdf.raw(), page_id, &decoded, &rects, &fonts);
        let mut operations = walk.operations;
        report.glyphs += walk.removed.glyphs;
        report.coarse_runs += walk.removed.coarse_runs;
        report.paths += walk.removed.paths;

        // Images are edited after the walk, because editing them needs a
        // mutable document and the walk needs to read it.
        let mut undecodable: Vec<Vec<u8>> = Vec::new();
        for draw in &walk.images {
            let Some(image_id) = resolve_xobject(pdf, page_id, &draw.name) else {
                continue;
            };

            // An image drawn on more than one page is copied first: redacting
            // page 4 must not blank the same logo on page 1.
            let target = if images::draw_count(pdf.raw(), image_id) > 1 {
                match images::duplicate(pdf.raw_mut(), image_id) {
                    Some(copy) => {
                        set_xobject(pdf, page_id, &draw.name, copy);
                        copy
                    }
                    None => image_id,
                }
            } else {
                image_id
            };

            let mut cleared = false;
            for rect in &rects {
                match images::redact_region(pdf.raw_mut(), target, draw.matrix, *rect, draw.bounds)
                {
                    images::Outcome::Redacted => cleared = true,
                    images::Outcome::Undecodable => {
                        undecodable.push(draw.name.clone());
                        break;
                    }
                    images::Outcome::NoOverlap => {}
                }
            }
            if cleared {
                report.images_redacted += 1;
            }
        }

        if !undecodable.is_empty() {
            // An image that could not be cleared must not be drawn at all.
            let before = operations.len();
            operations.retain(|operation| {
                if operation.operator != "Do" {
                    return true;
                }
                let name = operation
                    .operands
                    .first()
                    .and_then(|object| object.as_name().ok())
                    .map(<[u8]>::to_vec);
                !name.is_some_and(|name| undecodable.contains(&name))
            });
            report.images_removed += before - operations.len();
        }

        if settings.draw_cover {
            operations.extend(cover(&rects, settings));
        }

        report.annotations += strip_annotations(pdf, page_id, &rects);

        let encoded = Content { operations }
            .encode()
            .map_err(|e| Error::Backend {
                backend: "lopdf",
                detail: e.to_string(),
            })?;
        replace_content(pdf, page_id, encoded);
        report.pages += 1;
    }

    tracing::info!(
        pages = report.pages,
        glyphs = report.glyphs,
        images = report.images_redacted,
        "redaction applied"
    );
    Ok(report)
}

/// The opaque boxes drawn over each area.
fn cover(rects: &[Rect], settings: &Settings) -> Vec<Operation> {
    let (r, g, b) = settings.cover_colour;
    let mut operations = vec![
        Operation::new("q", Vec::new()),
        Operation::new(
            "rg",
            vec![Object::Real(r), Object::Real(g), Object::Real(b)],
        ),
    ];
    for rect in rects {
        operations.push(Operation::new(
            "re",
            vec![
                Object::Real(rect.x0),
                Object::Real(rect.y0),
                Object::Real(rect.width()),
                Object::Real(rect.height()),
            ],
        ));
        operations.push(Operation::new("f", Vec::new()));
    }
    operations.push(Operation::new("Q", Vec::new()));
    operations
}

/// Remove annotations that overlap a redaction.
///
/// A link, a comment, or a form field carries its own text and its own target,
/// none of which is in the content stream.
fn strip_annotations(pdf: &mut Pdf, page_id: lopdf::ObjectId, rects: &[Rect]) -> usize {
    let annotations = pdf
        .raw()
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Annots").ok())
        .and_then(|value| match value {
            Object::Array(items) => Some(items.clone()),
            Object::Reference(id) => pdf
                .raw()
                .get_object(*id)
                .ok()
                .and_then(|object| object.as_array().ok())
                .cloned(),
            _ => None,
        });

    let Some(annotations) = annotations else {
        return 0;
    };

    let mut kept = Vec::with_capacity(annotations.len());
    let mut removed = 0;

    for annotation in annotations {
        let dict = match &annotation {
            Object::Reference(id) => pdf.raw().get_dictionary(*id).ok().cloned(),
            Object::Dictionary(dict) => Some(dict.clone()),
            _ => None,
        };

        let overlaps = dict
            .as_ref()
            .and_then(|dict| dict.get(b"Rect").ok()?.as_array().ok().cloned())
            .and_then(|values| {
                let numbers: Vec<f32> = values
                    .iter()
                    .filter_map(|value| value.as_float().ok())
                    .collect();
                match numbers[..] {
                    [x0, y0, x1, y1] => Some(Rect::new(x0, y0, x1, y1)),
                    _ => None,
                }
            })
            .is_some_and(|rect| rects.iter().any(|r| r.intersects(rect)));

        if overlaps {
            removed += 1;
        } else {
            kept.push(annotation);
        }
    }

    if removed > 0
        && let Ok(page) = pdf.raw_mut().get_dictionary_mut(page_id)
    {
        page.set("Annots", Object::Array(kept));
    }
    removed
}

/// Replace a page's content with one stream.
///
/// The old streams are dropped from the page, and `commit` prunes anything left
/// unreferenced — which is what makes the removal permanent rather than merely
/// invisible.
fn replace_content(pdf: &mut Pdf, page_id: lopdf::ObjectId, content: Vec<u8>) {
    let stream_id = pdf
        .raw_mut()
        .add_object(Stream::new(dictionary! {}, content));
    if let Ok(page) = pdf.raw_mut().get_dictionary_mut(page_id) {
        page.set("Contents", Object::Reference(stream_id));
    }
}

fn resolve_xobject(pdf: &Pdf, page_id: lopdf::ObjectId, name: &[u8]) -> Option<lopdf::ObjectId> {
    let resources = page_resources(pdf, page_id)?;
    let xobjects = match resources.get(b"XObject").ok()? {
        Object::Reference(id) => pdf.raw().get_dictionary(*id).ok()?.clone(),
        Object::Dictionary(dict) => dict.clone(),
        _ => return None,
    };
    xobjects.get(name).ok()?.as_reference().ok()
}

fn set_xobject(pdf: &mut Pdf, page_id: lopdf::ObjectId, name: &[u8], target: lopdf::ObjectId) {
    let resources_ref = pdf
        .raw()
        .get_dictionary(page_id)
        .ok()
        .and_then(|page| page.get(b"Resources").ok())
        .and_then(|value| value.as_reference().ok());

    let xobject_ref = page_resources(pdf, page_id)
        .and_then(|resources| resources.get(b"XObject").ok().cloned())
        .and_then(|value| value.as_reference().ok());

    if let Some(id) = xobject_ref {
        if let Ok(xobjects) = pdf.raw_mut().get_dictionary_mut(id) {
            xobjects.set(String::from_utf8_lossy(name).into_owned(), target);
        }
        return;
    }

    let document = pdf.raw_mut();
    let container = if let Some(id) = resources_ref {
        document.get_dictionary_mut(id).ok()
    } else {
        document.get_dictionary_mut(page_id).ok().and_then(|page| {
            match page.get_mut(b"Resources") {
                Ok(Object::Dictionary(dict)) => Some(dict),
                _ => None,
            }
        })
    };

    if let Some(resources) = container
        && let Ok(Object::Dictionary(xobjects)) = resources.get_mut(b"XObject")
    {
        xobjects.set(String::from_utf8_lossy(name).into_owned(), target);
    }
}

fn page_resources(pdf: &Pdf, page_id: lopdf::ObjectId) -> Option<lopdf::Dictionary> {
    let page = pdf.raw().get_dictionary(page_id).ok()?;
    match page.get(b"Resources").ok()? {
        Object::Reference(id) => pdf.raw().get_dictionary(*id).ok().cloned(),
        Object::Dictionary(dict) => Some(dict.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    #[test]
    fn a_page_number_past_the_end_is_refused() {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        let error = redact(
            &mut pdf,
            &[Redaction {
                page: 9,
                rect: Rect::new(0.0, 0.0, 10.0, 10.0),
            }],
            &Settings::default(),
        )
        .expect_err("no such page");
        assert_eq!(error.code(), "E_PAGE_RANGE");
    }

    #[test]
    fn a_rectangle_with_no_area_is_refused() {
        let mut pdf = Pdf::open(fixture("two-pages.pdf")).expect("opens");
        assert!(
            redact(
                &mut pdf,
                &[Redaction {
                    page: 1,
                    rect: Rect::new(10.0, 10.0, 10.0, 40.0),
                }],
                &Settings::default(),
            )
            .is_err()
        );
    }

    #[test]
    fn the_cover_is_drawn_last_so_it_covers() {
        let rects = [Rect::new(10.0, 10.0, 20.0, 20.0)];
        let operations = cover(&rects, &Settings::default());
        assert_eq!(operations.first().map(|op| op.operator.as_str()), Some("q"));
        assert_eq!(operations.last().map(|op| op.operator.as_str()), Some("Q"));
        assert!(operations.iter().any(|op| op.operator == "f"));
    }

    #[test]
    fn a_report_says_when_it_removed_more_than_was_asked_for() {
        // The caller has to be able to tell the difference between a precise
        // redaction and a coarse one.
        let report = Report {
            coarse_runs: 2,
            ..Report::default()
        };
        let human = report.to_human();
        assert!(human.contains("More text came out"), "{human}");
    }

    #[test]
    fn a_report_says_when_an_image_disappeared_entirely() {
        let report = Report {
            images_removed: 1,
            ..Report::default()
        };
        assert!(report.to_human().contains("whole image was removed"));
    }
}
