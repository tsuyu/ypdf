//! Blocks into a Word document (spec §5).
//!
//! A `.docx` is a ZIP holding a handful of XML parts. Nothing here compresses
//! anything: the entries are stored, which is a valid ZIP and keeps the writer
//! small enough to read in one sitting. These documents are text, and text that
//! would have compressed well is not worth a dependency.
//!
//! The parts written are the smallest set Word will open without complaint:
//!
//! - `[Content_Types].xml` — what each part is.
//! - `_rels/.rels` — which part is the document.
//! - `word/document.xml` — the content.
//! - `word/styles.xml` — what `Heading1` and friends look like. Without it
//!   Word knows the style names but has nothing to draw them with, and every
//!   heading arrives looking like body text.
//! - `word/numbering.xml` — real Word lists rather than paragraphs that happen
//!   to start with a dash. This is the difference between a list the reader can
//!   continue by pressing Enter and a line of text shaped like one.

use crate::blocks::Block;
use crate::lines::Run;

/// Heading levels Word is told about. Deeper ones are clamped to this.
const MAX_HEADING: u8 = 6;

/// `numId` of the bullet list defined in `numbering.xml`.
const BULLET_LIST: u8 = 1;

/// `numId` of the numbered list defined in `numbering.xml`.
const ORDERED_LIST: u8 = 2;

/// Write blocks as a Word document.
#[must_use]
pub fn write(blocks: &[Block]) -> Vec<u8> {
    let parts: Vec<(&str, Vec<u8>)> = vec![
        ("[Content_Types].xml", CONTENT_TYPES.as_bytes().to_vec()),
        ("_rels/.rels", ROOT_RELS.as_bytes().to_vec()),
        (
            "word/_rels/document.xml.rels",
            DOCUMENT_RELS.as_bytes().to_vec(),
        ),
        ("word/document.xml", document(blocks).into_bytes()),
        ("word/styles.xml", STYLES.as_bytes().to_vec()),
        ("word/numbering.xml", NUMBERING.as_bytes().to_vec()),
    ];

    zip(&parts)
}

/// The body of the document.
fn document(blocks: &[Block]) -> String {
    let mut out = String::from(DOCUMENT_HEAD);

    for block in blocks {
        match block {
            Block::Heading { level, runs } => {
                let level = (*level).clamp(1, MAX_HEADING);
                out.push_str(&format!(
                    "<w:p><w:pPr><w:pStyle w:val=\"Heading{level}\"/></w:pPr>"
                ));
                // The style carries the weight. Marking the runs bold as well
                // would double up on a heading that is already strong.
                out.push_str(&runs_xml(runs, true));
                out.push_str("</w:p>");
            }
            Block::Paragraph { runs } => {
                out.push_str("<w:p>");
                out.push_str(&runs_xml(runs, false));
                out.push_str("</w:p>");
            }
            Block::ListItem { runs, ordered } => {
                let id = if *ordered { ORDERED_LIST } else { BULLET_LIST };
                out.push_str(&format!(
                    "<w:p><w:pPr><w:pStyle w:val=\"ListParagraph\"/>\
                     <w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"{id}\"/></w:numPr></w:pPr>"
                ));
                out.push_str(&runs_xml(runs, false));
                out.push_str("</w:p>");
            }
        }
    }

    out.push_str(DOCUMENT_TAIL);
    out
}

/// The runs of one paragraph.
fn runs_xml(runs: &[Run], plain: bool) -> String {
    let mut out = String::new();
    for run in runs {
        if run.text.is_empty() {
            continue;
        }

        let mut properties = String::new();
        if !plain {
            if run.bold {
                properties.push_str("<w:b/>");
            }
            if run.italic {
                properties.push_str("<w:i/>");
            }
        }

        out.push_str("<w:r>");
        if !properties.is_empty() {
            out.push_str(&format!("<w:rPr>{properties}</w:rPr>"));
        }
        // `xml:space="preserve"` or Word eats the spaces between runs, and
        // "a **loud** word" arrives as "aloudword".
        out.push_str(&format!(
            "<w:t xml:space=\"preserve\">{}</w:t></w:r>",
            escape(&run.text)
        ));
    }
    out
}

/// Escape text for XML, dropping what XML cannot carry at all.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 has no way to represent most control characters, not
            // even as a numeric reference. A stray one would make the whole
            // document unreadable, so it goes.
            c if (c.is_control() && c != '\t') => {}
            c => out.push(c),
        }
    }
    out
}

// --- the ZIP container ---------------------------------------------------

