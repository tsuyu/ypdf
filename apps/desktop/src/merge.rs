//! The merge dialog (spec §3.1).
//!
//! Merging is the mirror of splitting: several documents in, one file out, and
//! the open document is not changed. It is therefore not an [`crate::edit::Op`]
//! and never enters the undo log.
//!
//! The open document is the first entry because that is almost always what the
//! user means by "merge": this, followed by those. It can be removed like any
//! other entry, which turns the dialog into a plain file-to-file merge.
//!
//! Its pages are taken *as edited*, not as last saved, so a deletion or a
//! rotation made a moment ago is in the result.

use std::path::PathBuf;

use egui::{Context, Ui};

/// Where one entry's pages come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The document open in this tab, with its edits applied.
    Open,
    /// A file chosen from disk.
    File(PathBuf),
}

/// One document in the merge order.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Where its pages come from.
    pub source: Source,
    /// What to call it in the list.
    pub label: String,
    /// How many pages it contributes, read when it was added.
    pub pages: u32,
}

/// State of the merge dialog.
#[derive(Debug, Default)]
pub struct MergeDialog {
    /// Is it showing?
    pub open: bool,
    /// The documents to join, in the order they will be joined.
    pub entries: Vec<Entry>,
    /// Why the last attempt failed.
    pub error: Option<String>,
    /// Where the last merge landed.
    pub written: Option<PathBuf>,
}

/// What the dialog is asking the application to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Choose more files to add to the list.
    AddFiles,
    /// Join the listed documents and write the result.
    Run,
}

impl MergeDialog {
    /// Total pages the merge would produce.
    #[must_use]
    pub fn total_pages(&self) -> u32 {
        self.entries.iter().map(|e| e.pages).sum()
    }

    /// A merge of one document is a copy, so two is the real minimum — the
    /// same floor the CLI enforces.
    #[must_use]
    pub fn can_run(&self) -> bool {
        self.entries.len() >= 2
    }

    /// Put the open document at the head of an empty list.
    ///
    /// Only when empty: reopening the dialog must not push a second copy in
    /// front of an order the user has already arranged.
    pub fn seed(&mut self, label: String, pages: u32) {
        if self.entries.is_empty() {
            self.entries.push(Entry {
                source: Source::Open,
                label,
                pages,
            });
        }
    }

    /// Keep the open document's entry in step with the document itself.
    ///
    /// Editing the document while the dialog is up changes how many pages it
    /// will contribute. A list still claiming the old count promises a merge
    /// that will not happen — the merge takes the document as edited.
    pub fn refresh_open(&mut self, pages: u32) {
        for entry in &mut self.entries {
            if entry.source == Source::Open {
                entry.pages = pages;
            }
        }
    }

    /// Add a file to the end of the list.
    pub fn push_file(&mut self, path: PathBuf, pages: u32) {
        let label = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        self.entries.push(Entry {
            source: Source::File(path),
            label,
            pages,
        });
        // The list now describes a different merge from the one reported.
        self.written = None;
    }

    /// Draw the dialog. Returns what the user asked for, if anything.
    pub fn show(&mut self, ctx: &Context) -> Option<Action> {
        if !self.open {
            return None;
        }

        let mut action = None;
        let mut open = self.open;

        egui::Window::new("Merge")
            .open(&mut open)
            .resizable(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                self.list(ui);
                ui.separator();
                ui.weak(self.summary());
                ui.add_space(6.0);
                action = self.controls(ui);
            });

        self.open = open;
        action
    }

    fn list(&mut self, ui: &mut Ui) {
        if self.entries.is_empty() {
            ui.weak("Nothing to merge yet.");
            return;
        }

        // Reordering while walking the list would invalidate the indices the
        // rest of the pass is using, so the change is recorded and applied
        // once the list has been drawn.
        let mut pending: Option<Change> = None;
        let last = self.entries.len() - 1;

        egui::ScrollArea::vertical()
            .max_height(220.0)
            .show(ui, |ui| {
                for (i, entry) in self.entries.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("{}.", i + 1));
                        ui.label(&entry.label);
                        ui.weak(format!("{} pages", entry.pages));
                        if entry.source == Source::Open {
                            ui.weak("(open, as edited)");
                        }

                        // Right-aligned so the controls line up however long
                        // the names are.
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button("x")
                                .on_hover_text("Remove from the merge")
                                .clicked()
                            {
                                pending = Some(Change::Remove(i));
                            }
                            if ui
                                .add_enabled(i < last, egui::Button::new("v").small())
                                .on_hover_text("Later")
                                .clicked()
                            {
                                pending = Some(Change::Down(i));
                            }
                            if ui
                                .add_enabled(i > 0, egui::Button::new("^").small())
                                .on_hover_text("Earlier")
                                .clicked()
                            {
                                pending = Some(Change::Up(i));
                            }
                        });
                    });
                }
            });

        if let Some(change) = pending {
            self.apply(change);
        }
    }

    fn apply(&mut self, change: Change) {
        match change {
            Change::Up(i) if i > 0 => self.entries.swap(i - 1, i),
            Change::Down(i) if i + 1 < self.entries.len() => self.entries.swap(i, i + 1),
            Change::Remove(i) if i < self.entries.len() => {
                self.entries.remove(i);
            }
            _ => return,
        }
        // The order on screen is no longer the order that was written.
        self.written = None;
        self.error = None;
    }

    /// A sentence saying what pressing Merge would produce.
    fn summary(&self) -> String {
        match self.entries.len() {
            0 => "Add at least two documents.".to_string(),
            1 => "One document. Add another: merging one is just a copy.".to_string(),
            n => format!("{n} documents, {} pages into one file", self.total_pages()),
        }
    }

    fn controls(&mut self, ui: &mut Ui) -> Option<Action> {
        let mut action = None;

        ui.horizontal(|ui| {
            if ui.button("Add files…").clicked() {
                action = Some(Action::AddFiles);
            }
            if ui
                .add_enabled(self.can_run(), egui::Button::new("Merge…"))
                .on_hover_text("Choose where to write the joined document")
                .on_disabled_hover_text("Two documents are the minimum")
                .clicked()
            {
                action = Some(Action::Run);
            }
            ui.weak("The open document is not changed.");
        });

        if let Some(error) = &self.error {
            ui.add_space(4.0);
            ui.colored_label(ui.visuals().error_fg_color, error);
        }

        if let Some(path) = &self.written {
            ui.add_space(4.0);
            ui.label(format!("Wrote {}", path.display()));
        }

        action
    }
}

