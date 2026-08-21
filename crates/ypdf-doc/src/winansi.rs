//! WinAnsiEncoding: turning Rust text into something a base-14 font can show.
//!
//! Two crates need this and both need it to behave identically: the OCR text
//! layer and the watermark. It refuses what it cannot represent rather than
//! substituting question marks — a caller that knows a word was dropped can say
//! so, while one handed mojibake cannot tell that anything went wrong.

/// Encode a word as a PDF literal string in WinAnsi, or refuse it.
///
/// Refusing is the point: a word written in a font that cannot encode it comes
/// back out of the document as mojibake, and mojibake in a text layer is worse
/// than nothing because it looks like content.
pub fn encode_literal(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len() + 4);

    for ch in text.chars() {
        let code = code(ch)?;
        match code {
            // Escape what would otherwise end the string or start an escape.
            b'(' | b')' | b'\\' => {
                out.push('\\');
                out.push(char::from(code));
            }
            0x20..=0x7e => out.push(char::from(code)),
            // Anything else goes in as an octal escape, which is always safe.
            _ => out.push_str(&format!("\\{code:03o}")),
        }
    }

    Some(out)
}

/// The WinAnsi code for a character, if it has one.
pub fn code(ch: char) -> Option<u8> {
    // WinAnsiEncoding is Latin-1 with a different 0x80-0x9F range. Latin-1 is
    // handled directly; the handful of characters in the upper block that
    // differ are mapped by hand, and everything else is refused.
    match ch {
        '\u{20ac}' => Some(0x80),
        '\u{201a}' => Some(0x82),
        '\u{0192}' => Some(0x83),
        '\u{201e}' => Some(0x84),
        '\u{2026}' => Some(0x85),
        '\u{2020}' => Some(0x86),
        '\u{2021}' => Some(0x87),
        '\u{02c6}' => Some(0x88),
        '\u{2030}' => Some(0x89),
        '\u{0160}' => Some(0x8a),
        '\u{2039}' => Some(0x8b),
        '\u{0152}' => Some(0x8c),
        '\u{017d}' => Some(0x8e),
        '\u{2018}' => Some(0x91),
        '\u{2019}' => Some(0x92),
        '\u{201c}' => Some(0x93),
        '\u{201d}' => Some(0x94),
        '\u{2022}' => Some(0x95),
        '\u{2013}' => Some(0x96),
        '\u{2014}' => Some(0x97),
        '\u{02dc}' => Some(0x98),
        '\u{2122}' => Some(0x99),
        '\u{0161}' => Some(0x9a),
        '\u{203a}' => Some(0x9b),
        '\u{0153}' => Some(0x9c),
        '\u{017e}' => Some(0x9e),
        '\u{0178}' => Some(0x9f),
        // The unused WinAnsi slots; Latin-1 has control codes there.
        '\u{0080}'..='\u{009f}' => None,
        ch if (ch as u32) < 0x100 => u8::try_from(ch as u32).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parentheses_and_backslashes_cannot_break_a_content_stream() {
        // Unescaped, these end the string early and the rest of the text is
        // parsed as operators.
        let encoded = encode_literal("a(b)c\\d").expect("encodable");
        assert_eq!(encoded, "a\\(b\\)c\\\\d");
    }

    #[test]
    fn latin_text_survives_unchanged() {
        assert_eq!(
            encode_literal("CONFIDENTIAL").as_deref(),
            Some("CONFIDENTIAL")
        );
    }

    #[test]
    fn accented_characters_become_octal_escapes() {
        let encoded = encode_literal("Caf\u{e9}").expect("encodable");
        assert_eq!(encoded, "Caf\\351");
    }

    #[test]
    fn the_windows_specific_block_is_mapped_rather_than_taken_as_latin_1() {
        // WinAnsi puts typographic punctuation where Latin-1 has control codes.
        assert_eq!(code('\u{2019}'), Some(0x92), "right single quote");
        assert_eq!(code('\u{20ac}'), Some(0x80), "euro sign");
        assert_eq!(code('\u{0081}'), None, "an unused slot is not a character");
    }

    #[test]
    fn scripts_this_encoding_cannot_carry_are_refused() {
        // Refusing is the point: substituting would produce text that looks
        // like data and is not.
        assert_eq!(encode_literal("\u{6c49}\u{5b57}"), None);
        assert_eq!(
            encode_literal("\u{0645}\u{0631}\u{062d}\u{0628}\u{0627}"),
            None
        );
    }
}
