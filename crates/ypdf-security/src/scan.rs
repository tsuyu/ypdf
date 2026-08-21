//! The scanner itself (spec §26).

use std::collections::{BTreeMap, BTreeSet};

use lopdf::{Dictionary, Document, Object};
use ypdf_doc::{Pdf, Severity};

use crate::urls::{UrlKind, classify_url};

/// File extensions that have no business being carried inside a PDF.
///
/// An embedded `.docx` is a document; an embedded `.exe` or `.lnk` is a
/// delivery mechanism, and the only reason to attach one is that the recipient
/// will double-click it.
const DANGEROUS_EXTENSIONS: [&str; 18] = [
    "exe", "dll", "scr", "com", "pif", "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse",
    "wsf", "hta", "jar", "lnk", "msi",
];

/// One thing the scanner found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// How much it matters.
    pub severity: Severity,
    /// Stable code, for scripting against — never renumbered.
    pub code: &'static str,
    /// One-line headline.
    pub title: String,
    /// What was actually found.
    pub detail: String,
    /// How many times, when that is meaningful.
    pub count: usize,
}

/// The result of a scan (spec §26).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// Everything found, worst first.
    pub findings: Vec<Finding>,
    /// Does the document contain JavaScript?
    pub javascript: bool,
    /// Number of embedded files.
    pub embedded_files: usize,
    /// Every external URL, in document order, de-duplicated.
    pub urls: Vec<String>,
    /// Number of launch actions.
    pub launch_actions: usize,
    /// Whether the document runs something on open.
    pub auto_actions: bool,
    /// Whether the document is encrypted.
    pub encrypted: bool,
    /// Whether the document carries a digital signature.
    pub signed: bool,
}

impl ScanReport {
    /// The worst severity found.
    #[must_use]
    pub fn worst(&self) -> Option<Severity> {
        self.findings.iter().map(|f| f.severity).max()
    }

    /// Is there anything a cautious person should look at before opening this
    /// file somewhere it matters?
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.worst().is_none_or(|s| s <= Severity::Info)
    }

    /// The report as text, in the shape of spec §26.
    #[must_use]
    pub fn to_human(&self) -> String {
        let mut out = String::from("PDF SECURITY SCAN\n\n");
        if self.findings.is_empty() {
            out.push_str("Nothing found.\n");
            return out;
        }
        for finding in &self.findings {
            out.push_str(&format!(
                "{:<3} {:<24} {}\n",
                finding.severity.marker(),
                finding.title,
                finding.detail
            ));
        }
        out
    }
}