/// A reordering recorded during a draw pass, applied once it is over.
#[derive(Clone, Copy, Debug)]
enum Change {
    Up(usize),
    Down(usize),
    Remove(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog() -> MergeDialog {
        let mut dialog = MergeDialog::default();
        dialog.seed("many-pages.pdf".to_string(), 12);
        dialog.push_file(PathBuf::from("/tmp/two-pages.pdf"), 2);
        dialog
    }

    #[test]
    fn a_fresh_dialog_is_closed_and_has_nothing_to_merge() {
        let dialog = MergeDialog::default();
        assert!(!dialog.open);
        assert!(dialog.entries.is_empty());
        assert!(!dialog.can_run(), "nothing is not a merge");
    }

    #[test]
    fn the_open_document_leads_and_is_seeded_only_once() {
        let mut dialog = dialog();
        assert_eq!(dialog.entries[0].source, Source::Open);
        assert_eq!(dialog.entries[1].label, "two-pages.pdf");

        dialog.seed("many-pages.pdf".to_string(), 12);
        assert_eq!(
            dialog.entries.len(),
            2,
            "reopening must not stack a second copy of the open document"
        );
    }

    #[test]
    fn one_document_is_a_copy_not_a_merge() {
        let mut dialog = MergeDialog::default();
        dialog.seed("only.pdf".to_string(), 3);
        assert!(!dialog.can_run());
        assert_eq!(
            dialog.summary(),
            "One document. Add another: merging one is just a copy."
        );

        dialog.push_file(PathBuf::from("/tmp/other.pdf"), 4);
        assert!(dialog.can_run());
        assert_eq!(dialog.summary(), "2 documents, 7 pages into one file");
    }

    #[test]
    fn entries_move_and_the_ends_stay_put() {
        let mut dialog = dialog();
        dialog.apply(Change::Down(0));
        assert_eq!(dialog.entries[0].label, "two-pages.pdf");

        dialog.apply(Change::Up(1));
        assert_eq!(dialog.entries[0].source, Source::Open);

        // Off the ends: nothing moves, and nothing panics.
        dialog.apply(Change::Up(0));
        dialog.apply(Change::Down(1));
        assert_eq!(dialog.entries[0].source, Source::Open);
        assert_eq!(dialog.entries[1].label, "two-pages.pdf");
    }

    #[test]
    fn removing_an_entry_drops_it_and_the_stale_report() {
        let mut dialog = dialog();
        dialog.written = Some(PathBuf::from("/tmp/merged.pdf"));

        dialog.apply(Change::Remove(0));
        assert_eq!(dialog.entries.len(), 1);
        assert_eq!(dialog.entries[0].label, "two-pages.pdf");
        assert!(
            dialog.written.is_none(),
            "the report describes an order that no longer exists"
        );
    }

    #[test]
    fn the_open_entry_follows_the_document_as_it_is_edited() {
        let mut dialog = dialog();
        assert_eq!(dialog.total_pages(), 14);

        // A page was deleted while the dialog was up.
        dialog.refresh_open(11);
        assert_eq!(dialog.entries[0].pages, 11);
        assert_eq!(
            dialog.total_pages(),
            13,
            "the promise has to match what the merge would write"
        );
        assert_eq!(
            dialog.entries[1].pages, 2,
            "files on disk are untouched by an edit to the open document"
        );
    }

    #[test]
    fn total_pages_adds_every_entry_up() {
        let mut dialog = dialog();
        dialog.push_file(PathBuf::from("/tmp/form.pdf"), 1);
        assert_eq!(dialog.total_pages(), 15);
    }
}
