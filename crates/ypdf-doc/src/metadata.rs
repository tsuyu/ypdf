//! Document metadata (spec §13).
//!
//! Two places hold it and they disagree constantly: the `/Info` dictionary,
//! which is plain strings, and the XMP packet, which is XML. Both are read;
//! `/Info` is what gets written, because it is the one every reader agrees on,
//! and a stale XMP packet is flagged rather than silently rewritten.
//!
//! PDF text strings are either PDFDocEncoded bytes or UTF-16BE with a byte
//! order mark, and dates look like `D:20240305142201+08'00'`. Decoding both is
//! the entire difficulty here.

use lopdf::{Dictionary, Object};
use ypdf_core::Result;

use crate::pdf::Pdf;

/// Everything the metadata panel shows (spec §13).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    /// `/Title`.
    pub title: Option<String>,
    /// `/Author`.
    pub author: Option<String>,
    /// `/Subject`.
    pub subject: Option<String>,
    /// `/Keywords`.
    pub keywords: Option<String>,
    /// `/Creator` — the application the content came from.
    pub creator: Option<String>,
    /// `/Producer` — the library that wrote the PDF.
    pub producer: Option<String>,
    /// `/CreationDate`, normalized.
    pub created: Option<String>,
    /// `/ModDate`, normalized.
    pub modified: Option<String>,
    /// PDF version from the header, e.g. `"1.7"`.
    pub version: String,
    /// Page count.
    pub page_count: u32,
    /// Whether the file carries an XMP packet.
    pub has_xmp: bool,
}

/// The fields a user may edit (spec §13, "Edit").
///
/// `None` leaves a field alone; `Some("")` clears it. Anything not listed here
/// — `/Producer`, the dates — belongs to whatever wrote the file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetadataEdit {
    /// New `/Title`.
    pub title: Option<String>,
    /// New `/Author`.
    pub author: Option<String>,
    /// New `/Subject`.
    pub subject: Option<String>,
    /// New `/Keywords`.
    pub keywords: Option<String>,
}

impl MetadataEdit {
    /// Does this change anything?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.author.is_none()
            && self.subject.is_none()
            && self.keywords.is_none()
    }

    /// The edit that turns `from` into `to`, listing only what differs.
    #[must_use]
    pub fn diff(from: &Metadata, to: &Metadata) -> Self {
        fn changed(before: &Option<String>, after: &Option<String>) -> Option<String> {
            (before != after).then(|| after.clone().unwrap_or_default())
        }
        Self {
            title: changed(&from.title, &to.title),
            author: changed(&from.author, &to.author),
            subject: changed(&from.subject, &to.subject),
            keywords: changed(&from.keywords, &to.keywords),
        }
    }
}

impl Pdf {
    /// Read the document's metadata.
    #[must_use]
    pub fn metadata(&self) -> Metadata {
        let info = self.info_dictionary();
        let get = |key: &[u8]| info.as_ref().and_then(|d| text_entry(d, key));

        Metadata {
            title: get(b"Title"),
            author: get(b"Author"),
            subject: get(b"Subject"),
            keywords: get(b"Keywords"),
            creator: get(b"Creator"),
            producer: get(b"Producer"),
            created: get(b"CreationDate").map(|d| format_date(&d)),
            modified: get(b"ModDate").map(|d| format_date(&d)),
            version: self.raw().version.clone(),
            page_count: self.page_count(),
            has_xmp: self.xmp().is_some(),
        }
    }

    /// Apply an edit to the `/Info` dictionary.
    ///
    /// An empty value removes the key rather than storing an empty string: a
    /// reader shows "(none)" for a missing title and an empty box for a blank
    /// one, and the first is what clearing a field means.
    pub fn set_metadata(&mut self, edit: &MetadataEdit) -> Result<()> {
        if edit.is_empty() {
            return Ok(());
        }

        let id = self.info_dictionary_id();
        let fields: [(&[u8], &Option<String>); 4] = [
            (b"Title", &edit.title),
            (b"Author", &edit.author),
            (b"Subject", &edit.subject),
            (b"Keywords", &edit.keywords),
        ];

        let doc = self.raw_mut();
        let dict = doc
            .get_object_mut(id)
            .and_then(Object::as_dict_mut)
            .map_err(|e| crate::pdf::from_lopdf(&e, None))?;

        for (key, value) in fields {
            match value.as_deref() {
                None => {}
                Some("") => {
                    dict.remove(key);
                }
                // Written as UTF-16BE so non-ASCII titles survive; PDFDocEncoding
                // cannot represent most of them.
                Some(text) => dict.set(
                    key.to_vec(),
                    Object::String(encode_text(text), lopdf::StringFormat::Hexadecimal),
                ),
            }
        }

        Ok(())
    }

