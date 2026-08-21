//! Walking a page's content stream and taking things out of it.
//!
//! This is the part that makes a redaction a redaction. The page's operators
//! are decoded, the graphics and text state are tracked well enough to know
//! where every glyph lands, and the ones inside a redaction rectangle are
//! **deleted from the stream**. What is written back has no record of them.
//!
//! Removed glyphs are replaced by the kerning that would have advanced past
//! them, so the words on either side stay exactly where they were. A redaction
//! that reflowed the line would be obvious at a glance, and obvious in a way
//! that tells a reader how long the removed text was.
//!
//! Where a glyph cannot be measured — a composite font whose CMap this crate
//! does not read — the **whole show operation** goes. Removing too much is
//! visible to anyone who looks at the result; removing too little is not.

use std::collections::HashMap;

use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, ObjectId};

use crate::fonts::Font;
use crate::geometry::{Matrix, Rect};

/// What the walk removed from one page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Removed {
    /// Glyphs deleted individually.
    pub glyphs: usize,
    /// Show operations dropped whole, because their font could not be split.
    pub coarse_runs: usize,
    /// Path-painting operations dropped because they fell inside a redaction.
    pub paths: usize,
    /// Image draws dropped because the image could not be edited.
    pub image_draws: usize,
}

/// The state a content stream carries as it runs.
#[derive(Clone, Copy, Debug)]
struct State {
    ctm: Matrix,
    font_size: f32,
    char_spacing: f32,
    word_spacing: f32,
    horizontal_scale: f32,
    rise: f32,
    leading: f32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 1.0,
            rise: 0.0,
            leading: 0.0,
        }
    }
}

/// An image the page draws, and where.
#[derive(Clone, Debug)]
pub struct ImageDraw {
    /// The resource name it was drawn under.
    pub name: Vec<u8>,
    /// The matrix in force when it was drawn.
    pub matrix: Matrix,
    /// Where it landed on the page.
    pub bounds: Rect,
}

/// The result of walking one page.
pub struct Walk {
    /// The rewritten operations.
    pub operations: Vec<Operation>,
    /// What was taken out.
    pub removed: Removed,
    /// Images that overlap a redaction, for the caller to edit or drop.
    pub images: Vec<ImageDraw>,
}