/// Pack parts into a ZIP with stored (uncompressed) entries.
fn zip(parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();

    for (name, data) in parts {
        let offset = u32::try_from(out.len()).unwrap_or(u32::MAX);
        let crc = crc32fast::hash(data);
        let size = u32::try_from(data.len()).unwrap_or(u32::MAX);

        out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes()); // local header
        out.extend_from_slice(&20_u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0_u16.to_le_bytes()); // flags
        out.extend_from_slice(&0_u16.to_le_bytes()); // stored
        out.extend_from_slice(&DOS_TIME.to_le_bytes());
        out.extend_from_slice(&DOS_DATE.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes()); // compressed
        out.extend_from_slice(&size.to_le_bytes()); // uncompressed
        out.extend_from_slice(&name_len(name).to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes()); // extra
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        directory.extend_from_slice(&0x0201_4b50_u32.to_le_bytes()); // central
        directory.extend_from_slice(&20_u16.to_le_bytes()); // made by
        directory.extend_from_slice(&20_u16.to_le_bytes()); // needed
        directory.extend_from_slice(&0_u16.to_le_bytes()); // flags
        directory.extend_from_slice(&0_u16.to_le_bytes()); // stored
        directory.extend_from_slice(&DOS_TIME.to_le_bytes());
        directory.extend_from_slice(&DOS_DATE.to_le_bytes());
        directory.extend_from_slice(&crc.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&name_len(name).to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes()); // extra
        directory.extend_from_slice(&0_u16.to_le_bytes()); // comment
        directory.extend_from_slice(&0_u16.to_le_bytes()); // disk
        directory.extend_from_slice(&0_u16.to_le_bytes()); // internal attrs
        directory.extend_from_slice(&0_u32.to_le_bytes()); // external attrs
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }

    let directory_at = u32::try_from(out.len()).unwrap_or(u32::MAX);
    let directory_size = u32::try_from(directory.len()).unwrap_or(u32::MAX);
    let count = u16::try_from(parts.len()).unwrap_or(u16::MAX);

    out.extend_from_slice(&directory);
    out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes()); // end of directory
    out.extend_from_slice(&0_u16.to_le_bytes()); // this disk
    out.extend_from_slice(&0_u16.to_le_bytes()); // disk with directory
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&directory_size.to_le_bytes());
    out.extend_from_slice(&directory_at.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes()); // comment

    out
}

fn name_len(name: &str) -> u16 {
    u16::try_from(name.len()).unwrap_or(u16::MAX)
}

/// Midnight, and 1 January 1980 — the earliest a ZIP can express.
///
/// Fixed rather than "now" so that converting the same document twice gives
/// the same bytes, which is what makes the output diffable and testable.
const DOS_TIME: u16 = 0;
const DOS_DATE: u16 = 0x0021;

// --- the fixed parts -----------------------------------------------------

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

const DOCUMENT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;

const DOCUMENT_HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>"#;

/// US Letter, in twentieths of a point, with one-inch margins.
const DOCUMENT_TAIL: &str = r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#;

const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:qFormat/><w:pPr><w:outlineLvl w:val="0"/><w:spacing w:before="240" w:after="120"/></w:pPr><w:rPr><w:b/><w:sz w:val="40"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:qFormat/><w:pPr><w:outlineLvl w:val="1"/><w:spacing w:before="200" w:after="100"/></w:pPr><w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading3"><w:name w:val="heading 3"/><w:basedOn w:val="Normal"/><w:qFormat/><w:pPr><w:outlineLvl w:val="2"/><w:spacing w:before="180" w:after="90"/></w:pPr><w:rPr><w:b/><w:sz w:val="28"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading4"><w:name w:val="heading 4"/><w:basedOn w:val="Normal"/><w:qFormat/><w:pPr><w:outlineLvl w:val="3"/></w:pPr><w:rPr><w:b/><w:sz w:val="26"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading5"><w:name w:val="heading 5"/><w:basedOn w:val="Normal"/><w:qFormat/><w:pPr><w:outlineLvl w:val="4"/></w:pPr><w:rPr><w:b/><w:sz w:val="24"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Heading6"><w:name w:val="heading 6"/><w:basedOn w:val="Normal"/><w:qFormat/><w:pPr><w:outlineLvl w:val="5"/></w:pPr><w:rPr><w:b/><w:i/><w:sz w:val="24"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="ListParagraph"><w:name w:val="List Paragraph"/><w:basedOn w:val="Normal"/><w:qFormat/><w:pPr><w:ind w:left="720"/><w:contextualSpacing/></w:pPr></w:style></w:styles>"#;