/// Scan a document (spec §26).
///
/// Reads only. Nothing found here is executed, followed, or fetched.
#[must_use]
pub fn scan(pdf: &Pdf) -> ScanReport {
    let doc = pdf.raw();
    let mut report = ScanReport {
        encrypted: doc.trailer.get(b"Encrypt").is_ok(),
        ..ScanReport::default()
    };

    let mut javascript = 0;
    let mut launches = 0;
    let mut embedded: Vec<String> = Vec::new();
    let mut dangerous: Vec<String> = Vec::new();
    let mut urls: BTreeSet<String> = BTreeSet::new();
    let mut url_order: Vec<String> = Vec::new();
    let mut media = 0;
    let mut remote_go_to = 0;
    let mut additional_actions = 0;
    let mut xfa = false;

    // Every dictionary, not just the indirect ones: an annotation carries its
    // action inline, so a /Launch or a URI most often sits nested inside
    // another object rather than standing on its own.
    for dict in all_dictionaries(doc) {
        let dict = &dict;

        // JavaScript hides in several places: a /JS entry, a /JavaScript name
        // tree, and an action whose subtype says so.
        if dict.has(b"JS") || dict.has(b"JavaScript") {
            javascript += 1;
        }
        if action_type(dict) == Some(b"JavaScript".as_slice()) {
            javascript += 1;
        }

        match action_type(dict) {
            Some(b"Launch") => launches += 1,
            Some(b"GoToR" | b"GoToE") => remote_go_to += 1,
            Some(b"URI") => {
                if let Some(url) = uri_of(dict, doc)
                    && urls.insert(url.clone())
                {
                    url_order.push(url);
                }
            }
            _ => {}
        }

        if dict.has(b"AA") {
            additional_actions += 1;
        }
        if dict
            .get(b"Type")
            .and_then(Object::as_name)
            .is_ok_and(|t| t == b"Filespec")
        {
            let name = file_name_of(dict).unwrap_or_else(|| "(unnamed)".to_string());
            if is_dangerous(&name) {
                dangerous.push(name.clone());
            }
            embedded.push(name);
        }
        if dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .is_ok_and(|s| matches!(s, b"RichMedia" | b"Movie" | b"Screen" | b"Sound"))
        {
            media += 1;
        }
        if dict.has(b"XFA") {
            xfa = true;
        }
        if dict
            .get(b"FT")
            .and_then(Object::as_name)
            .is_ok_and(|t| t == b"Sig")
        {
            report.signed = true;
        }
    }

    // An /OpenAction runs without the reader doing anything at all, which is
    // what makes it worth separating from the same action behind a click.
    let open_action = doc
        .catalog()
        .ok()
        .and_then(|catalog| catalog.get(b"OpenAction").ok())
        .map(|action| describe_open_action(action, doc));

    report.javascript = javascript > 0;
    report.launch_actions = launches;
    report.embedded_files = embedded.len();
    report.urls = url_order;
    report.auto_actions = open_action.is_some() || additional_actions > 0;

    let mut findings = Vec::new();

    if launches > 0 {
        findings.push(Finding {
            severity: Severity::Critical,
            code: "S_LAUNCH_ACTION",
            title: "Launch action".into(),
            detail: format!(
                "{launches} action{} that asks the reader to run an external program. \
                 yPDF never follows these.",
                plural(launches)
            ),
            count: launches,
        });
    }

    if !dangerous.is_empty() {
        findings.push(Finding {
            severity: Severity::Critical,
            code: "S_EMBEDDED_EXECUTABLE",
            title: "Executable attachment".into(),
            detail: format!(
                "Attached file{}: {}",
                plural(dangerous.len()),
                dangerous.join(", ")
            ),
            count: dangerous.len(),
        });
    }

    if javascript > 0 {
        findings.push(Finding {
            severity: Severity::High,
            code: "S_JAVASCRIPT",
            title: "JavaScript".into(),
            detail: format!(
                "{javascript} object{} carrying script. The bundled PDFium build has no \
                 JavaScript engine, so it cannot run here.",
                plural(javascript)
            ),
            count: javascript,
        });
    }

    if let Some(detail) = open_action {
        findings.push(Finding {
            severity: if report.javascript {
                Severity::High
            } else {
                Severity::Warning
            },
            code: "S_OPEN_ACTION",
            title: "Runs on open".into(),
            detail,
            count: 1,
        });
    }

    if additional_actions > 0 {
        findings.push(Finding {
            severity: Severity::Warning,
            code: "S_ADDITIONAL_ACTIONS",
            title: "Triggered actions".into(),
            detail: format!(
                "{additional_actions} object{} with actions bound to events such as page \
                 open, focus, or printing.",
                plural(additional_actions)
            ),
            count: additional_actions,
        });
    }

    let attachments = embedded.len() - dangerous.len();
    if attachments > 0 {
        findings.push(Finding {
            severity: Severity::Warning,
            code: "S_EMBEDDED_FILE",
            title: "Embedded files".into(),
            detail: format!("{attachments} attached file{}.", plural(attachments)),
            count: attachments,
        });
    }

    findings.extend(url_findings(&report.urls));

    if remote_go_to > 0 {
        findings.push(Finding {
            severity: Severity::Warning,
            code: "S_REMOTE_DESTINATION",
            title: "Remote destination".into(),
            detail: format!(
                "{remote_go_to} link{} into another document, which the reader would have \
                 to fetch.",
                plural(remote_go_to)
            ),
            count: remote_go_to,
        });
    }

    if media > 0 {
        findings.push(Finding {
            severity: Severity::Warning,
            code: "S_RICH_MEDIA",
            title: "Rich media".into(),
            detail: format!("{media} embedded media annotation{}.", plural(media)),
            count: media,
        });
    }

    if xfa {
        findings.push(Finding {
            severity: Severity::Warning,
            code: "S_XFA_FORM",
            title: "XFA form".into(),
            detail: "An XFA form: a second document format inside this one.".into(),
            count: 1,
        });
    }

    if report.encrypted {
        findings.push(Finding {
            severity: Severity::Info,
            code: "S_ENCRYPTED",
            title: "Encrypted".into(),
            detail: encryption_detail(doc),
            count: 1,
        });
    }

    if report.signed {
        findings.push(Finding {
            severity: Severity::Info,
            code: "S_SIGNED",
            title: "Digital signature".into(),
            detail: "The document carries a signature field. Validation is not implemented \
                     yet, so this says nothing about whether it is valid."
                .into(),
            count: 1,
        });
    }

    findings.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.code.cmp(b.code)));
    report.findings = findings;
    report
}

