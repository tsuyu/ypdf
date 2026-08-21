//! Drawing annotations, so they appear the same in every reader.
//!
//! An annotation without an `/AP` stream is a suggestion: the reader draws it
//! however it likes, or not at all. PDFium draws some kinds and ignores others,
//! and printing usually ignores all of them. So every annotation this crate
//! writes carries its own appearance, and what you see is what the file says.
//!
//! One deliberate exception: a sticky note keeps a small icon of its own but
//! readers habitually draw their own, and that is fine — the note's value is
//! the comment inside it, not the shape of the pin.

use lopdf::{Dictionary, Object, Stream, dictionary};
use ypdf_doc::winansi;

use crate::model::{Annotation, Colour, Shape};

/// Bezier constant for drawing a circle with four curves.
const KAPPA: f32 = 0.552_284_8;

/// The stream and the resources it needs.
pub struct Drawn {
    /// The content stream.
    pub stream: Stream,
    /// True when the text could not be encoded and was left out.
    pub unencodable: bool,
}

/// Build the appearance for one annotation.
///
/// The stream draws in a space whose origin is the annotation's bottom-left
/// corner, which is what `/BBox` and the identity `/Matrix` mean together.
#[must_use]
pub fn draw(annotation: &Annotation) -> Drawn {
    let bounds = annotation.shape.bounds_with_width(annotation.width);
    let width = (bounds[2] - bounds[0]).max(1.0);
    let height = (bounds[3] - bounds[1]).max(1.0);

    // Everything is drawn relative to the bottom-left corner.
    let ox = bounds[0];
    let oy = bounds[1];
    let mut body = String::new();
    let mut unencodable = false;

    let stroke = format!(
        "{:.4} {:.4} {:.4} RG\n",
        annotation.colour.0, annotation.colour.1, annotation.colour.2
    );
    let fill = format!(
        "{:.4} {:.4} {:.4} rg\n",
        annotation.colour.0, annotation.colour.1, annotation.colour.2
    );

    match &annotation.shape {
        Shape::Highlight { quads } => {
            body.push_str(&fill);
            for quad in quads {
                body.push_str(&format!(
                    "{:.2} {:.2} {:.2} {:.2} re f\n",
                    quad[0] - ox,
                    quad[1] - oy,
                    quad[2] - quad[0],
                    quad[3] - quad[1]
                ));
            }
        }
        Shape::Underline { quads } => {
            body.push_str(&fill);
            for quad in quads {
                // A little above the bottom of the line box, where a reader
                // draws it, rather than through the descenders.
                let thickness = ((quad[3] - quad[1]) * 0.07).max(0.6);
                body.push_str(&format!(
                    "{:.2} {:.2} {:.2} {thickness:.2} re f\n",
                    quad[0] - ox,
                    quad[1] - oy + thickness,
                    quad[2] - quad[0]
                ));
            }
        }
        Shape::StrikeOut { quads } => {
            body.push_str(&fill);
            for quad in quads {
                let thickness = ((quad[3] - quad[1]) * 0.07).max(0.6);
                let middle = (quad[3] - quad[1]) * 0.42;
                body.push_str(&format!(
                    "{:.2} {:.2} {:.2} {thickness:.2} re f\n",
                    quad[0] - ox,
                    quad[1] - oy + middle,
                    quad[2] - quad[0]
                ));
            }
        }
        Shape::Note { .. } => {
            // A rounded page with a folded corner: recognizable at 20 points,
            // which is all the room there is.
            body.push_str(&fill);
            body.push_str("0 0 0 RG\n0.7 w\n");
            body.push_str(&format!(
                "1 1 {:.2} {:.2} re B\n",
                width - 2.0,
                height - 2.0
            ));
            for line in 1..=3 {
                #[expect(clippy::cast_precision_loss, reason = "three lines")]
                let y = height - 4.0 - (line as f32) * (height - 6.0) / 4.0;
                body.push_str(&format!(
                    "0 0 0 RG\n0.5 w\n4 {y:.2} m {:.2} {y:.2} l S\n",
                    width - 4.0
                ));
            }
        }
        Shape::TextBox { rect, size } => {
            let (text, missing) = encode(&annotation.contents);
            unencodable = missing;
            let box_width = rect[2] - rect[0];
            let box_height = rect[3] - rect[1];
            let size = size.unwrap_or_else(|| fit(&annotation.contents, box_width, box_height));

            body.push_str(&stroke);
            body.push_str(&format!("{:.2} w\n", annotation.width));
            body.push_str(&format!(
                "0.5 0.5 {:.2} {:.2} re S\n",
                box_width - 1.0,
                box_height - 1.0
            ));
            body.push_str(&fill);
            body.push_str(&format!(
                "BT\n/YPDFAF {size:.2} Tf\n1 0 0 1 {:.2} {:.2} Tm\n({text}) Tj\nET\n",
                3.0,
                box_height - size - 3.0
            ));
        }
        Shape::Ink { strokes } => {
            body.push_str(&stroke);
            body.push_str(&format!("{:.2} w\n1 J\n1 j\n", annotation.width));
            for points in strokes {
                let mut first = true;
                for point in points {
                    body.push_str(&format!(
                        "{:.2} {:.2} {}\n",
                        point[0] - ox,
                        point[1] - oy,
                        if std::mem::take(&mut first) { "m" } else { "l" }
                    ));
                }
                body.push_str("S\n");
            }
        }
        Shape::Rectangle { rect, fill: inner } => {
            body.push_str(&stroke);
            body.push_str(&format!("{:.2} w\n", annotation.width));
            if let Some(inner) = inner {
                body.push_str(&colour_fill(*inner));
            }
            let inset = annotation.width / 2.0;
            body.push_str(&format!(
                "{inset:.2} {inset:.2} {:.2} {:.2} re {}\n",
                (rect[2] - rect[0]) - annotation.width,
                (rect[3] - rect[1]) - annotation.width,
                if inner.is_some() { "B" } else { "S" }
            ));
        }
        Shape::Ellipse { rect, fill: inner } => {
            body.push_str(&stroke);
            body.push_str(&format!("{:.2} w\n", annotation.width));
            if let Some(inner) = inner {
                body.push_str(&colour_fill(*inner));
            }
            body.push_str(&ellipse(
                annotation.width / 2.0,
                annotation.width / 2.0,
                (rect[2] - rect[0]) - annotation.width,
                (rect[3] - rect[1]) - annotation.width,
            ));
            body.push_str(if inner.is_some() { "B\n" } else { "S\n" });
        }
        Shape::Line { from, to, arrow } => {
            body.push_str(&stroke);
            body.push_str(&format!("{:.2} w\n1 J\n", annotation.width));
            let (x0, y0) = (from[0] - ox, from[1] - oy);
            let (x1, y1) = (to[0] - ox, to[1] - oy);
            body.push_str(&format!("{x0:.2} {y0:.2} m {x1:.2} {y1:.2} l S\n"));

            if *arrow {
                body.push_str(&fill);
                body.push_str(&arrowhead(x0, y0, x1, y1, annotation.width));
            }
        }
        Shape::Stamp { rect, text } => {
            let (encoded, missing) = encode(text);
            unencodable = missing;
            let box_width = rect[2] - rect[0];
            let box_height = rect[3] - rect[1];
            let size = fit(text, box_width * 0.85, box_height * 0.6);

            body.push_str(&stroke);
            body.push_str(&format!("{:.2} w\n", annotation.width.max(1.5)));
            body.push_str(&format!(
                "1.5 1.5 {:.2} {:.2} re S\n",
                box_width - 3.0,
                box_height - 3.0
            ));
            body.push_str(&fill);
            // Roughly centred, using the average advance of the bold face.
            #[expect(clippy::cast_precision_loss, reason = "stamp words are short")]
            let characters = text.chars().count().max(1) as f32;
            let text_width = characters * size * 0.6;
            body.push_str(&format!(
                "BT\n/YPDFAF {size:.2} Tf\n1 0 0 1 {:.2} {:.2} Tm\n({encoded}) Tj\nET\n",
                ((box_width - text_width) / 2.0).max(3.0),
                (box_height - size * 0.72) / 2.0
            ));
        }
    }

    let mut dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Form",
        "BBox" => Object::Array(vec![
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(width),
            Object::Real(height),
        ]),
    };

    let needs_font = matches!(
        annotation.shape,
        Shape::TextBox { .. } | Shape::Stamp { .. }
    );
    let needs_state = annotation.opacity < 1.0 || is_markup(&annotation.shape);
    if needs_font || needs_state {
        dict.set(
            "Resources",
            Object::Dictionary(resources(needs_font, needs_state)),
        );
    }

    // The whole drawing goes inside one save/restore, and the transparency
    // state applies to all of it.
    let mut content = String::from("q\n");
    if needs_state {
        content.push_str("/YPDFAG gs\n");
    }
    content.push_str(&body);
    content.push_str("Q\n");

    Drawn {
        stream: Stream::new(dict, content.into_bytes()),
        unencodable,
    }
}