    /// The XMP packet, if the document has one (spec §13, "Advanced").
    #[must_use]
    pub fn xmp(&self) -> Option<String> {
        let doc = self.raw();
        let catalog = doc.catalog().ok()?;
        let id = catalog.get(b"Metadata").ok()?.as_reference().ok()?;
        let stream = doc.get_object(id).ok()?.as_stream().ok()?;
        // The packet is XML and is normally stored uncompressed, but nothing
        // requires that.
        let bytes = stream
            .decompressed_content()
            .unwrap_or_else(|_| stream.content.clone());
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Is the XMP packet inconsistent with `/Info`?
    ///
    /// Reported rather than fixed: rewriting someone's XMP to match a title
    /// they just typed is a bigger change than they asked for.
    #[must_use]
    pub fn xmp_disagrees_with_info(&self) -> bool {
        let Some(xmp) = self.xmp() else { return false };
        let Some(title) = self.metadata().title else {
            return false;
        };
        !xmp.contains(&title)
    }

    fn info_dictionary(&self) -> Option<Dictionary> {
        let doc = self.raw();
        let object = doc.trailer.get(b"Info").ok()?;
        match object {
            Object::Reference(id) => doc.get_dictionary(*id).ok().cloned(),
            Object::Dictionary(dict) => Some(dict.clone()),
            _ => None,
        }
    }

    /// The `/Info` dictionary's object id, creating one if the file has none.
    fn info_dictionary_id(&mut self) -> lopdf::ObjectId {
        if let Ok(Object::Reference(id)) = self.raw().trailer.get(b"Info") {
            return *id;
        }
        let doc = self.raw_mut();
        let id = doc.add_object(Dictionary::new());
        doc.trailer.set("Info", Object::Reference(id));
        id
    }
}

/// Read a `/Info` entry as text, decoding whichever encoding it used.
fn text_entry(dict: &Dictionary, key: &[u8]) -> Option<String> {
    let bytes = dict.get(key).ok()?.as_str().ok()?;
    let text = decode_text(bytes);
    (!text.trim().is_empty()).then_some(text)
}

/// Decode a PDF text string.
///
/// UTF-16BE when it starts with a byte order mark, otherwise PDFDocEncoding —
/// which agrees with Latin-1 across the range that matters here.
#[must_use]
pub fn decode_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    bytes.iter().map(|b| char::from(*b)).collect()
}

/// Encode a string as UTF-16BE with a byte order mark.
#[must_use]
pub fn encode_text(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    bytes
}

/// Normalize a PDF date string for display.
///
/// `D:20240305142201+08'00'` becomes `2024-03-05 14:22:01 +08:00`. Anything
/// that does not parse is passed through unchanged rather than dropped — a
/// weird date is still information.
#[must_use]
pub fn format_date(raw: &str) -> String {
    let text = raw.strip_prefix("D:").unwrap_or(raw);
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() < 8 {
        return raw.to_string();
    }

    let at = |start: usize, len: usize| digits.get(start..start + len).unwrap_or("");
    let mut out = format!("{}-{}-{}", at(0, 4), at(4, 2), at(6, 2));
    if digits.len() >= 14 {
        out.push_str(&format!(" {}:{}:{}", at(8, 2), at(10, 2), at(12, 2)));
    } else if digits.len() >= 12 {
        out.push_str(&format!(" {}:{}", at(8, 2), at(10, 2)));
    }

    // Offsets look like +08'00', -05'00', or Z.
    let rest = &text[digits.len()..];
    if let Some(sign) = rest.chars().next() {
        match sign {
            'Z' => out.push_str(" UTC"),
            '+' | '-' => {
                let offset: String = rest[1..].chars().filter(char::is_ascii_digit).collect();
                if offset.len() >= 4 {
                    out.push_str(&format!(" {sign}{}:{}", &offset[0..2], &offset[2..4]));
                } else if offset.len() >= 2 {
                    out.push_str(&format!(" {sign}{}:00", &offset[0..2]));
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_text_strings_decode() {
        let bytes = encode_text("Résumé — 履歴書");
        assert_eq!(decode_text(&bytes), "Résumé — 履歴書");
        assert!(
            bytes.starts_with(&[0xFE, 0xFF]),
            "must carry a byte order mark"
        );
    }

    #[test]
    fn latin1_text_strings_decode() {
        assert_eq!(decode_text(b"Plain ASCII"), "Plain ASCII");
        assert_eq!(decode_text(&[0x41, 0xE9]), "Aé");
    }

    #[test]
    fn dates_normalize_to_something_readable() {
        assert_eq!(
            format_date("D:20240305142201+08'00'"),
            "2024-03-05 14:22:01 +08:00"
        );
        assert_eq!(format_date("D:20240305142201Z"), "2024-03-05 14:22:01 UTC");
        assert_eq!(format_date("D:20240305142201"), "2024-03-05 14:22:01");
        assert_eq!(format_date("D:20240305"), "2024-03-05");
    }

    #[test]
    fn an_unparseable_date_survives_rather_than_vanishing() {
        assert_eq!(format_date("yesterday"), "yesterday");
        assert_eq!(format_date("D:2024"), "D:2024");
    }

    #[test]
    fn a_diff_lists_only_what_changed() {
        let before = Metadata {
            title: Some("Old".into()),
            author: Some("Ada".into()),
            ..Metadata::default()
        };
        let after = Metadata {
            title: Some("New".into()),
            author: Some("Ada".into()),
            ..before.clone()
        };

        let edit = MetadataEdit::diff(&before, &after);
        assert_eq!(edit.title.as_deref(), Some("New"));
        assert_eq!(
            edit.author, None,
            "an unchanged field must not be rewritten"
        );
        assert!(!edit.is_empty());
    }

    #[test]
    fn clearing_a_field_shows_up_as_an_empty_string() {
        let before = Metadata {
            title: Some("Old".into()),
            ..Metadata::default()
        };
        let after = Metadata {
            title: None,
            ..Metadata::default()
        };
        assert_eq!(
            MetadataEdit::diff(&before, &after).title.as_deref(),
            Some("")
        );
    }

    #[test]
    fn an_empty_edit_is_recognized() {
        assert!(MetadataEdit::default().is_empty());
    }
}