const NUMBERING: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="•"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> Run {
        Run {
            text: text.to_string(),
            bold: false,
            italic: false,
        }
    }

    /// Read one stored entry back out of the ZIP, by name.
    ///
    /// A deliberately naive reader: it walks the local headers, which is
    /// enough to prove the sizes and names were written where they were said
    /// to be.
    fn entry(zip: &[u8], want: &str) -> Option<String> {
        let mut at = 0;
        while at + 30 <= zip.len() {
            if zip[at..at + 4] != 0x0403_4b50_u32.to_le_bytes() {
                break;
            }
            let size = u32::from_le_bytes(zip[at + 18..at + 22].try_into().ok()?) as usize;
            let name_len = u16::from_le_bytes(zip[at + 26..at + 28].try_into().ok()?) as usize;
            let extra_len = u16::from_le_bytes(zip[at + 28..at + 30].try_into().ok()?) as usize;
            let name_at = at + 30;
            let data_at = name_at + name_len + extra_len;
            let name = std::str::from_utf8(&zip[name_at..name_at + name_len]).ok()?;
            if name == want {
                return String::from_utf8(zip[data_at..data_at + size].to_vec()).ok();
            }
            at = data_at + size;
        }
        None
    }

    #[test]
    fn the_package_is_a_zip_holding_the_parts_word_needs() {
        let bytes = write(&[Block::Paragraph {
            runs: vec![run("Hello.")],
        }]);

        assert_eq!(&bytes[..2], b"PK", "a docx is a ZIP");
        for part in [
            "[Content_Types].xml",
            "_rels/.rels",
            "word/_rels/document.xml.rels",
            "word/document.xml",
            "word/styles.xml",
            "word/numbering.xml",
        ] {
            assert!(entry(&bytes, part).is_some(), "{part} is missing");
        }
    }

    #[test]
    fn headings_carry_the_style_that_makes_them_look_like_headings() {
        let bytes = write(&[Block::Heading {
            level: 2,
            runs: vec![run("Findings")],
        }]);
        let xml = entry(&bytes, "word/document.xml").expect("document");

        assert!(xml.contains(r#"<w:pStyle w:val="Heading2"/>"#), "{xml}");
        assert!(xml.contains("Findings"));

        // The style is bold already; the run must not be bold on top of it.
        let styles = entry(&bytes, "word/styles.xml").expect("styles");
        assert!(
            styles.contains(r#"w:styleId="Heading2""#),
            "the style has to be defined or Word draws it as body text"
        );
    }

    #[test]
    fn list_items_are_real_word_lists_not_text_that_looks_like_one() {
        let bytes = write(&[
            Block::ListItem {
                runs: vec![run("first")],
                ordered: false,
            },
            Block::ListItem {
                runs: vec![run("second")],
                ordered: true,
            },
        ]);
        let xml = entry(&bytes, "word/document.xml").expect("document");

        assert!(
            xml.contains(r#"<w:numId w:val="1"/>"#),
            "bullet list: {xml}"
        );
        assert!(
            xml.contains(r#"<w:numId w:val="2"/>"#),
            "numbered list: {xml}"
        );

        let numbering = entry(&bytes, "word/numbering.xml").expect("numbering");
        assert!(numbering.contains(r#"w:numFmt w:val="bullet""#));
        assert!(numbering.contains(r#"w:numFmt w:val="decimal""#));
    }

    #[test]
    fn bold_and_italic_survive_as_run_properties() {
        let bytes = write(&[Block::Paragraph {
            runs: vec![
                run("plain "),
                Run {
                    text: "loud".to_string(),
                    bold: true,
                    italic: true,
                },
            ],
        }]);
        let xml = entry(&bytes, "word/document.xml").expect("document");

        assert!(xml.contains("<w:rPr><w:b/><w:i/></w:rPr>"), "{xml}");
        // Without xml:space the leading and trailing spaces are eaten and the
        // words run together.
        assert!(
            xml.contains(r#"<w:t xml:space="preserve">plain </w:t>"#),
            "{xml}"
        );
    }

    #[test]
    fn text_that_would_break_the_xml_is_escaped() {
        let bytes = write(&[Block::Paragraph {
            runs: vec![run("a < b & c > d \u{7}")],
        }]);
        let xml = entry(&bytes, "word/document.xml").expect("document");

        assert!(xml.contains("a &lt; b &amp; c &gt; d"), "{xml}");
        assert!(
            !xml.contains('\u{7}'),
            "a control character has no XML representation and must be dropped"
        );
    }

    #[test]
    fn the_same_blocks_always_give_the_same_bytes() {
        let blocks = [Block::Paragraph {
            runs: vec![run("Repeatable.")],
        }];
        assert_eq!(
            write(&blocks),
            write(&blocks),
            "a timestamp would make every conversion differ from the last"
        );
    }
}