/// Is this a text markup annotation, which should not hide the text under it?
const fn is_markup(shape: &Shape) -> bool {
    matches!(shape, Shape::Highlight { .. })
}

/// The resource dictionary an appearance needs.
///
/// The graphics state carries both the opacity and, for a highlight, the
/// multiply blend that lets the words show through the wash. A highlight drawn
/// opaque is a redaction that says "highlight".
fn resources(font: bool, state: bool) -> Dictionary {
    let mut resources = Dictionary::new();
    if font {
        resources.set(
            "Font",
            Object::Dictionary(dictionary! {
                "YPDFAF" => Object::Dictionary(dictionary! {
                    "Type" => "Font",
                    "Subtype" => "Type1",
                    "BaseFont" => "Helvetica-Bold",
                    "Encoding" => "WinAnsiEncoding",
                }),
            }),
        );
    }
    if state {
        resources.set("ExtGState", Object::Dictionary(Dictionary::new()));
    }
    resources
}

/// Fill in the graphics state, which needs the annotation's own opacity.
pub fn graphics_state(annotation: &Annotation) -> Dictionary {
    let alpha = f64::from(annotation.opacity.clamp(0.0, 1.0));
    let mut state = dictionary! {
        "Type" => "ExtGState",
        "CA" => alpha,
        "ca" => alpha,
    };
    if is_markup(&annotation.shape) {
        state.set("BM", Object::Name(b"Multiply".to_vec()));
    }
    state
}