/// Rewrite a page's content, removing everything inside `redactions`.
pub fn walk(
    document: &Document,
    page_id: ObjectId,
    content: &Content,
    redactions: &[Rect],
    fonts: &HashMap<Vec<u8>, Font>,
) -> Walk {
    let _ = (document, page_id);

    let mut out = Vec::with_capacity(content.operations.len());
    let mut removed = Removed::default();
    let mut images = Vec::new();

    let mut state = State::default();
    let mut stack: Vec<State> = Vec::new();
    let mut font: Option<Font> = None;
    let mut text_matrix = Matrix::IDENTITY;
    let mut line_matrix = Matrix::IDENTITY;
    let mut path_points: Vec<(f32, f32)> = Vec::new();

    for operation in &content.operations {
        let numbers = operands(&operation.operands);

        match operation.operator.as_str() {
            "q" => stack.push(state),
            "Q" => state = stack.pop().unwrap_or_default(),
            "cm" => {
                if let [a, b, c, d, e, f] = numbers[..] {
                    state.ctm = Matrix::new(a, b, c, d, e, f).then(state.ctm);
                }
            }

            "BT" => {
                text_matrix = Matrix::IDENTITY;
                line_matrix = Matrix::IDENTITY;
            }
            "Tf" => {
                if let Some(name) = operation.operands.first().and_then(as_name) {
                    font = fonts.get(&name).cloned();
                }
                state.font_size = numbers.last().copied().unwrap_or(state.font_size);
            }
            "Tc" => state.char_spacing = numbers.first().copied().unwrap_or(0.0),
            "Tw" => state.word_spacing = numbers.first().copied().unwrap_or(0.0),
            "Tz" => state.horizontal_scale = numbers.first().copied().unwrap_or(100.0) / 100.0,
            "TL" => state.leading = numbers.first().copied().unwrap_or(0.0),
            "Ts" => state.rise = numbers.first().copied().unwrap_or(0.0),
            "Tm" => {
                if let [a, b, c, d, e, f] = numbers[..] {
                    text_matrix = Matrix::new(a, b, c, d, e, f);
                    line_matrix = text_matrix;
                }
            }
            "Td" => {
                if let [x, y] = numbers[..] {
                    line_matrix = Matrix::translate(x, y).then(line_matrix);
                    text_matrix = line_matrix;
                }
            }
            "TD" => {
                if let [x, y] = numbers[..] {
                    state.leading = -y;
                    line_matrix = Matrix::translate(x, y).then(line_matrix);
                    text_matrix = line_matrix;
                }
            }
            "T*" => {
                line_matrix = Matrix::translate(0.0, -state.leading).then(line_matrix);
                text_matrix = line_matrix;
            }

            "Tj" | "'" | "\"" | "TJ" => {
                // The quote operators move to the next line first, and the
                // double quote also sets spacing.
                if operation.operator == "'" || operation.operator == "\"" {
                    if operation.operator == "\""
                        && let [word, char_spacing] = numbers[..2.min(numbers.len())]
                    {
                        state.word_spacing = word;
                        state.char_spacing = char_spacing;
                    }
                    line_matrix = Matrix::translate(0.0, -state.leading).then(line_matrix);
                    text_matrix = line_matrix;
                }

                let rewritten = show(
                    operation,
                    &state,
                    font.as_ref(),
                    &mut text_matrix,
                    redactions,
                    &mut removed,
                );
                if let Some(operation) = rewritten {
                    out.push(operation);
                }
                continue;
            }

            // Path construction: remembered so a path entirely inside a
            // redaction can be dropped when it is painted.
            "m" | "l" => {
                if let [x, y] = numbers[..] {
                    path_points.push(state.ctm.apply(x, y));
                }
            }
            "c" | "v" | "y" => {
                for pair in numbers.chunks_exact(2) {
                    if let [x, y] = pair {
                        path_points.push(state.ctm.apply(*x, *y));
                    }
                }
            }
            "re" => {
                if let [x, y, w, h] = numbers[..] {
                    for (px, py) in [(x, y), (x + w, y), (x + w, y + h), (x, y + h)] {
                        path_points.push(state.ctm.apply(px, py));
                    }
                }
            }
            "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" => {
                let inside = Rect::around(&path_points)
                    .is_some_and(|bounds| redactions.iter().any(|r| r.contains(bounds)));
                path_points.clear();
                if inside {
                    // Painted entirely within a redaction: it is part of what
                    // was covered, so it goes.
                    removed.paths += 1;
                    // Drop the painting operator, and paint nothing instead, so
                    // the path built above does not leak into the next one.
                    out.push(Operation::new("n", Vec::new()));
                    continue;
                }
            }

            "Do" => {
                if let Some(name) = operation.operands.first().and_then(as_name) {
                    // The unit square, transformed: where the image landed.
                    let corners = [
                        state.ctm.apply(0.0, 0.0),
                        state.ctm.apply(1.0, 0.0),
                        state.ctm.apply(1.0, 1.0),
                        state.ctm.apply(0.0, 1.0),
                    ];
                    if let Some(bounds) = Rect::around(&corners)
                        && redactions.iter().any(|r| r.intersects(bounds))
                    {
                        images.push(ImageDraw {
                            name,
                            matrix: state.ctm,
                            bounds,
                        });
                    }
                }
            }

            _ => {}
        }

        out.push(operation.clone());
    }

    Walk {
        operations: out,
        removed,
        images,
    }
}

