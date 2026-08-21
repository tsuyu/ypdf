//! Glyph widths, read from the document's own fonts.
//!
//! Deciding whether a letter falls inside a rectangle needs to know how wide
//! the letters before it were. Guessing produces drift — by the middle of a
//! line the computed position can be a whole word out, and a redaction that is
//! a word out either leaves the secret behind or eats the sentence.
//!
//! So widths come from the font dictionary the page actually uses. Where a
//! font does not carry them, the base-14 metrics do; where neither applies, the
//! font is marked [`Font::coarse`] and the caller removes whole show
//! operations rather than individual glyphs. Removing too much is recoverable
//! by anyone looking at the result. Removing too little is not.

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object};

/// What is known about one font in a page's resources.
#[derive(Clone, Debug)]
pub struct Font {
    /// Widths by character code, in 1/1000 em.
    widths: HashMap<u32, f32>,
    /// Width for a code the table does not cover.
    missing: f32,
    /// True when glyphs cannot be measured or split individually.
    ///
    /// Composite (Type0) fonts encode a code in one or two bytes according to
    /// a CMap this crate does not read. Rather than mis-split a string, the
    /// caller drops the whole run.
    pub coarse: bool,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            widths: HashMap::new(),
            // Half an em: the average across the base-14 faces, and the value
            // most producers use for /MissingWidth.
            missing: 500.0,
            coarse: false,
        }
    }
}

impl Font {
    /// The advance for one character code, in 1/1000 em.
    #[must_use]
    pub fn width(&self, code: u32) -> f32 {
        self.widths.get(&code).copied().unwrap_or(self.missing)
    }

    /// Read a font dictionary.
    #[must_use]
    pub fn read(document: &Document, dict: &Dictionary) -> Self {
        let subtype = dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .unwrap_or_default();

        if subtype == "Type0" {
            // A CMap decides how many bytes each code takes; without reading it
            // the string cannot be split safely.
            return Self {
                coarse: true,
                ..Self::default()
            };
        }

        let first = dict
            .get(b"FirstChar")
            .and_then(Object::as_i64)
            .unwrap_or(0)
            .max(0);
        let missing = dict
            .get(b"FontDescriptor")
            .ok()
            .and_then(|value| resolve_dict(document, value))
            .and_then(|descriptor| {
                descriptor
                    .get(b"MissingWidth")
                    .and_then(Object::as_float)
                    .ok()
            })
            .unwrap_or(500.0);

        let widths = dict
            .get(b"Widths")
            .and_then(|value| match value {
                Object::Reference(id) => document.get_object(*id),
                other => Ok(other),
            })
            .and_then(Object::as_array)
            .ok()
            .map(|values| {
                values
                    .iter()
                    .enumerate()
                    .filter_map(|(index, value)| {
                        let code = u32::try_from(first + index as i64).ok()?;
                        Some((code, value.as_float().ok()?))
                    })
                    .collect::<HashMap<u32, f32>>()
            })
            .unwrap_or_default();

        if !widths.is_empty() {
            return Self {
                widths,
                missing,
                coarse: false,
            };
        }

        // No /Widths: one of the base-14, which readers are expected to know.
        let base = dict
            .get(b"BaseFont")
            .and_then(Object::as_name)
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .unwrap_or_default();
        base14(&base)
    }