fn colour_fill(colour: Colour) -> String {
    format!("{:.4} {:.4} {:.4} rg\n", colour.0, colour.1, colour.2)
}

/// Four bezier curves make a passable ellipse.
fn ellipse(x: f32, y: f32, width: f32, height: f32) -> String {
    let (rx, ry) = (width / 2.0, height / 2.0);
    let (cx, cy) = (x + rx, y + ry);
    let (ox, oy) = (rx * KAPPA, ry * KAPPA);

    format!(
        "{:.2} {cy:.2} m\n\
         {:.2} {:.2} {:.2} {:.2} {cx:.2} {:.2} c\n\
         {:.2} {:.2} {:.2} {:.2} {:.2} {cy:.2} c\n\
         {:.2} {:.2} {:.2} {:.2} {cx:.2} {:.2} c\n\
         {:.2} {:.2} {:.2} {:.2} {:.2} {cy:.2} c\n",
        cx - rx,
        cx - rx,
        cy + oy,
        cx - ox,
        cy + ry,
        cy + ry,
        cx + ox,
        cy + ry,
        cx + rx,
        cy + oy,
        cx + rx,
        cx + rx,
        cy - oy,
        cx + ox,
        cy - ry,
        cy - ry,
        cx - ox,
        cy - ry,
        cx - rx,
        cy - oy,
        cx - rx,
    )
}

/// A filled triangle at the far end of a line.
fn arrowhead(x0: f32, y0: f32, x1: f32, y1: f32, width: f32) -> String {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let length = (dx * dx + dy * dy).sqrt();
    if length < f32::EPSILON {
        return String::new();
    }

    let (ux, uy) = (dx / length, dy / length);
    let size = (width * 4.0).clamp(4.0, 20.0);
    // Back along the line, then out to each side.
    let (bx, by) = (x1 - ux * size, y1 - uy * size);
    let (px, py) = (-uy * size * 0.4, ux * size * 0.4);

    format!(
        "{x1:.2} {y1:.2} m {:.2} {:.2} l {:.2} {:.2} l f\n",
        bx + px,
        by + py,
        bx - px,
        by - py
    )
}

/// Encode text for a base-14 font, reporting what it could not carry.
fn encode(text: &str) -> (String, bool) {
    match winansi::encode_literal(text) {
        Some(encoded) => (encoded, false),
        // Drawn empty rather than as question marks: the comment itself is
        // still in `/Contents`, where a reader shows it in full.
        None => (String::new(), true),
    }
}