/// Rewrite one text-showing operation.
///
/// Returns the operation to keep, or `None` when nothing of it survived.
fn show(
    operation: &Operation,
    state: &State,
    font: Option<&Font>,
    text_matrix: &mut Matrix,
    redactions: &[Rect],
    removed: &mut Removed,
) -> Option<Operation> {
    let items = show_items(operation);
    let coarse = font.is_none_or(|font| font.coarse);

    let mut kept: Vec<Object> = Vec::new();
    let mut any_removed = false;
    let mut run: Vec<u8> = Vec::new();

    for item in items {
        match item {
            ShowItem::Kern(amount) => {
                flush(&mut run, &mut kept);
                kept.push(Object::Real(amount));
                // A kern moves the pen without drawing.
                let shift = -amount / 1000.0 * state.font_size * state.horizontal_scale;
                *text_matrix = Matrix::translate(shift, 0.0).then(*text_matrix);
            }
            ShowItem::Text(bytes) => {
                for byte in bytes {
                    let code = u32::from(byte);
                    let width = font.map_or(500.0, |font| font.width(code));
                    let advance = (width / 1000.0 * state.font_size
                        + state.char_spacing
                        + if byte == b' ' {
                            state.word_spacing
                        } else {
                            0.0
                        })
                        * state.horizontal_scale;

                    let box_ = glyph_box(state, *text_matrix, width, advance);
                    let hit = redactions.iter().any(|rect| rect.intersects(box_));

                    if hit && coarse {
                        // The string cannot be split safely, so none of it is
                        // kept. Over-removal is the safe direction to err in.
                        removed.coarse_runs += 1;
                        return None;
                    }

                    if hit {
                        flush(&mut run, &mut kept);
                        // Replace the glyph with the kern that advances past
                        // it, so what is left does not move.
                        let kern = kern_for(advance, state);
                        kept.push(Object::Real(kern));
                        removed.glyphs += 1;
                        any_removed = true;
                    } else {
                        run.push(byte);
                    }

                    *text_matrix = Matrix::translate(advance, 0.0).then(*text_matrix);
                }
            }
        }
    }
    flush(&mut run, &mut kept);

    if !any_removed {
        return Some(operation.clone());
    }
    if kept
        .iter()
        .all(|object| matches!(object, Object::Real(_) | Object::Integer(_)))
    {
        // Only spacing left: nothing is drawn, so nothing needs to be.
        return None;
    }

    // Everything becomes a TJ array, which is the only form that can carry both
    // text and the spacing that stands in for what was removed.
    Some(Operation::new("TJ", vec![Object::Array(kept)]))
}

/// The kerning number that advances the pen by `advance` points.
fn kern_for(advance: f32, state: &State) -> f32 {
    let scale = state.font_size * state.horizontal_scale;
    if scale.abs() < f32::EPSILON {
        return 0.0;
    }
    -advance / scale * 1000.0
}

fn flush(run: &mut Vec<u8>, kept: &mut Vec<Object>) {
    if !run.is_empty() {
        kept.push(Object::string_literal(std::mem::take(run)));
    }
}

/// Where one glyph lands on the page.
fn glyph_box(state: &State, text_matrix: Matrix, width: f32, advance: f32) -> Rect {
    let trm = Matrix::new(
        state.font_size * state.horizontal_scale,
        0.0,
        0.0,
        state.font_size,
        0.0,
        state.rise,
    )
    .then(text_matrix)
    .then(state.ctm);

    // In text space the glyph runs from the origin to its advance, and from a
    // little below the baseline to the cap height. Exact ascent and descent are
    // per-font; these cover the common range and err on the large side, which
    // takes in a glyph that only just touches the rectangle.
    let width_em = if width > 0.0 { width / 1000.0 } else { advance };
    let corners = [
        trm.apply(0.0, -0.25),
        trm.apply(width_em, -0.25),
        trm.apply(width_em, 0.9),
        trm.apply(0.0, 0.9),
    ];
    Rect::around(&corners).unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0))
}

/// One element of a show operation.
enum ShowItem {
    /// Bytes to draw.
    Text(Vec<u8>),
    /// A spacing adjustment, in 1/1000 em.
    Kern(f32),
}

