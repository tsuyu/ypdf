//! Getting form data in and out: JSON, FDF, and XFDF (spec §16).
//!
//! All three carry the same thing — a list of name/value pairs — so all three
//! are read into and written from one `BTreeMap`. Sorted, so exporting the same
//! form twice produces the same bytes and a diff between two filled copies
//! shows what actually differs.
//!
//! FDF and XFDF are written by hand rather than through a library. They are
//! small formats, and the alternative is a dependency that would parse far more
//! of them than this needs.

use std::collections::BTreeMap;

use ypdf_core::{Error, Result};

use crate::model::Field;

/// Which interchange format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    /// JSON: an object of name to value.
    #[default]
    Json,
    /// FDF, the PDF-shaped one.
    Fdf,
    /// XFDF, the XML one.
    Xfdf,
}

impl Format {
    /// Guess from a file name.
    #[must_use]
    pub fn from_path(path: &std::path::Path) -> Self {
        match path
            .extension()
            .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
            .as_deref()
        {
            Some("fdf") => Self::Fdf,
            Some("xfdf") => Self::Xfdf,
            _ => Self::Json,
        }
    }

    /// The usual extension.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Fdf => "fdf",
            Self::Xfdf => "xfdf",
        }
    }
}

/// Turn fields into name/value pairs.
#[must_use]
pub fn values_of(fields: &[Field]) -> BTreeMap<String, String> {
    fields
        .iter()
        .filter(|field| field.kind.is_fillable())
        .map(|field| (field.name.clone(), field.value.clone()))
        .collect()
}

/// Write the data out.
pub fn export(values: &BTreeMap<String, String>, format: Format) -> Result<String> {
    Ok(match format {
        Format::Json => serde_json::to_string_pretty(values).map_err(|e| Error::Config {
            detail: format!("the form data could not be written as JSON: {e}"),
            source_path: None,
        })?,
        Format::Fdf => fdf(values),
        Format::Xfdf => xfdf(values),
    })
}

/// Read data in.
pub fn import(text: &str, format: Format) -> Result<BTreeMap<String, String>> {
    match format {
        Format::Json => serde_json::from_str(text).map_err(|e| Error::Config {
            detail: format!("this is not form data: {e}"),
            source_path: None,
        }),
        Format::Fdf => Ok(parse_fdf(text)),
        Format::Xfdf => Ok(parse_xfdf(text)),
    }
}

/// An FDF file, which is a small PDF whose one object is the field list.
fn fdf(values: &BTreeMap<String, String>) -> String {
    let mut fields = String::new();
    for (name, value) in values {
        fields.push_str(&format!(
            "<< /T ({}) /V ({}) >>\n",
            escape_pdf(name),
            escape_pdf(value)
        ));
    }

    format!(
        "%FDF-1.2\n\
         1 0 obj\n\
         << /FDF << /Fields [\n{fields}] >> >>\n\
         endobj\n\
         trailer\n\
         << /Root 1 0 R >>\n\
         %%EOF\n"
    )
}

fn xfdf(values: &BTreeMap<String, String>) -> String {
    let mut fields = String::new();
    for (name, value) in values {
        fields.push_str(&format!(
            "    <field name=\"{}\">\n      <value>{}</value>\n    </field>\n",
            escape_xml(name),
            escape_xml(value)
        ));
    }

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <xfdf xmlns=\"http://ns.adobe.com/xfdf/\" xml:space=\"preserve\">\n\
         \x20 <fields>\n{fields}  </fields>\n\
         </xfdf>\n"
    )
}

/// Pull `/T (name) /V (value)` pairs out of an FDF file.
///
/// A deliberately small reader: it looks for the two keys and ignores
/// everything else, which is all this needs and cannot be surprised by the
/// parts of FDF nobody uses.
fn parse_fdf(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut pending_name: Option<String> = None;

    while index < bytes.len() {
        if bytes[index..].starts_with(b"/T") {
            index += 2;
            if let Some((value, next)) = read_pdf_string(bytes, index) {
                pending_name = Some(value);
                index = next;
                continue;
            }
        } else if bytes[index..].starts_with(b"/V") {
            index += 2;
            if let Some((value, next)) = read_pdf_string(bytes, index) {
                if let Some(name) = pending_name.take() {
                    out.insert(name, value);
                }
                index = next;
                continue;
            }
        }
        index += 1;
    }

    out
}