/// A point size that fits `text` in a box.
fn fit(text: &str, width: f32, height: f32) -> f32 {
    #[expect(clippy::cast_precision_loss, reason = "annotation text is short")]
    let characters = text.chars().count().max(1) as f32;
    let by_width = width / (characters * 0.6);
    let by_height = height * 0.8;
    by_width.min(by_height).clamp(4.0, 48.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(annotation: &Annotation) -> String {
        String::from_utf8_lossy(&draw(annotation).stream.content).into_owned()
    }

    #[test]
    fn every_appearance_is_balanced() {
        // An unbalanced save/restore leaks the annotation's colour into
        // whatever the reader draws next.
        let shapes = [
            Shape::Highlight {
                quads: vec![[10.0, 10.0, 100.0, 24.0]],
            },
            Shape::Underline {
                quads: vec![[10.0, 10.0, 100.0, 24.0]],
            },
            Shape::StrikeOut {
                quads: vec![[10.0, 10.0, 100.0, 24.0]],
            },
            Shape::Note { at: [10.0, 30.0] },
            Shape::TextBox {
                rect: [10.0, 10.0, 200.0, 60.0],
                size: None,
            },
            Shape::Ink {
                strokes: vec![vec![[10.0, 10.0], [50.0, 40.0]]],
            },
            Shape::Rectangle {
                rect: [10.0, 10.0, 100.0, 60.0],
                fill: None,
            },
            Shape::Ellipse {
                rect: [10.0, 10.0, 100.0, 60.0],
                fill: None,
            },
            Shape::Line {
                from: [10.0, 10.0],
                to: [100.0, 60.0],
                arrow: true,
            },
            Shape::Stamp {
                rect: [10.0, 10.0, 150.0, 60.0],
                text: "APPROVED".into(),
            },
        ];

        for shape in shapes {
            let annotation = Annotation::new(1, shape.clone()).with_contents("note");
            let text = content(&annotation);
            assert_eq!(
                text.matches('q').count(),
                text.matches('Q').count(),
                "{shape:?}: {text}"
            );
            assert_eq!(text.matches("BT").count(), text.matches("ET").count());
            assert!(!text.trim().is_empty(), "{shape:?} drew nothing");
        }
    }

    #[test]
    fn a_highlight_multiplies_so_the_words_show_through() {
        // An opaque highlight is a redaction that says "highlight".
        let annotation = Annotation::new(
            1,
            Shape::Highlight {
                quads: vec![[10.0, 10.0, 100.0, 24.0]],
            },
        );
        let state = graphics_state(&annotation);
        assert_eq!(
            state.get(b"BM").and_then(Object::as_name).ok(),
            Some(b"Multiply".as_slice())
        );
        assert!(content(&annotation).contains("/YPDFAG gs"));
    }

    #[test]
    fn an_opacity_below_one_reaches_the_graphics_state() {
        let annotation = Annotation::new(
            1,
            Shape::Rectangle {
                rect: [0.0, 0.0, 10.0, 10.0],
                fill: None,
            },
        )
        .with_opacity(0.25);

        let state = graphics_state(&annotation);
        assert_eq!(state.get(b"ca").and_then(Object::as_float).ok(), Some(0.25));
        assert!(content(&annotation).contains("gs"));
    }

    #[test]
    fn drawing_happens_relative_to_the_annotations_own_corner() {
        // The appearance draws in its own space; a stroke at the rectangle's
        // origin has to land at 0,0 in the stream.
        let annotation = Annotation::new(
            1,
            Shape::Ink {
                strokes: vec![vec![[100.0, 700.0], [140.0, 720.0]]],
            },
        );
        // The stroke reaches half its width past the path, so the origin sits
        // that far outside the first point rather than exactly on it.
        let text = content(&annotation);
        assert!(text.contains("1.50 1.50 m"), "{text}");
        assert!(text.contains("41.50 21.50 l"), "{text}");
    }

    #[test]
    fn an_arrow_adds_a_head_and_a_plain_line_does_not() {
        let with = Annotation::new(
            1,
            Shape::Line {
                from: [0.0, 0.0],
                to: [100.0, 0.0],
                arrow: true,
            },
        );
        let without = Annotation::new(
            1,
            Shape::Line {
                from: [0.0, 0.0],
                to: [100.0, 0.0],
                arrow: false,
            },
        );

        assert!(content(&with).contains(" f\n"), "no arrowhead was filled");
        assert!(!content(&without).contains(" f\n"));
    }

    #[test]
    fn a_filled_rectangle_paints_and_strokes() {
        let annotation = Annotation::new(
            1,
            Shape::Rectangle {
                rect: [0.0, 0.0, 50.0, 20.0],
                fill: Some(Colour(0.0, 0.0, 1.0)),
            },
        );
        let text = content(&annotation);
        assert!(text.contains("0.0000 0.0000 1.0000 rg"), "{text}");
        assert!(text.contains(" B\n"), "{text}");
    }

    #[test]
    fn text_the_font_cannot_draw_is_reported_rather_than_mangled() {
        // The comment is still in /Contents, where a reader shows it in full.
        let annotation = Annotation::new(
            1,
            Shape::TextBox {
                rect: [0.0, 0.0, 100.0, 40.0],
                size: None,
            },
        )
        .with_contents("機密");

        assert!(draw(&annotation).unencodable);
    }

    #[test]
    fn an_ellipse_closes_its_four_curves() {
        let annotation = Annotation::new(
            1,
            Shape::Ellipse {
                rect: [0.0, 0.0, 100.0, 50.0],
                fill: None,
            },
        );
        let text = content(&annotation);
        assert_eq!(text.matches(" c\n").count(), 4, "{text}");
    }
}
