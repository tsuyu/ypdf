//! The pictures-into-PDF dialog (spec §6).
//!
//! Shaped like the merge dialog, because it is the same shape of job: a list
//! the user arranges, then one file out, and the open document untouched. It
//! is not an [`crate::edit::Op`] and never enters the undo log.
//!
//! The one thing this dialog has that merging does not is a choice about
//! paper. Someone scanning receipts wants each page to *be* the receipt;
//! someone assembling a report wants every page to be A4 whatever the photos
//! were. Neither is a sane default for the other, so both are on screen with
//! the consequence spelled out rather than hidden in a settings panel.

use std::path::PathBuf;

use egui::{Context, Ui};
use ypdf_image::{Options, PageFit, PageSize};

/// One picture in the order it will appear.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Where to read it from.
    pub path: PathBuf,
    /// What to call it in the list.
    pub label: String,
}

/// Which paper the pages are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paper {
    /// The page is the picture, at the chosen resolution.
    Picture,
    /// A4.
    A4,
    /// US Letter.
    Letter,
    /// US Legal.
    Legal,
}

impl Paper {
    const ALL: [Self; 4] = [Self::Picture, Self::A4, Self::Letter, Self::Legal];

    const fn label(self) -> &'static str {
        match self {
            Self::Picture => "Picture size",
            Self::A4 => "A4",
            Self::Letter => "Letter",
            Self::Legal => "Legal",
        }
    }

    const fn size(self) -> Option<PageSize> {
        match self {
            Self::Picture => None,
            Self::A4 => Some(PageSize::A4),
            Self::Letter => Some(PageSize::LETTER),
            Self::Legal => Some(PageSize::LEGAL),
        }
    }
}

/// State of the dialog.
#[derive(Debug)]
pub struct ImagesDialog {
    /// Is it showing?
    pub open: bool,
    /// The pictures, in page order.
    pub entries: Vec<Entry>,
    /// Which paper to use.
    pub paper: Paper,
    /// Resolution when the page is the picture.
    pub dpi: f32,
    /// Margin in points when the page is fixed.
    pub margin: f32,
    /// Turn the sheet for a picture wider than it is tall.
    pub auto_orient: bool,
    /// Why the last attempt failed.
    pub error: Option<String>,
    /// Where the last run landed, and what it left out.
    pub written: Option<(PathBuf, Vec<String>)>,
}

impl Default for ImagesDialog {
    fn default() -> Self {
        Self {
            open: false,
            entries: Vec::new(),
            // Scans are the common case and a scan already knows its own size.
            paper: Paper::Picture,
            dpi: 300.0,
            margin: 36.0,
            auto_orient: true,
            error: None,
            written: None,
        }
    }
}

/// What the dialog is asking the application to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Choose pictures to add.
    AddFiles,
    /// Build the document and write it.
    Run,
}

/// A reordering, applied after the list has been drawn.
#[derive(Clone, Copy, Debug)]
enum Change {
    Up(usize),
    Down(usize),
    Remove(usize),
}

impl ImagesDialog {
    /// Can it run? One picture is a perfectly good one-page PDF.
    #[must_use]
    pub fn can_run(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Add a picture to the end of the list.
    pub fn push(&mut self, path: PathBuf) {
        let label = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        self.entries.push(Entry { path, label });
        // The list now describes a different document from the one reported.
        self.written = None;
        self.error = None;
    }

    /// The settings as the engine wants them.
    #[must_use]
    pub fn options(&self) -> Options {
        Options {
            fit: match self.paper.size() {
                Some(size) => PageFit::Fixed {
                    size,
                    margin: self.margin,
                },
                None => PageFit::Image { dpi: self.dpi },
            },
            auto_orient: self.auto_orient,
        }
    }

    /// Draw the dialog. Returns what the user asked for, if anything.
    pub fn show(&mut self, ctx: &Context) -> Option<Action> {
        if !self.open {
            return None;
        }

        let mut action = None;
        let mut open = self.open;

        egui::Window::new("Pictures to PDF")
            .open(&mut open)
            .resizable(false)
            .default_width(480.0)
            .show(ctx, |ui| {
                self.list(ui);
                ui.separator();
                self.settings(ui);
                ui.separator();
                action = self.controls(ui);
            });

        self.open = open;
        action
    }

    fn list(&mut self, ui: &mut Ui) {
        if self.entries.is_empty() {
            ui.weak("No pictures yet. JPEG, PNG, WebP and TIFF.");
            return;
        }

        // Reordering mid-walk would invalidate the indices the rest of the
        // pass is using, so the change is recorded and applied afterwards.
        let mut pending: Option<Change> = None;
        let last = self.entries.len() - 1;

        egui::ScrollArea::vertical()
            .max_height(220.0)
            .show(ui, |ui| {
                for (i, entry) in self.entries.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("{}.", i + 1));
                        ui.label(&entry.label);

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button("x")
                                .on_hover_text("Leave this one out")
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
        self.written = None;
        self.error = None;
    }

    fn settings(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label("Pages:");
            for paper in Paper::ALL {
                if ui
                    .selectable_label(self.paper == paper, paper.label())
                    .clicked()
                {
                    self.paper = paper;
                    self.written = None;
                }
            }
        });

        if self.paper == Paper::Picture {
            ui.horizontal(|ui| {
                ui.label("Resolution:");
                ui.add(
                    egui::DragValue::new(&mut self.dpi)
                        .speed(10.0)
                        .range(18.0..=1200.0)
                        .suffix(" dpi"),
                );
                ui.weak("Each page is exactly its picture. Nothing is cropped or padded.");
            });
        } else {
            ui.horizontal(|ui| {
                ui.label("Margin:");
                ui.add(
                    egui::DragValue::new(&mut self.margin)
                        .speed(1.0)
                        .range(0.0..=144.0)
                        .suffix(" pt"),
                );
                ui.checkbox(&mut self.auto_orient, "Turn the page for wide pictures");
            });
            ui.weak("Every picture is scaled to fit and centred on the same paper.");
        }
    }

