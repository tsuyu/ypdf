//! The export panel (spec §5).
//!
//! Conversion shows its work before anything is written. A PDF does not record
//! paragraphs or headings, so the output is a reconstruction, and how good it
//! is depends entirely on the document — which the person looking at it can
//! judge in a second and this code cannot judge at all. So the panel converts,
//! shows what it found and everything it was unsure about, and only then
//! offers to save it.
//!
//! Markdown and Word are two writers over one reconstruction, so the panel
//! holds the rebuilt structure rather than either rendering of it. The preview
//! is Markdown because it is the one that can be read as plain text.
//!
//! Reading a text layer is the render thread's work and arrives a page at a
//! time, so the panel spends a few frames waiting. That wait is shown rather
//! than hidden behind a frozen window.

use egui::{Context, Ui};
use ypdf_convert::Document;

/// State of the export panel.
#[derive(Debug, Default)]
pub struct ExportPanel {
    /// Is it showing?
    pub open: bool,
    /// True while the pages are still being read.
    pub waiting: bool,
    /// The rebuilt document.
    pub result: Option<Document>,
    /// Its Markdown, rendered once when the conversion finished.
    pub preview: String,
    /// Why the last attempt failed.
    pub error: Option<String>,
}

/// What the panel is asking the application to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Read every page and rebuild the structure.
    Convert,
    /// Write the Markdown to a file.
    SaveMarkdown,
    /// Write a Word document to a file.
    SaveDocx,
    /// Put the Markdown on the clipboard.
    Copy,
}

impl ExportPanel {
    /// Forget a conversion that no longer describes the document.
    pub fn invalidate(&mut self) {
        self.result = None;
        self.preview.clear();
        self.waiting = false;
        self.error = None;
    }

    /// Draw the panel. Returns what the user asked for, if anything.
    ///
    /// `ready` and `total` are the pages that have settled and the pages there
    /// are, which is the only honest progress report available: pages come
    /// back in whatever order the render thread finishes them.
    pub fn show(&mut self, ctx: &Context, ready: usize, total: usize) -> Option<Action> {
        if !self.open {
            return None;
        }

        let mut action = None;
        let mut open = self.open;

        egui::Window::new("Export")
            .open(&mut open)
            .resizable(true)
            .default_width(560.0)
            .default_height(480.0)
            .show(ctx, |ui| {
                action = self.body(ui, ready, total);
            });

        self.open = open;
        action
    }

    fn body(&mut self, ui: &mut Ui, ready: usize, total: usize) -> Option<Action> {
        let mut action = None;

        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
            ui.add_space(6.0);
        }

        if self.waiting {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Reading the text of page {ready} of {total}…"));
            });
            return None;
        }

        let Some(document) = &self.result else {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(total > 0, egui::Button::new("Convert"))
                    .on_hover_text("Reads the text of every page")
                    .clicked()
                {
                    action = Some(Action::Convert);
                }
                ui.weak("The open document is not changed.");
            });
            return action;
        };

        // The warnings come first because they decide whether what is below
        // them is worth saving.
        for warning in &document.warnings {
            ui.colored_label(ui.visuals().warn_fg_color, warning.message());
        }
        if !document.warnings.is_empty() {
            ui.add_space(6.0);
        }

        ui.horizontal(|ui| {
            if ui.button("Save Markdown…").clicked() {
                action = Some(Action::SaveMarkdown);
            }
            if ui.button("Save Word…").clicked() {
                action = Some(Action::SaveDocx);
            }
            if ui
                .button("Copy")
                .on_hover_text("Copies the Markdown")
                .clicked()
            {
                action = Some(Action::Copy);
            }
            if ui.button("Convert again").clicked() {
                action = Some(Action::Convert);
            }
        });

        ui.weak(summary(document));
        ui.separator();

        if self.preview.is_empty() {
            ui.weak("Nothing came out. The document has no text layer to read.");
            return action;
        }

        // Shown as text so it can be selected by hand, but not editable: the
        // buffer is a copy, edits would be thrown away on the next conversion,
        // and pretending otherwise would be a lie.
        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut preview = self.preview.clone();
            ui.add(
                egui::TextEdit::multiline(&mut preview)
                    .desired_width(f32::INFINITY)
                    .desired_rows(18)
                    .font(egui::TextStyle::Monospace)
                    .interactive(false),
            );
        });

        action
    }
}

/// What the reconstruction found, in one line.
fn summary(document: &Document) -> String {
    let mut headings = 0;
    let mut paragraphs = 0;
    let mut items = 0;
    for block in &document.blocks {
        match block {
            ypdf_convert::Block::Heading { .. } => headings += 1,
            ypdf_convert::Block::Paragraph { .. } => paragraphs += 1,
            ypdf_convert::Block::ListItem { .. } => items += 1,
        }
    }
    format!("{headings} heading(s), {paragraphs} paragraph(s), {items} list item(s)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ypdf_convert::{Block, Run, Warning};

    fn block(text: &str) -> Block {
        Block::Paragraph {
            runs: vec![Run {
                text: text.to_string(),
                bold: false,
                italic: false,
            }],
        }
    }

    #[test]
    fn a_fresh_panel_is_closed_with_nothing_converted() {
        let panel = ExportPanel::default();
        assert!(!panel.open);
        assert!(panel.result.is_none());
        assert!(!panel.waiting);
    }

    #[test]
    fn invalidating_drops_the_result_and_stops_the_wait() {
        let mut panel = ExportPanel {
            open: true,
            waiting: true,
            result: Some(Document::default()),
            preview: "# Old".to_string(),
            error: Some("stale".to_string()),
        };

        panel.invalidate();

        assert!(panel.result.is_none(), "the document has moved on");
        assert!(panel.preview.is_empty());
        assert!(
            !panel.waiting,
            "pages arriving now describe the old document"
        );
        assert!(panel.error.is_none());
        assert!(panel.open, "invalidating is not closing");
    }

    #[test]
    fn the_summary_counts_what_was_rebuilt() {
        let document = Document {
            blocks: vec![
                Block::Heading {
                    level: 1,
                    runs: vec![],
                },
                block("one"),
                block("two"),
                Block::ListItem {
                    runs: vec![],
                    ordered: false,
                },
            ],
            warnings: vec![Warning::NoTextLayer { page: 2 }],
        };
        assert_eq!(
            summary(&document),
            "1 heading(s), 2 paragraph(s), 1 list item(s)"
        );
    }
}
