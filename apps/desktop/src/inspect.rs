//! The inspect panel: metadata, diagnostics, and the security scan.
//!
//! All three read the document through `ypdf-doc`, which never loads PDFium.
//! Inspection is therefore independent of what the viewer is doing, and a file
//! too broken to render can still be inspected — which is exactly when someone
//! wants to.

use egui::{Color32, RichText, Ui};
use ypdf_core::Result;
use ypdf_doc::{Diagnostics, Metadata, MetadataEdit, Pdf, Severity};
use ypdf_security::ScanReport;

/// Which report the panel is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    /// Editable document information.
    #[default]
    Metadata,
    /// Structural report (spec §14).
    Diagnostics,
    /// Security scan (spec §26).
    Security,
}

/// Everything the panel knows, computed once per document and after each edit.
#[derive(Debug, Default)]
pub struct Inspection {
    /// Is the panel open?
    pub open: bool,
    /// Which tab is showing.
    pub tab: Tab,
    /// Metadata as it is on disk.
    pub metadata: Metadata,
    /// Metadata as edited in the panel.
    pub draft: Metadata,
    /// The structural report.
    pub diagnostics: Diagnostics,
    /// The security scan.
    pub security: ScanReport,
    /// The protection the file carried when it was opened.
    pub protection: ypdf_crypt::SecurityInfo,
    /// The XMP packet, if there is one.
    pub xmp: Option<String>,
    /// Why the last inspection failed, if it did.
    pub error: Option<String>,
    /// True once the reports match the document as it currently stands.
    pub loaded: bool,
}

impl Inspection {
    /// Read everything from the document.
    ///
    /// Cheap enough to run whenever the document changes: one parse and one
    /// pass over the objects, with no rendering involved.
    pub fn refresh(&mut self, pdf: &Pdf) -> Result<()> {
        self.metadata = pdf.metadata();
        self.draft = self.metadata.clone();
        self.diagnostics = pdf.diagnostics()?;
        self.security = ypdf_security::scan(pdf);
        self.protection = ypdf_crypt::security_info(pdf);
        self.xmp = pdf.xmp();
        self.error = None;
        self.loaded = true;
        Ok(())
    }

    /// Mark the reports as out of date after an edit.
    pub fn invalidate(&mut self) {
        self.loaded = false;
    }

    /// The metadata edit the user has drafted, if any.
    #[must_use]
    pub fn pending_edit(&self) -> MetadataEdit {
        MetadataEdit::diff(&self.metadata, &self.draft)
    }

    /// A one-word summary of the security scan, for the toolbar.
    #[must_use]
    pub fn security_badge(&self) -> (&'static str, Severity) {
        match self.security.worst() {
            Some(Severity::Critical) => ("Critical", Severity::Critical),
            Some(Severity::High) => ("High risk", Severity::High),
            Some(Severity::Warning) => ("Check", Severity::Warning),
            _ => ("Clean", Severity::Info),
        }
    }
}

/// Colour for a severity, readable in both themes.
#[must_use]
pub fn severity_colour(ui: &Ui, severity: Severity) -> Color32 {
    match severity {
        Severity::Critical => Color32::from_rgb(255, 90, 90),
        Severity::High => Color32::from_rgb(255, 145, 60),
        Severity::Warning => ui.visuals().warn_fg_color,
        Severity::Info => ui.visuals().weak_text_color(),
    }
}

/// Draw the panel. Returns true when the user asked to apply metadata edits.
pub fn show(ui: &mut Ui, inspection: &mut Inspection) -> bool {
    let mut apply = false;

    ui.horizontal(|ui| {
        ui.selectable_value(&mut inspection.tab, Tab::Metadata, "Metadata");
        ui.selectable_value(&mut inspection.tab, Tab::Diagnostics, "Diagnostics");

        let (label, severity) = inspection.security_badge();
        let text =
            RichText::new(format!("Security · {label}")).color(severity_colour(ui, severity));
        ui.selectable_value(&mut inspection.tab, Tab::Security, text);
    });
    ui.separator();

    if let Some(error) = &inspection.error {
        ui.colored_label(ui.visuals().error_fg_color, error);
        return false;
    }
    if !inspection.loaded {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Inspecting…");
        });
        return false;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| match inspection.tab {
            Tab::Metadata => apply = metadata_tab(ui, inspection),
            Tab::Diagnostics => diagnostics_tab(ui, &inspection.diagnostics),
            Tab::Security => {
                security_tab(ui, &inspection.security, &inspection.protection);
            }
        });

    apply
}