    fn controls(&mut self, ui: &mut Ui) -> Option<Action> {
        let mut action = None;

        ui.horizontal(|ui| {
            if ui.button("Add pictures…").clicked() {
                action = Some(Action::AddFiles);
            }
            if ui
                .add_enabled(self.can_run(), egui::Button::new("Save PDF…"))
                .on_hover_text("Choose where to write the document")
                .on_disabled_hover_text("Add at least one picture")
                .clicked()
            {
                action = Some(Action::Run);
            }
            ui.weak(self.summary());
        });

        if let Some(error) = &self.error {
            ui.add_space(4.0);
            ui.colored_label(ui.visuals().error_fg_color, error);
        }

        if let Some((path, skipped)) = &self.written {
            ui.add_space(4.0);
            ui.weak(format!("Written to {}", path.display()));
            // Anything left out is said here rather than silently missing from
            // a document the user is about to send someone.
            for line in skipped {
                ui.colored_label(ui.visuals().warn_fg_color, line);
            }
        }

        action
    }

    /// What pressing the button would produce.
    fn summary(&self) -> String {
        match self.entries.len() {
            // Not "the open document is not changed": this dialog is reachable
            // with nothing open at all, and saying otherwise would be a small
            // lie on the most common first screen.
            0 => "Writes a new file.".to_string(),
            1 => "1 picture, 1 page".to_string(),
            n => format!("{n} pictures, {n} pages"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog_with(names: &[&str]) -> ImagesDialog {
        let mut dialog = ImagesDialog::default();
        for name in names {
            dialog.push(PathBuf::from(name));
        }
        dialog
    }

    #[test]
    fn a_fresh_dialog_is_closed_and_empty() {
        let dialog = ImagesDialog::default();
        assert!(!dialog.open);
        assert!(!dialog.can_run(), "nothing to convert yet");
    }

    #[test]
    fn one_picture_is_enough_unlike_a_merge() {
        assert!(dialog_with(&["a.png"]).can_run());
    }

    #[test]
    fn entries_are_labelled_by_file_name() {
        let dialog = dialog_with(&[r"C:\scans\receipt-01.jpg"]);
        assert_eq!(dialog.entries[0].label, "receipt-01.jpg");
    }

    #[test]
    fn reordering_moves_the_page_not_the_file() {
        let mut dialog = dialog_with(&["a.png", "b.png", "c.png"]);
        dialog.apply(Change::Down(0));
        let order: Vec<&str> = dialog.entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(order, ["b.png", "a.png", "c.png"]);

        dialog.apply(Change::Up(2));
        let order: Vec<&str> = dialog.entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(order, ["b.png", "c.png", "a.png"]);
    }

    #[test]
    fn moving_past_either_end_is_ignored() {
        let mut dialog = dialog_with(&["a.png", "b.png"]);
        dialog.apply(Change::Up(0));
        dialog.apply(Change::Down(1));
        let order: Vec<&str> = dialog.entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(order, ["a.png", "b.png"], "the list should be untouched");
    }

    #[test]
    fn removing_the_last_picture_disables_the_button() {
        let mut dialog = dialog_with(&["only.png"]);
        dialog.apply(Change::Remove(0));
        assert!(!dialog.can_run());
    }

    #[test]
    fn changing_the_list_forgets_where_the_last_run_landed() {
        let mut dialog = dialog_with(&["a.png"]);
        dialog.written = Some((PathBuf::from("out.pdf"), Vec::new()));
        dialog.push(PathBuf::from("b.png"));
        assert!(
            dialog.written.is_none(),
            "that path no longer describes this list"
        );
    }

    #[test]
    fn picture_size_asks_for_a_resolution_and_paper_asks_for_a_margin() {
        let mut dialog = dialog_with(&["a.png"]);
        dialog.dpi = 150.0;
        assert_eq!(
            dialog.options().fit,
            PageFit::Image { dpi: 150.0 },
            "picture size means the page follows the pixels"
        );

        dialog.paper = Paper::A4;
        dialog.margin = 20.0;
        assert_eq!(
            dialog.options().fit,
            PageFit::Fixed {
                size: PageSize::A4,
                margin: 20.0
            }
        );
    }

    #[test]
    fn every_paper_but_picture_size_names_a_sheet() {
        for paper in Paper::ALL {
            assert_eq!(
                paper.size().is_none(),
                paper == Paper::Picture,
                "{paper:?} disagrees with itself"
            );
        }
    }
}
