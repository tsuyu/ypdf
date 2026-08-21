//! The little of XMP that PDF/A conformance turns on.
//!
//! A full XMP implementation is an RDF parser and a pile of schemas. What is
//! needed here is narrow: which part and conformance level the file claims,
//! and whether the title it carries in XMP agrees with the one in `/Info`.
//! Everything else is left to a real validator, and said so in
//! [`crate::LIMITS`].

use crate::model::{Conformance, Level};

/// The PDF/A level a packet claims, if it claims one.
///
/// Both spellings are accepted, because both are in the wild: XMP allows a
/// property to be an element or an attribute of `rdf:Description`, and
/// producers disagree about which to write.
#[must_use]
pub fn claimed_level(packet: &str) -> Option<Level> {
    let part = element(packet, "pdfaid:part")
        .or_else(|| attribute(packet, "pdfaid:part"))?
        .trim()
        .parse::<u8>()
        .ok()?;
    let letter = element(packet, "pdfaid:conformance")
        .or_else(|| attribute(packet, "pdfaid:conformance"))
        .unwrap_or_default();
    let conformance = match letter.trim().to_ascii_lowercase().chars().next()? {
        'a' => Conformance::A,
        'u' => Conformance::U,
        _ => Conformance::B,
    };
    Level::new(part, conformance).ok()
}

/// The `dc:title` from a packet, if it has one.
///
/// The title is stored as a language alternative, so the text sits inside an
/// `rdf:li` rather than in `dc:title` directly.
#[must_use]
pub fn title(packet: &str) -> Option<String> {
    let block = element(packet, "dc:title")?;
    let text = element(&block, "rdf:li").unwrap_or(block);
    let text = text.trim();
    (!text.is_empty()).then(|| unescape(text))
}

/// The text inside `<tag ...>…</tag>`.
fn element(packet: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let start = packet.find(&open)?;
    // Skip the rest of the opening tag, attributes included.
    let after_open = packet[start..].find('>')? + start + 1;
    if packet[start..after_open].ends_with("/>") {
        return None;
    }
    let close = format!("</{tag}>");
    let end = packet[after_open..].find(&close)? + after_open;
    Some(packet[after_open..end].to_string())
}

/// The value of `name="…"` or `name='…'`.
fn attribute(packet: &str, name: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let needle = format!("{name}={quote}");
        if let Some(start) = packet.find(&needle) {
            let from = start + needle.len();
            if let Some(len) = packet[from..].find(quote) {
                return Some(packet[from..from + len].to_string());
            }
        }
    }
    None
}

/// The five XML entities. Enough for a title.
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ELEMENTS: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/">
   <pdfaid:part>2</pdfaid:part>
   <pdfaid:conformance>B</pdfaid:conformance>
  </rdf:Description>
  <rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:title><rdf:Alt><rdf:li xml:lang="x-default">Q1 &amp; Q2</rdf:li></rdf:Alt></dc:title>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    const ATTRIBUTES: &str =
        r#"<rdf:Description rdf:about="" pdfaid:part="3" pdfaid:conformance="U"/>"#;

    #[test]
    fn a_claim_written_as_elements_is_read() {
        let level = claimed_level(ELEMENTS).expect("claims a level");
        assert_eq!(level.part, 2);
        assert_eq!(level.conformance, Conformance::B);
    }

    #[test]
    fn a_claim_written_as_attributes_is_read() {
        // Both spellings are legal XMP, and both ship in real files.
        let level = claimed_level(ATTRIBUTES).expect("claims a level");
        assert_eq!(level.part, 3);
        assert_eq!(level.conformance, Conformance::U);
    }

    #[test]
    fn a_packet_with_no_claim_claims_nothing() {
        assert!(claimed_level("<x:xmpmeta/>").is_none());
    }

    #[test]
    fn a_title_comes_out_of_its_language_alternative_unescaped() {
        assert_eq!(title(ELEMENTS).as_deref(), Some("Q1 & Q2"));
    }

    #[test]
    fn a_claim_of_a_level_that_does_not_exist_is_not_a_claim() {
        // PDF/A-1u has never existed. Reading it as 1b would invent a claim the
        // file never made.
        let packet = r#"<rdf:Description pdfaid:part="1" pdfaid:conformance="U"/>"#;
        assert!(claimed_level(packet).is_none());
    }
}
