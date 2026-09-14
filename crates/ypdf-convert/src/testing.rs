//! Synthetic pages for the tests.
//!
//! The whole point of taking a [`PageText`] rather than a file is that the
//! structure rebuilding can be tested against pages laid out by hand, where
//! the intended answer is known exactly. PDFium is exercised separately, in
//! `ypdf-render`'s own tests.

use ypdf_render::{CharBox, FontFace, PageText, RectPt};

/// One character and whether it is bold.
pub type Glyph = (CharBox, bool);

/// Lay a string out left to right from `x`, sitting on baseline `y`.
///
/// Every glyph is half its point size wide, which is close enough to a real
/// proportional face for gap arithmetic to mean something.
pub fn placed_line(text: &str, x: f32, y: f32, size: f32) -> Vec<Glyph> {
    let advance = size * 0.5;
    text.chars()
        .enumerate()
        .map(|(i, ch)| {
            #[expect(clippy::cast_precision_loss, reason = "test fixture")]
            let left = x + i as f32 * advance;
            (
                CharBox {
                    ch,
                    rect: RectPt {
                        left,
                        right: left + advance,
                        bottom: y,
                        top: y + size,
                    },
                    size,
                    font: 0,
                },
                false,
            )
        })
        .collect()
}

/// Build a page from lines of glyphs.
///
/// The font table has two entries — regular and bold — and each glyph's flag
/// picks between them, which is exactly the shape a real page arrives in.
#[must_use]
pub fn page(lines: &[Vec<Glyph>]) -> PageText {
    let fonts = vec![
        FontFace {
            name: "Test-Regular".to_string(),
            weight: Some(400),
            ..FontFace::default()
        },
        FontFace {
            name: "Test-Bold".to_string(),
            weight: Some(700),
            ..FontFace::default()
        },
    ];

    let chars = lines
        .iter()
        .flatten()
        .map(|(c, bold)| CharBox {
            font: u16::from(*bold),
            ..*c
        })
        .collect();

    PageText { chars, fonts }
}

/// Mark every glyph of a line bold.
pub fn bold(mut line: Vec<Glyph>) -> Vec<Glyph> {
    for glyph in &mut line {
        glyph.1 = true;
    }
    line
}
