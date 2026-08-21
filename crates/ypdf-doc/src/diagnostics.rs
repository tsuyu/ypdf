//! Structural diagnostics (spec §14).
//!
//! One pass over every object, counting what is there and noting what is
//! wrong. This is deliberately *structural*: it says a font is missing or a
//! reference dangles, and leaves "is this dangerous" to the security scanner.

use std::collections::BTreeSet;

use lopdf::{Document, Object, ObjectId};
use ypdf_core::Result;

use crate::pdf::Pdf;

/// How much a finding matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth knowing.
    Info,
    /// Something is unusual or wasteful.
    Warning,
    /// The file is damaged but still usable.
    High,
    /// The file is damaged in a way that loses content.
    Critical,
}

impl Severity {
    /// A short marker for text output (spec §26 uses these).
    #[must_use]
    pub const fn marker(self) -> &'static str {
        match self {
            Self::Info => "i",
            Self::Warning => "!",
            Self::High => "!!",
            Self::Critical => "!!!",
        }
    }
}

/// One structural problem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    /// How much it matters.
    pub severity: Severity,
    /// Stable code, for scripting against.
    pub code: &'static str,
    /// What is wrong, in a sentence.
    pub message: String,
}

/// The report from spec §14.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics {
    /// PDF version from the header.
    pub version: String,
    /// Page count.
    pub pages: u32,
    /// File size in bytes, when the document came from a file.
    pub file_size: Option<u64>,
    /// Distinct font objects.
    pub fonts: usize,
    /// Image XObjects.
    pub images: usize,
    /// Annotations across all pages.
    pub annotations: usize,
    /// AcroForm fields.
    pub form_fields: usize,
    /// Embedded file streams.
    pub embedded_files: usize,
    /// Whether the document is encrypted.
    pub encrypted: bool,
    /// Whether the document declares itself linearized.
    pub linearized: bool,
    /// The PDF/A part and conformance level claimed by the XMP, if any.
    ///
    /// A *claim*, not a validation — spec §15 is a later milestone, and saying
    /// "PDF/A-2b" here would be reporting the file's own word as fact.
    pub pdf_a_claim: Option<String>,
    /// Total indirect objects.
    pub objects: usize,
    /// Everything wrong with the file.
    pub issues: Vec<Issue>,
}

impl Diagnostics {
    /// The worst severity present, if anything is wrong.
    #[must_use]
    pub fn worst(&self) -> Option<Severity> {
        self.issues.iter().map(|i| i.severity).max()
    }

    /// Issues at or above a severity.
    #[must_use]
    pub fn at_least(&self, severity: Severity) -> Vec<&Issue> {
        self.issues
            .iter()
            .filter(|i| i.severity >= severity)
            .collect()
    }
}

/// Images larger than this are worth mentioning: they usually dominate a file
/// that someone is trying to shrink (spec §4).
const OVERSIZED_IMAGE_BYTES: usize = 4 * 1024 * 1024;

impl Pdf {
    /// Inspect the document's structure (spec §14).
    pub fn diagnostics(&self) -> Result<Diagnostics> {
        let doc = self.raw();
        let mut report = Diagnostics {
            version: doc.version.clone(),
            pages: self.page_count(),
            file_size: self
                .path()
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|m| m.len()),
            encrypted: doc.trailer.get(b"Encrypt").is_ok(),
            linearized: is_linearized(doc),
            objects: doc.objects.len(),
            ..Diagnostics::default()
        };

        let mut oversized = 0;
        for (id, object) in &doc.objects {
            match object {
                Object::Dictionary(dict) => match dict.get(b"Type").and_then(Object::as_name) {
                    Ok(b"Font") => report.fonts += 1,
                    Ok(b"Filespec") => report.embedded_files += 1,
                    _ => {}
                },
                Object::Stream(stream) => {
                    let dict = &stream.dict;
                    if dict
                        .get(b"Subtype")
                        .and_then(Object::as_name)
                        .is_ok_and(|s| s == b"Image")
                    {
                        report.images += 1;
                        if stream.content.len() > OVERSIZED_IMAGE_BYTES {
                            oversized += 1;
                        }
                    }
                    if dict
                        .get(b"Type")
                        .and_then(Object::as_name)
                        .is_ok_and(|t| t == b"EmbeddedFile")
                    {
                        report.embedded_files += 1;
                    }
                    // A stream whose declared filters cannot be applied has
                    // lost its content, whatever else is true of the file.
                    if stream.decompressed_content().is_err() && dict.get(b"Filter").is_ok() {
                        report.issues.push(Issue {
                            severity: Severity::High,
                            code: "D_STREAM_UNDECODABLE",
                            message: format!(
                                "Stream in object {} {} could not be decoded with its filters.",
                                id.0, id.1
                            ),
                        });
                    }
                }
                _ => {}
            }
        }

        report.annotations = self.count_annotations();
        report.form_fields = count_form_fields(doc);
        report.pdf_a_claim = self.pdf_a_claim();

        if oversized > 0 {
            report.issues.push(Issue {
                severity: Severity::Warning,
                code: "D_IMAGE_OVERSIZED",
                message: format!(
                    "{oversized} image{} over 4 MB; compression would help most here.",
                    if oversized == 1 { "" } else { "s" }
                ),
            });
        }

        let dangling = dangling_references(doc);
        if !dangling.is_empty() {
            report.issues.push(Issue {
                severity: Severity::High,
                code: "D_DANGLING_REFERENCE",
                message: format!(
                    "{} reference{} point at objects that do not exist (first: {} {}).",
                    dangling.len(),
                    if dangling.len() == 1 { "" } else { "s" },
                    dangling[0].0,
                    dangling[0].1
                ),
            });
        }