fn url_findings(urls: &[String]) -> Vec<Finding> {
    let mut by_kind: BTreeMap<&'static str, (UrlKind, Vec<&String>)> = BTreeMap::new();
    for url in urls {
        let kind = classify_url(url);
        by_kind
            .entry(kind.describe())
            .or_insert((kind, Vec::new()))
            .1
            .push(url);
    }

    by_kind
        .into_values()
        .map(|(kind, urls)| {
            let sample = urls
                .iter()
                .take(3)
                .map(|u| truncate(u, 70))
                .collect::<Vec<_>>()
                .join(", ");
            let more = urls.len().saturating_sub(3);
            Finding {
                severity: kind.severity(),
                code: url_code(kind),
                title: "External link".into(),
                detail: format!(
                    "{} × {}: {sample}{}",
                    urls.len(),
                    kind.describe(),
                    if more > 0 {
                        format!(", and {more} more")
                    } else {
                        String::new()
                    }
                ),
                count: urls.len(),
            }
        })
        .collect()
}

const fn url_code(kind: UrlKind) -> &'static str {
    match kind {
        UrlKind::Script => "S_URL_SCRIPT",
        UrlKind::LocalFile => "S_URL_LOCAL_FILE",
        UrlKind::RawAddress => "S_URL_IP_ADDRESS",
        UrlKind::Punycode => "S_URL_PUNYCODE",
        UrlKind::Insecure => "S_URL_INSECURE",
        UrlKind::Mail => "S_URL_MAIL",
        UrlKind::Secure | UrlKind::Other => "S_URL",
    }
}

/// Collect every dictionary in the document, including ones nested inside
/// other objects.
///
/// References are not followed: everything they point at is already an indirect
/// object and will be visited on its own. That also makes a reference cycle
/// harmless here.
fn all_dictionaries(doc: &Document) -> Vec<Dictionary> {
    /// Deep enough for real documents, shallow enough that a malicious nesting
    /// bomb cannot exhaust the stack.
    const MAX_DEPTH: usize = 32;

    fn visit(object: &Object, depth: usize, out: &mut Vec<Dictionary>) {
        if depth > MAX_DEPTH {
            return;
        }
        match object {
            Object::Dictionary(dict) => {
                out.push(dict.clone());
                for (_, value) in dict.iter() {
                    visit(value, depth + 1, out);
                }
            }
            Object::Stream(stream) => {
                out.push(stream.dict.clone());
                for (_, value) in stream.dict.iter() {
                    visit(value, depth + 1, out);
                }
            }
            Object::Array(items) => {
                for item in items {
                    visit(item, depth + 1, out);
                }
            }
            _ => {}
        }
    }

    let mut out = Vec::new();
    for object in doc.objects.values() {
        visit(object, 0, &mut out);
    }
    out
}

fn dictionary_of(object: &Object) -> Option<&Dictionary> {
    match object {
        Object::Dictionary(dict) => Some(dict),
        Object::Stream(stream) => Some(&stream.dict),
        _ => None,
    }
}

/// The `/S` entry of an action dictionary.
fn action_type(dict: &Dictionary) -> Option<&[u8]> {
    dict.get(b"S").ok()?.as_name().ok()
}

fn uri_of(dict: &Dictionary, doc: &Document) -> Option<String> {
    let object = dict.get(b"URI").ok()?;
    let bytes = match object {
        Object::Reference(id) => doc.get_object(*id).ok()?.as_str().ok()?.to_vec(),
        other => other.as_str().ok()?.to_vec(),
    };
    let url = ypdf_doc::decode_text(&bytes);
    (!url.trim().is_empty()).then_some(url)
}

fn file_name_of(dict: &Dictionary) -> Option<String> {
    for key in [b"UF".as_slice(), b"F".as_slice(), b"Desc".as_slice()] {
        if let Ok(value) = dict.get(key)
            && let Ok(bytes) = value.as_str()
        {
            let name = ypdf_doc::decode_text(bytes);
            if !name.trim().is_empty() {
                return Some(name);
            }
        }
    }
    None
}

fn is_dangerous(name: &str) -> bool {
    // Compare on the last extension: `invoice.pdf.exe` is an executable.
    name.rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| DANGEROUS_EXTENSIONS.contains(&ext.as_str()))
}