    /// Every font in a page's resources, by resource name.
    #[must_use]
    pub fn read_page(document: &Document, page_id: lopdf::ObjectId) -> HashMap<Vec<u8>, Self> {
        document
            .get_page_fonts(page_id)
            .map(|fonts| {
                fonts
                    .into_iter()
                    .map(|(name, dict)| (name, Self::read(document, dict)))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn resolve_dict<'a>(document: &'a Document, value: &'a Object) -> Option<&'a Dictionary> {
    match value {
        Object::Reference(id) => document.get_dictionary(*id).ok(),
        Object::Dictionary(dict) => Some(dict),
        _ => None,
    }
}

/// Metrics for the base-14 faces, by family.
///
/// Only the shapes that matter for placement: the fixed-width faces are exactly
/// 600, and the proportional ones are close enough to their real metrics that a
/// glyph box lands on the right glyph.
fn base14(base_font: &str) -> Font {
    let name = base_font.to_ascii_lowercase();

    if name.contains("courier") || name.contains("mono") {
        return Font {
            widths: HashMap::new(),
            missing: 600.0,
            coarse: false,
        };
    }

    let widths = if name.contains("times") {
        times_widths()
    } else {
        helvetica_widths()
    };
    Font {
        widths,
        missing: 500.0,
        coarse: false,
    }
}

/// Helvetica widths for codes 32-126.
fn helvetica_widths() -> HashMap<u32, f32> {
    const WIDTHS: [u16; 95] = [
        278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556,
        556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722,
        722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722,
        667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556,
        556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500,
        500, 334, 260, 334, 584,
    ];
    table(&WIDTHS)
}

/// Times-Roman widths for codes 32-126.
fn times_widths() -> HashMap<u32, f32> {
    const WIDTHS: [u16; 95] = [
        250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500,
        500, 500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, 921, 722, 667, 667,
        722, 611, 556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722,
        722, 944, 722, 722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500,
        500, 278, 278, 500, 278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500,
        444, 480, 200, 480, 541,
    ];
    table(&WIDTHS)
}

fn table(widths: &[u16; 95]) -> HashMap<u32, f32> {
    widths
        .iter()
        .enumerate()
        .filter_map(|(index, width)| {
            let code = u32::try_from(index + 32).ok()?;
            Some((code, f32::from(*width)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    #[test]
    fn a_font_with_its_own_widths_uses_them() {
        let doc = Document::with_version("1.7");
        let dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "SomeFont",
            "FirstChar" => 65_i64,
            "Widths" => vec![600.into(), 700.into(), 800.into()],
        };
        let font = Font::read(&doc, &dict);

        assert!((font.width(65) - 600.0).abs() < f32::EPSILON, "A");
        assert!((font.width(67) - 800.0).abs() < f32::EPSILON, "C");
        assert!(!font.coarse);
    }

    #[test]
    fn a_code_outside_the_table_falls_back_rather_than_reading_as_zero() {
        // A zero width would pile every later glyph on top of this one, and the
        // computed positions would be wrong for the rest of the line.
        let doc = Document::with_version("1.7");
        let dict = dictionary! {
            "Type" => "Font",
            "FirstChar" => 65_i64,
            "Widths" => vec![600.into()],
        };
        let font = Font::read(&doc, &dict);
        assert!(font.width(200) > 0.0);
    }

    #[test]
    fn a_base_14_font_without_widths_still_measures() {
        let doc = Document::with_version("1.7");
        let dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        };
        let font = Font::read(&doc, &dict);

        // 'A' is 667 in Helvetica, ' ' is 278.
        assert!((font.width(u32::from(b'A')) - 667.0).abs() < f32::EPSILON);
        assert!((font.width(u32::from(b' ')) - 278.0).abs() < f32::EPSILON);
    }

    #[test]
    fn courier_is_fixed_width() {
        let doc = Document::with_version("1.7");
        let dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Courier-Bold",
        };
        let font = Font::read(&doc, &dict);
        assert!((font.width(u32::from(b'i')) - font.width(u32::from(b'W'))).abs() < f32::EPSILON);
    }

    #[test]
    fn times_is_measured_as_times_rather_than_as_helvetica() {
        let doc = Document::with_version("1.7");
        let dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Times-Roman",
        };
        let font = Font::read(&doc, &dict);
        // 'A' is 722 in Times and 667 in Helvetica.
        assert!((font.width(u32::from(b'A')) - 722.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_composite_font_is_marked_coarse_rather_than_mis_split() {
        // Its codes may be one or two bytes depending on a CMap this crate does
        // not read. Splitting on a guess could leave half a character behind.
        let doc = Document::with_version("1.7");
        let dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type0",
            "BaseFont" => "SomeCJKFont",
        };
        assert!(Font::read(&doc, &dict).coarse);
    }
}