        if report.fonts == 0 && report.pages > 0 {
            report.issues.push(Issue {
                severity: Severity::Info,
                code: "D_NO_FONTS",
                message: "No fonts: the pages are images, and text will need OCR.".into(),
            });
        }

        if self.xmp_disagrees_with_info() {
            report.issues.push(Issue {
                severity: Severity::Warning,
                code: "D_XMP_STALE",
                message: "The XMP packet does not match the /Info dictionary.".into(),
            });
        }

        report.issues.sort_by(|a, b| b.severity.cmp(&a.severity));
        Ok(report)
    }

    /// Annotations on every page.
    fn count_annotations(&self) -> usize {
        let doc = self.raw();
        self.page_ids()
            .iter()
            .filter_map(|id| doc.get_dictionary(*id).ok())
            .filter_map(|page| page.get(b"Annots").ok())
            .map(|annots| match annots {
                Object::Array(items) => items.len(),
                Object::Reference(id) => doc
                    .get_object(*id)
                    .ok()
                    .and_then(|o| o.as_array().ok())
                    .map_or(0, Vec::len),
                _ => 0,
            })
            .sum()
    }

    /// The PDF/A part and level the XMP claims, if any.
    fn pdf_a_claim(&self) -> Option<String> {
        let xmp = self.xmp()?;
        let part = between(&xmp, "pdfaid:part>", "<").or_else(|| attribute(&xmp, "pdfaid:part"))?;
        let level = between(&xmp, "pdfaid:conformance>", "<")
            .or_else(|| attribute(&xmp, "pdfaid:conformance"))
            .unwrap_or_default();
        Some(format!(
            "PDF/A-{}{}",
            part.trim(),
            level.trim().to_lowercase()
        ))
    }
}

fn is_linearized(doc: &Document) -> bool {
    // A linearized file puts a /Linearized dictionary in its first object.
    doc.objects.values().take(4).any(|object| {
        object.as_dict().is_ok_and(|dict| dict.has(b"Linearized"))
            || object.as_stream().is_ok_and(|s| s.dict.has(b"Linearized"))
    })
}

fn count_form_fields(doc: &Document) -> usize {
    let Ok(catalog) = doc.catalog() else { return 0 };
    let Ok(acroform) = catalog.get(b"AcroForm") else {
        return 0;
    };
    let acroform = match acroform {
        Object::Reference(id) => doc.get_dictionary(*id).ok().cloned(),
        Object::Dictionary(dict) => Some(dict.clone()),
        _ => None,
    };
    acroform
        .and_then(|form| form.get(b"Fields").ok().cloned())
        .and_then(|fields| match fields {
            Object::Array(items) => Some(items.len()),
            Object::Reference(id) => doc
                .get_object(id)
                .ok()
                .and_then(|o| o.as_array().ok())
                .map(Vec::len),
            _ => None,
        })
        .unwrap_or(0)
}

/// References that point at objects the file does not contain.
fn dangling_references(doc: &Document) -> Vec<ObjectId> {
    let mut missing = BTreeSet::new();
    for object in doc.objects.values() {
        walk(object, &mut |id| {
            if !doc.objects.contains_key(&id) {
                missing.insert(id);
            }
        });
    }
    missing.into_iter().collect()
}

/// Visit every reference inside an object.
fn walk(object: &Object, found: &mut impl FnMut(ObjectId)) {
    match object {
        Object::Reference(id) => found(*id),
        Object::Array(items) => {
            for item in items {
                walk(item, found);
            }
        }
        Object::Dictionary(dict) => {
            for (_, value) in dict.iter() {
                walk(value, found);
            }
        }
        Object::Stream(stream) => {
            for (_, value) in stream.dict.iter() {
                walk(value, found);
            }
        }
        _ => {}
    }
}

fn between(haystack: &str, start: &str, end: &str) -> Option<String> {
    let from = haystack.find(start)? + start.len();
    let rest = &haystack[from..];
    let to = rest.find(end)?;
    Some(rest[..to].to_string())
}

/// Read `name="value"` from an XMP attribute, for packets that use the
/// attribute form rather than elements.
fn attribute(haystack: &str, name: &str) -> Option<String> {
    let pattern = format!("{name}=\"");
    between(haystack, &pattern, "\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_orders_from_info_to_critical() {
        assert!(Severity::Critical > Severity::High);
        assert!(Severity::High > Severity::Warning);
        assert!(Severity::Warning > Severity::Info);
    }

    #[test]
    fn the_worst_issue_is_reported() {
        let report = Diagnostics {
            issues: vec![
                Issue {
                    severity: Severity::Info,
                    code: "A",
                    message: String::new(),
                },
                Issue {
                    severity: Severity::High,
                    code: "B",
                    message: String::new(),
                },
                Issue {
                    severity: Severity::Warning,
                    code: "C",
                    message: String::new(),
                },
            ],
            ..Diagnostics::default()
        };
        assert_eq!(report.worst(), Some(Severity::High));
        assert_eq!(report.at_least(Severity::Warning).len(), 2);
    }

    #[test]
    fn a_clean_report_has_no_worst_severity() {
        assert_eq!(Diagnostics::default().worst(), None);
    }

    #[test]
    fn xmp_values_are_read_from_elements_or_attributes() {
        let element = "<pdfaid:part>2</pdfaid:part><pdfaid:conformance>B</pdfaid:conformance>";
        assert_eq!(between(element, "pdfaid:part>", "<").as_deref(), Some("2"));

        let attr = r#"<rdf:Description pdfaid:part="1" pdfaid:conformance="A"/>"#;
        assert_eq!(attribute(attr, "pdfaid:part").as_deref(), Some("1"));
    }
}