/// What protects the file, above the scan findings (spec §8).
fn protection_block(ui: &mut Ui, protection: &ypdf_crypt::SecurityInfo) {
    if !protection.encrypted {
        return;
    }

    ui.label(
        RichText::new(format!(
            "Encrypted with {}",
            protection.algorithm.as_deref().unwrap_or("unknown")
        ))
        .strong(),
    );
    ui.label(if protection.user_password_required {
        "A password is needed to open it."
    } else {
        "No password is needed to open it."
    });

    let restrictions = protection.permissions.restrictions();
    if restrictions.is_empty() {
        ui.weak("Nothing restricted.");
    } else {
        ui.label(format!("Restricted: {}", restrictions.join(", ")));
        // Someone reading this list is about to decide how far to trust it.
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "Permission flags are advisory. Conforming readers honour them; nothing enforces them.",
        );
    }

    ui.add_space(4.0);
    ui.colored_label(
        ui.visuals().warn_fg_color,
        "Saving from here writes an unprotected copy.",
    );
    ui.separator();
}

fn metadata_tab(ui: &mut Ui, inspection: &mut Inspection) -> bool {
    let mut apply = false;

    egui::Grid::new("metadata")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            field(ui, "Title", &mut inspection.draft.title);
            field(ui, "Author", &mut inspection.draft.author);
            field(ui, "Subject", &mut inspection.draft.subject);
            field(ui, "Keywords", &mut inspection.draft.keywords);
        });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let pending = !inspection.pending_edit().is_empty();
        if ui
            .add_enabled(pending, egui::Button::new("Apply"))
            .clicked()
        {
            apply = true;
        }
        if ui
            .add_enabled(pending, egui::Button::new("Revert"))
            .clicked()
        {
            inspection.draft = inspection.metadata.clone();
        }
    });

    ui.add_space(12.0);
    ui.label(RichText::new("Read-only").strong());
    egui::Grid::new("metadata-readonly")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            // These belong to whatever wrote the file; presenting them as editable
            // would invite people to lie about provenance.
            read_only(ui, "Creator", inspection.metadata.creator.as_deref());
            read_only(ui, "Producer", inspection.metadata.producer.as_deref());
            read_only(ui, "Created", inspection.metadata.created.as_deref());
            read_only(ui, "Modified", inspection.metadata.modified.as_deref());
            read_only(ui, "PDF version", Some(&inspection.metadata.version));
            read_only(
                ui,
                "Pages",
                Some(&inspection.metadata.page_count.to_string()),
            );
        });

    if let Some(xmp) = &inspection.xmp {
        ui.add_space(12.0);
        egui::CollapsingHeader::new("XMP packet").show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut xmp.as_str())
                    .desired_width(f32::INFINITY)
                    .code_editor(),
            );
        });
    }

    apply
}

fn field(ui: &mut Ui, label: &str, value: &mut Option<String>) {
    ui.label(label);
    let mut text = value.clone().unwrap_or_default();
    if ui
        .add(egui::TextEdit::singleline(&mut text).desired_width(260.0))
        .changed()
    {
        *value = if text.is_empty() { None } else { Some(text) };
    }
    ui.end_row();
}

fn read_only(ui: &mut Ui, label: &str, value: Option<&str>) {
    ui.label(label);
    match value {
        Some(text) => {
            ui.label(text);
        }
        None => {
            ui.weak("—");
        }
    }
    ui.end_row();
}