/// Flatten any of the four show operators into a list of items.
fn show_items(operation: &Operation) -> Vec<ShowItem> {
    match operation.operator.as_str() {
        "TJ" => operation
            .operands
            .first()
            .and_then(|object| object.as_array().ok())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| match item {
                        Object::String(bytes, _) => Some(ShowItem::Text(bytes.clone())),
                        Object::Real(value) => Some(ShowItem::Kern(*value)),
                        #[expect(clippy::cast_precision_loss, reason = "kerning values are small")]
                        Object::Integer(value) => Some(ShowItem::Kern(*value as f32)),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        // The quote operators take the string last.
        _ => operation
            .operands
            .iter()
            .filter_map(|object| match object {
                Object::String(bytes, _) => Some(ShowItem::Text(bytes.clone())),
                _ => None,
            })
            .collect(),
    }
}

fn operands(objects: &[Object]) -> Vec<f32> {
    objects
        .iter()
        .filter_map(|object| object.as_float().ok())
        .collect()
}

fn as_name(object: &Object) -> Option<Vec<u8>> {
    object.as_name().ok().map(<[u8]>::to_vec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::Content;
    use lopdf::dictionary;

    fn helvetica() -> HashMap<Vec<u8>, Font> {
        let doc = Document::with_version("1.7");
        let dict = dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        };
        let font = Font::read(&doc, &dict);
        HashMap::from([(b"F1".to_vec(), font)])
    }

    fn walk_text(content: &str, redactions: &[Rect]) -> (String, Removed) {
        let decoded = Content::decode(content.as_bytes()).expect("parses");
        let doc = Document::with_version("1.7");
        let walk = walk(&doc, (1, 0), &decoded, redactions, &helvetica());
        let encoded = Content {
            operations: walk.operations,
        }
        .encode()
        .expect("encodes");
        (String::from_utf8_lossy(&encoded).into_owned(), walk.removed)
    }

    #[test]
    fn a_word_inside_the_rectangle_is_deleted_from_the_stream() {
        // At 12pt starting at x=100, "SECRET" runs to about x=150.
        let (out, removed) = walk_text(
            "BT /F1 12 Tf 100 700 Td (SECRET) Tj ET",
            &[Rect::new(95.0, 690.0, 160.0, 715.0)],
        );

        assert_eq!(removed.glyphs, 6);
        assert!(!out.contains("SECRET"), "{out}");
    }

    #[test]
    fn text_outside_the_rectangle_is_left_exactly_as_it_was() {
        let (out, removed) = walk_text(
            "BT /F1 12 Tf 100 700 Td (SECRET) Tj ET",
            &[Rect::new(0.0, 0.0, 50.0, 50.0)],
        );

        assert_eq!(removed, Removed::default());
        assert!(out.contains("(SECRET) Tj"), "{out}");
    }

    #[test]
    fn only_the_glyphs_inside_go_and_the_rest_stay_put() {
        // "KEEP SECRET" at 12pt from x=100: K/E/E/P are 8pt each and the space
        // 3.3pt, so "SECRET" starts around x=135. The rectangle begins at 136,
        // clear of the "P" — a glyph that ends exactly on the boundary is taken
        // as inside, since over-removal is the safe direction to err in.
        let (out, removed) = walk_text(
            "BT /F1 12 Tf 100 700 Td (KEEP SECRET) Tj ET",
            &[Rect::new(136.0, 690.0, 220.0, 715.0)],
        );

        assert!(removed.glyphs >= 6, "{removed:?}");
        assert!(
            out.contains("KEEP"),
            "the rest of the line must survive: {out}"
        );
        assert!(!out.contains("SECRET"), "{out}");
        // The kerning that replaces the removed glyphs keeps what is left in
        // place, so the survivors are followed by spacing numbers.
        assert!(out.contains("TJ"), "{out}");
    }

    #[test]
    fn a_show_operation_with_nothing_left_disappears_entirely() {
        let (out, _) = walk_text(
            "BT /F1 12 Tf 100 700 Td (SECRET) Tj ET",
            &[Rect::new(0.0, 0.0, 612.0, 792.0)],
        );
        assert!(!out.contains("Tj"), "{out}");
        assert!(!out.contains("TJ"), "{out}");
        assert!(out.contains("BT"), "the text block itself stays: {out}");
    }

    #[test]
    fn the_text_matrix_follows_td_so_later_lines_are_placed_correctly() {
        // Two lines; only the second is inside the rectangle. Getting the
        // matrix wrong would redact the wrong one.
        let (out, removed) = walk_text(
            "BT /F1 12 Tf 100 700 Td (FIRST) Tj 0 -20 Td (SECOND) Tj ET",
            &[Rect::new(95.0, 670.0, 200.0, 695.0)],
        );

        assert!(out.contains("FIRST"), "{out}");
        assert!(!out.contains("SECOND"), "{out}");
        assert!(removed.glyphs >= 6);
    }

    #[test]
    fn the_current_transformation_matrix_is_respected() {
        // The text is drawn at 50,350 but scaled 2x, so it lands at 100,700.
        let (out, _) = walk_text(
            "q 2 0 0 2 0 0 cm BT /F1 6 Tf 50 350 Td (SECRET) Tj ET Q",
            &[Rect::new(95.0, 690.0, 160.0, 715.0)],
        );
        assert!(!out.contains("SECRET"), "{out}");
    }

    #[test]
    fn a_saved_state_is_restored_so_a_transform_does_not_leak() {
        let (out, _) = walk_text(
            "q 2 0 0 2 0 0 cm Q BT /F1 12 Tf 100 700 Td (SECRET) Tj ET",
            &[Rect::new(95.0, 690.0, 160.0, 715.0)],
        );
        assert!(
            !out.contains("SECRET"),
            "the CTM should have been restored: {out}"
        );
    }

    #[test]
    fn a_tj_array_keeps_its_own_kerning_for_the_text_that_stays() {
        let (out, removed) = walk_text(
            "BT /F1 12 Tf 100 700 Td [(KEEP) -200 (SECRET)] TJ ET",
            &[Rect::new(135.0, 690.0, 220.0, 715.0)],
        );
        assert!(out.contains("KEEP"), "{out}");
        assert!(!out.contains("SECRET"), "{out}");
        assert!(removed.glyphs > 0);
    }

    #[test]
    fn a_path_entirely_inside_a_redaction_is_dropped() {
        let decoded = Content::decode(b"100 700 m 150 700 l S").expect("parses");
        let doc = Document::with_version("1.7");
        let walk = walk(
            &doc,
            (1, 0),
            &decoded,
            &[Rect::new(50.0, 650.0, 200.0, 750.0)],
            &helvetica(),
        );
        assert_eq!(walk.removed.paths, 1);
        assert!(walk.operations.iter().all(|op| op.operator != "S"));
    }

    #[test]
    fn a_path_crossing_the_edge_is_kept_for_the_cover_to_deal_with() {
        // Splitting a curve is not something this can do safely, so it is left
        // and the opaque cover rectangle hides it.
        let decoded = Content::decode(b"0 700 m 500 700 l S").expect("parses");
        let doc = Document::with_version("1.7");
        let walk = walk(
            &doc,
            (1, 0),
            &decoded,
            &[Rect::new(50.0, 650.0, 200.0, 750.0)],
            &helvetica(),
        );
        assert_eq!(walk.removed.paths, 0);
        assert!(walk.operations.iter().any(|op| op.operator == "S"));
    }

    #[test]
    fn an_image_that_overlaps_is_reported_with_where_it_landed() {
        let decoded = Content::decode(b"q 200 0 0 100 100 600 cm /Im0 Do Q").expect("parses");
        let doc = Document::with_version("1.7");
        let walk = walk(
            &doc,
            (1, 0),
            &decoded,
            &[Rect::new(150.0, 650.0, 250.0, 690.0)],
            &helvetica(),
        );

        assert_eq!(walk.images.len(), 1);
        let drawn = &walk.images[0];
        assert_eq!(drawn.name, b"Im0");
        assert_eq!(drawn.bounds, Rect::new(100.0, 600.0, 300.0, 700.0));
    }

    #[test]
    fn an_image_well_away_from_the_redaction_is_not_reported() {
        let decoded = Content::decode(b"q 50 0 0 50 0 0 cm /Im0 Do Q").expect("parses");
        let doc = Document::with_version("1.7");
        let walk = walk(
            &doc,
            (1, 0),
            &decoded,
            &[Rect::new(300.0, 300.0, 400.0, 400.0)],
            &helvetica(),
        );
        assert!(walk.images.is_empty());
    }
}
