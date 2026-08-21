//! The forms panel (spec §16).
//!
//! Fields are edited in a list rather than on the page. Editing in place looks
//! better in a screenshot and is worse to use: the boxes are small, several are
//! usually off screen, and a form with thirty fields becomes a hunt. A list can
//! be read top to bottom and says which fields are required.
//!
//! Nothing is written until **Apply**, and flattening — which cannot be undone —
//! is a separate button that says so.

use std::collections::BTreeMap;

use egui::{Context, Ui};
use ypdf_forms::{Field, Kind};

/// State of the panel.
#[derive(Debug, Default)]
pub struct FormsPanel {
    /// Is it showing?
    pub open: bool,
    /// The fields as the document has them.
    pub fields: Vec<Field>,
    /// The values being edited.
    pub draft: BTreeMap<String, String>,
    /// True once the fields have been read.
    pub loaded: bool,
    /// Does the document have a form at all?
    pub has_form: bool,
    /// What the last write did.
    pub report: Option<ypdf_forms::Report>,
    /// Why the last attempt failed.
    pub error: Option<String>,
}

/// What the panel is asking for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Write the edited values into the document.
    Apply,
    /// Empty every field.
    Clear,
    /// Draw the fields into the page and remove the form.
    Flatten,
    /// Write the values to a file.
    Export,
    /// Read values from a file.
    Import,
}

impl FormsPanel {
    /// Take the fields read from the document.
    pub fn load(&mut self, fields: Vec<Field>, has_form: bool) {
        self.draft = fields
            .iter()
            .map(|field| (field.name.clone(), field.value.clone()))
            .collect();
        self.fields = fields;
        self.has_form = has_form;
        self.loaded = true;
    }

    /// Forget them, so they are read again.
    pub fn invalidate(&mut self) {
        self.loaded = false;
        self.report = None;
    }

    /// The values that differ from what the document holds.
    ///
    /// Only these are sent: writing every field would rebuild appearances for
    /// fields nobody touched, and a diff of the two files would show changes
    /// that did not happen.
    #[must_use]
    pub fn changes(&self) -> BTreeMap<String, String> {
        self.fields
            .iter()
            .filter(|field| field.is_writable())
            .filter_map(|field| {
                let drafted = self.draft.get(&field.name)?;
                (drafted != &field.value).then(|| (field.name.clone(), drafted.clone()))
            })
            .collect()
    }

    /// Draw it.
    pub fn show(&mut self, ctx: &Context) -> Option<Action> {
        if !self.open {
            return None;
        }

        let mut action = None;
        let mut open = self.open;

        egui::Window::new("Form")
            .open(&mut open)
            .default_width(440.0)
            .show(ctx, |ui| {
                action = self.body(ui);
            });

        self.open = open;
        action
    }