fn describe_open_action(action: &Object, doc: &Document) -> String {
    let resolved = match action {
        Object::Reference(id) => doc.get_object(*id).ok().cloned(),
        other => Some(other.clone()),
    };

    match resolved.as_ref().and_then(dictionary_of) {
        Some(dict) => match action_type(dict) {
            Some(b"JavaScript") => "Runs JavaScript when the document opens.".to_string(),
            Some(b"Launch") => {
                "Tries to run an external program when the document opens.".to_string()
            }
            Some(b"URI") => match uri_of(dict, doc) {
                Some(url) => format!(
                    "Opens a URL when the document opens: {}",
                    truncate(&url, 70)
                ),
                None => "Opens a URL when the document opens.".to_string(),
            },
            Some(other) => {
                format!(
                    "Runs a {} action when the document opens.",
                    String::from_utf8_lossy(other)
                )
            }
            None => "Has an action bound to opening the document.".to_string(),
        },
        // A destination array is just "start on page N", which is ordinary.
        None => "Jumps to a destination when opened.".to_string(),
    }
}

fn encryption_detail(doc: &Document) -> String {
    let dict = doc
        .trailer
        .get(b"Encrypt")
        .ok()
        .and_then(|object| match object {
            Object::Reference(id) => doc.get_dictionary(*id).ok().cloned(),
            Object::Dictionary(dict) => Some(dict.clone()),
            _ => None,
        });

    let Some(dict) = dict else {
        return "The document is encrypted.".to_string();
    };
    let v = dict.get(b"V").and_then(Object::as_i64).unwrap_or(0);
    let r = dict.get(b"R").and_then(Object::as_i64).unwrap_or(0);
    let length = dict.get(b"Length").and_then(Object::as_i64).unwrap_or(40);

    let algorithm = match (v, r) {
        (5, 6) => "AES-256".to_string(),
        (5, _) => "AES-256 (revision 5)".to_string(),
        (4, _) => "AES-128 or RC4-128".to_string(),
        (2 | 3, _) => format!("RC4-{length}"),
        _ => format!("version {v}, revision {r}"),
    };
    format!("Encryption: {algorithm}.")
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_extensions_are_recognized_including_double_ones() {
        assert!(is_dangerous("payload.exe"));
        assert!(
            is_dangerous("Invoice.PDF.EXE"),
            "the last extension is the real one"
        );
        assert!(is_dangerous("script.js"));
        assert!(!is_dangerous("attachment.pdf"));
        assert!(!is_dangerous("notes.txt"));
        assert!(!is_dangerous("no-extension"));
    }

    #[test]
    fn long_urls_are_shortened_for_display() {
        let long = "https://example.org/".to_string() + &"a".repeat(200);
        let short = truncate(&long, 70);
        assert_eq!(short.chars().count(), 71, "70 characters plus the ellipsis");
        assert!(short.ends_with('…'));
        assert_eq!(truncate("short", 70), "short");
    }

    #[test]
    fn an_empty_report_is_clean() {
        let report = ScanReport::default();
        assert!(report.is_clean());
        assert_eq!(report.worst(), None);
        assert!(report.to_human().contains("Nothing found"));
    }

    #[test]
    fn informational_findings_still_count_as_clean() {
        let report = ScanReport {
            findings: vec![Finding {
                severity: Severity::Info,
                code: "S_ENCRYPTED",
                title: "Encrypted".into(),
                detail: String::new(),
                count: 1,
            }],
            ..ScanReport::default()
        };
        assert!(
            report.is_clean(),
            "an encrypted file is not a suspicious one"
        );
    }

    #[test]
    fn a_warning_is_not_clean() {
        let report = ScanReport {
            findings: vec![Finding {
                severity: Severity::Warning,
                code: "S_EMBEDDED_FILE",
                title: "Embedded files".into(),
                detail: String::new(),
                count: 1,
            }],
            ..ScanReport::default()
        };
        assert!(!report.is_clean());
    }

    #[test]
    fn url_findings_group_by_kind_and_count() {
        let urls = vec![
            "https://a.example".to_string(),
            "https://b.example".to_string(),
            "javascript:alert(1)".to_string(),
        ];
        let findings = url_findings(&urls);

        let script = findings
            .iter()
            .find(|f| f.code == "S_URL_SCRIPT")
            .expect("script link");
        assert_eq!(script.severity, Severity::Critical);
        assert_eq!(script.count, 1);

        let ordinary = findings
            .iter()
            .find(|f| f.code == "S_URL")
            .expect("ordinary links");
        assert_eq!(ordinary.count, 2);
    }
}