fn diagnostics_tab(ui: &mut Ui, report: &Diagnostics) {
    egui::Grid::new("diagnostics")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            row(ui, "PDF version", &report.version);
            row(ui, "Pages", &report.pages.to_string());
            row(
                ui,
                "File size",
                &report.file_size.map_or("—".into(), format_size),
            );
            row(ui, "Objects", &report.objects.to_string());
            row(ui, "Fonts", &report.fonts.to_string());
            row(ui, "Images", &report.images.to_string());
            row(ui, "Annotations", &report.annotations.to_string());
            row(ui, "Form fields", &report.form_fields.to_string());
            row(ui, "Embedded files", &report.embedded_files.to_string());
            row(
                ui,
                "Encryption",
                if report.encrypted { "Yes" } else { "None" },
            );
            row(
                ui,
                "Linearized",
                if report.linearized { "Yes" } else { "No" },
            );
            row(
                ui,
                "PDF/A",
                &report.pdf_a_claim.as_ref().map_or_else(
                    || "Not claimed".to_string(),
                    |c| format!("{c} (claimed, unverified)"),
                ),
            );
        });

    ui.add_space(12.0);
    if report.issues.is_empty() {
        ui.label("No structural problems found.");
        return;
    }

    ui.label(RichText::new(format!("{} issue(s)", report.issues.len())).strong());
    ui.add_space(4.0);
    for issue in &report.issues {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(severity_colour(ui, issue.severity), issue.severity.marker());
            ui.label(&issue.message);
        });
        ui.weak(RichText::new(issue.code).small());
        ui.add_space(6.0);
    }
}

fn security_tab(ui: &mut Ui, report: &ScanReport, protection: &ypdf_crypt::SecurityInfo) {
    protection_block(ui, protection);

    if report.findings.is_empty() {
        ui.label("Nothing found.");
        ui.add_space(8.0);
        ui.weak("No scripts, attachments, automatic actions, or external links.");
        return;
    }

    for finding in &report.findings {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(
                severity_colour(ui, finding.severity),
                RichText::new(&finding.title).strong(),
            );
        });
        ui.label(&finding.detail);
        ui.weak(RichText::new(finding.code).small());
        ui.add_space(8.0);
    }

    if !report.urls.is_empty() {
        egui::CollapsingHeader::new(format!("{} external link(s)", report.urls.len())).show(
            ui,
            |ui| {
                // Shown as text, never as clickable links: this list exists so
                // someone can read where a file wants to send them.
                for url in &report.urls {
                    ui.weak(url);
                }
            },
        );
    }
}

fn row(ui: &mut Ui, label: &str, value: &str) {
    ui.label(label);
    ui.label(value);
    ui.end_row();
}

/// Human-readable byte count, as the statistics panels report it (spec §4).
fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    #[expect(clippy::cast_precision_loss, reason = "display only")]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Width the panel wants.
#[must_use]
pub const fn preferred_width() -> f32 {
    360.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_render_like_the_spec() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(48_200_000), "46.0 MB");
    }

    #[test]
    fn the_badge_follows_the_worst_finding() {
        let mut inspection = Inspection::default();
        assert_eq!(inspection.security_badge().0, "Clean");

        inspection.security.findings.push(ypdf_security::Finding {
            severity: Severity::Warning,
            code: "S_EMBEDDED_FILE",
            title: "Embedded files".into(),
            detail: String::new(),
            count: 1,
        });
        assert_eq!(inspection.security_badge().0, "Check");

        inspection.security.findings.push(ypdf_security::Finding {
            severity: Severity::Critical,
            code: "S_LAUNCH_ACTION",
            title: "Launch action".into(),
            detail: String::new(),
            count: 1,
        });
        assert_eq!(inspection.security_badge().0, "Critical");
    }

    #[test]
    fn an_untouched_draft_produces_no_edit() {
        let inspection = Inspection {
            metadata: Metadata {
                title: Some("A".into()),
                ..Metadata::default()
            },
            draft: Metadata {
                title: Some("A".into()),
                ..Metadata::default()
            },
            ..Inspection::default()
        };
        assert!(inspection.pending_edit().is_empty());
    }

    #[test]
    fn an_edited_draft_produces_exactly_that_change() {
        let inspection = Inspection {
            metadata: Metadata {
                title: Some("A".into()),
                author: Some("Ada".into()),
                ..Metadata::default()
            },
            draft: Metadata {
                title: Some("B".into()),
                author: Some("Ada".into()),
                ..Metadata::default()
            },
            ..Inspection::default()
        };
        let edit = inspection.pending_edit();
        assert_eq!(edit.title.as_deref(), Some("B"));
        assert!(edit.author.is_none());
    }
}