    fn body(&mut self, ui: &mut Ui) -> Option<Action> {
        if !self.loaded {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading…");
            });
            return None;
        }
        if !self.has_form {
            ui.label("This document has no form.");
            return None;
        }
        if self.fields.is_empty() {
            ui.label("This form has no fields.");
            return None;
        }

        let mut action = None;

        egui::ScrollArea::vertical()
            .max_height(360.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                egui::Grid::new("form-fields")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        // Collected first: the row draws from `draft`, which is
                        // borrowed mutably while `fields` is read.
                        let rows: Vec<Field> = self.fields.clone();
                        for field in &rows {
                            self.row(ui, field);
                            ui.end_row();
                        }
                    });
            });

        ui.separator();

        if let Some(report) = &self.report {
            let text = report.to_human();
            if report.is_complete() {
                ui.label(text.trim().to_string());
            } else {
                ui.colored_label(ui.visuals().warn_fg_color, text.trim().to_string());
            }
            ui.add_space(4.0);
        }
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
            ui.add_space(4.0);
        }

        let changed = !self.changes().is_empty();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(changed, egui::Button::new("Apply"))
                .clicked()
            {
                action = Some(Action::Apply);
            }
            if ui.button("Clear all").clicked() {
                action = Some(Action::Clear);
            }
            if ui.button("Import…").clicked() {
                action = Some(Action::Import);
            }
            if ui.button("Export…").clicked() {
                action = Some(Action::Export);
            }
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .button("Flatten")
                .on_hover_text("Draws the values into the page and removes the form. Permanent")
                .clicked()
            {
                action = Some(Action::Flatten);
            }
            ui.weak("Flattening cannot be undone.");
        });

        action
    }

    /// One field's label and editor.
    fn row(&mut self, ui: &mut Ui, field: &Field) {
        let mut label = field.name.clone();
        if field.required {
            label.push('*');
        }
        ui.label(label)
            .on_hover_text(format!("{} field", field.kind.label()));

        if !field.is_writable() {
            // Shown, never editable: a signature field is not something to type
            // into, and a read-only field was marked that way on purpose.
            let reason = match field.kind {
                Kind::Signature => "signature field",
                Kind::Button => "button",
                _ => "read-only",
            };
            ui.weak(if field.value.is_empty() {
                format!("({reason})")
            } else {
                format!("{}  ({reason})", field.value)
            });
            return;
        }

        let Some(value) = self.draft.get_mut(&field.name) else {
            return;
        };

        match field.kind {
            Kind::Checkbox => {
                let mut on = !value.is_empty();
                if ui.checkbox(&mut on, "").changed() {
                    // The engine turns this into whatever on-state the widget's
                    // own appearance names.
                    *value = if on { "yes".to_string() } else { String::new() };
                }
            }
            Kind::Radio | Kind::Choice if !field.options.is_empty() => {
                egui::ComboBox::from_id_salt(format!("form-{}", field.name))
                    .selected_text(if value.is_empty() {
                        "—"
                    } else {
                        value.as_str()
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(value, String::new(), "—");
                        for option in &field.options {
                            ui.selectable_value(value, option.clone(), option);
                        }
                    });
            }
            _ => {
                ui.add(egui::TextEdit::singleline(value).desired_width(240.0));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> Vec<Field> {
        vec![
            Field {
                name: "name".into(),
                kind: Kind::Text,
                value: "Ada".into(),
                ..Field::default()
            },
            Field {
                name: "total".into(),
                kind: Kind::Text,
                value: "100".into(),
                read_only: true,
                ..Field::default()
            },
            Field {
                name: "sign here".into(),
                kind: Kind::Signature,
                ..Field::default()
            },
        ]
    }

    #[test]
    fn loading_seeds_the_draft_from_the_document() {
        let mut panel = FormsPanel::default();
        panel.load(fields(), true);

        assert_eq!(panel.draft.get("name").map(String::as_str), Some("Ada"));
        assert!(panel.loaded);
        assert!(panel.changes().is_empty(), "nothing has been edited yet");
    }

    #[test]
    fn only_edited_fields_are_sent() {
        // Sending every field would rebuild appearances nobody touched, and a
        // diff of the two files would show changes that did not happen.
        let mut panel = FormsPanel::default();
        panel.load(fields(), true);
        panel
            .draft
            .insert("name".into(), "Ada Lovelace".to_string());

        let changes = panel.changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes.get("name").map(String::as_str),
            Some("Ada Lovelace")
        );
    }

    #[test]
    fn a_read_only_field_is_never_sent_even_if_the_draft_changed() {
        let mut panel = FormsPanel::default();
        panel.load(fields(), true);
        panel.draft.insert("total".into(), "0".to_string());

        assert!(panel.changes().is_empty());
    }

    #[test]
    fn a_signature_field_is_never_sent() {
        let mut panel = FormsPanel::default();
        panel.load(fields(), true);
        panel.draft.insert("sign here".into(), "Ada".to_string());

        assert!(panel.changes().is_empty());
    }

    #[test]
    fn invalidating_forgets_the_fields_and_the_last_report() {
        let mut panel = FormsPanel::default();
        panel.load(fields(), true);
        panel.report = Some(ypdf_forms::Report::default());

        panel.invalidate();
        assert!(!panel.loaded);
        assert!(panel.report.is_none());
    }
}