/// Read a `(...)` string starting at or after `from`, returning it and the
/// position after it.
fn read_pdf_string(bytes: &[u8], from: usize) -> Option<(String, usize)> {
    let start = bytes[from..].iter().position(|byte| *byte == b'(')? + from + 1;

    let mut out = Vec::new();
    let mut depth = 1;
    let mut index = start;

    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                // An escaped character is taken as itself, which covers the
                // parentheses and backslashes that would otherwise confuse the
                // nesting count.
                if let Some(next) = bytes.get(index + 1) {
                    out.push(*next);
                }
                index += 2;
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((ypdf_doc::decode_text(&out), index + 1));
                }
            }
            _ => {}
        }
        out.push(bytes[index]);
        index += 1;
    }

    None
}

/// Pull `<field name="x"><value>y</value></field>` out of XFDF.
fn parse_xfdf(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut rest = text;

    while let Some(start) = rest.find("<field ") {
        rest = &rest[start..];
        let Some(name) = between(rest, "name=\"", "\"") else {
            break;
        };
        let Some(end) = rest.find("</field>") else {
            break;
        };
        let body = &rest[..end];

        let value = between(body, "<value>", "</value>").unwrap_or_default();
        out.insert(unescape_xml(&name), unescape_xml(&value));
        rest = &rest[end + "</field>".len()..];
    }

    out
}

fn between(haystack: &str, start: &str, end: &str) -> Option<String> {
    let from = haystack.find(start)? + start.len();
    let to = haystack[from..].find(end)? + from;
    Some(haystack[from..to].to_string())
}

fn escape_pdf(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn unescape_xml(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        // Last, or an escaped `&amp;lt;` would come back as `<`.
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("name".to_string(), "Ada Lovelace".to_string()),
            (
                "notes".to_string(),
                "a (parenthesis) & an <angle>".to_string(),
            ),
        ])
    }

    #[test]
    fn json_round_trips() {
        let text = export(&sample(), Format::Json).expect("writes");
        assert_eq!(import(&text, Format::Json).expect("reads"), sample());
    }

    #[test]
    fn fdf_round_trips_including_the_characters_that_break_it() {
        // Unescaped parentheses end the string early, and everything after them
        // is read as structure.
        let text = export(&sample(), Format::Fdf).expect("writes");
        assert!(text.starts_with("%FDF-1.2"));
        assert_eq!(import(&text, Format::Fdf).expect("reads"), sample());
    }

    #[test]
    fn xfdf_round_trips_including_the_characters_that_break_it() {
        let text = export(&sample(), Format::Xfdf).expect("writes");
        assert!(text.contains("<xfdf"));
        assert!(
            text.contains("&amp;") && text.contains("&lt;angle&gt;"),
            "{text}"
        );
        assert_eq!(import(&text, Format::Xfdf).expect("reads"), sample());
    }

    #[test]
    fn export_is_ordered_so_two_runs_produce_the_same_bytes() {
        let once = export(&sample(), Format::Json).expect("writes");
        let twice = export(&sample(), Format::Json).expect("writes");
        assert_eq!(once, twice);
    }

    #[test]
    fn a_format_is_guessed_from_the_file_name() {
        assert_eq!(
            Format::from_path(std::path::Path::new("data.fdf")),
            Format::Fdf
        );
        assert_eq!(
            Format::from_path(std::path::Path::new("data.XFDF")),
            Format::Xfdf
        );
        assert_eq!(
            Format::from_path(std::path::Path::new("data.json")),
            Format::Json
        );
        assert_eq!(
            Format::from_path(std::path::Path::new("data")),
            Format::Json,
            "the default is the one people can read"
        );
    }

    #[test]
    fn nonsense_json_is_refused_with_a_readable_reason() {
        let error = import("this is not json", Format::Json).expect_err("refused");
        assert!(error.report().to_human().contains("not form data"));
    }

    #[test]
    fn an_empty_fdf_reads_as_no_values_rather_than_failing() {
        assert!(
            import("%FDF-1.2\ntrailer\n<< >>\n%%EOF", Format::Fdf)
                .expect("reads")
                .is_empty()
        );
    }

    #[test]
    fn only_fillable_fields_are_exported() {
        // A signature field has nothing to export, and a push button never had
        // a value to begin with.
        let fields = vec![
            Field {
                name: "name".into(),
                kind: crate::model::Kind::Text,
                value: "Ada".into(),
                ..Field::default()
            },
            Field {
                name: "sign here".into(),
                kind: crate::model::Kind::Signature,
                ..Field::default()
            },
        ];
        let values = values_of(&fields);
        assert_eq!(values.len(), 1);
        assert!(values.contains_key("name"));
    }
}
