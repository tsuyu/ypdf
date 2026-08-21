//! The compression dialog (spec §4, §29).
//!
//! Compression is the one operation that is both slow and irreversible in
//! feel — it throws pixels away — so it gets a dialog with the settings
//! visible, a preview of what happened, and an explicit save afterwards. It
//! never overwrites the original by itself.

use egui::{Context, RichText, Ui};
use ypdf_core::preset::{Compression, Preset};
use ypdf_optimize::{OptimizeReport, format_size};

/// State of the compression dialog.
pub struct CompressDialog {
    /// Is it showing?
    pub open: bool,
    /// The settings being edited.
    pub settings: Compression,
    /// Name of the preset those settings came from.
    pub preset_name: String,
    /// Also strip metadata (spec §4). Off by default: it is a privacy and
    /// provenance decision, not a size one.
    pub strip_metadata: bool,
    /// The last run's result.
    pub report: Option<OptimizeReport>,
    /// Why the last run failed.
    pub error: Option<String>,
    /// True while a run is in progress.
    pub running: bool,
}

impl Default for CompressDialog {
    fn default() -> Self {
        Self {
            open: false,
            settings: Compression::default(),
            // Not one of the builtins: the defaults are the engine's, and
            // naming a preset that was never chosen would be a small lie.
            preset_name: "Custom".to_string(),
            strip_metadata: false,
            report: None,
            error: None,
            running: false,
        }
    }
}

/// What the dialog is asking the application to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Run compression with the current settings.
    Run,
    /// Keep the compressed document and save it somewhere.
    SaveAs,
    /// Throw the result away and restore the document.
    Discard,
}

impl CompressDialog {
    /// Draw the dialog. Returns what the user asked for, if anything.
    pub fn show(&mut self, ctx: &Context) -> Option<Action> {
        if !self.open {
            return None;
        }

        let mut action = None;
        let mut open = self.open;

        egui::Window::new("Compress")
            .open(&mut open)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                self.presets(ui);
                ui.separator();
                self.controls(ui);
                ui.separator();
                action = self.results(ui);
            });

        self.open = open;
        action
    }

    fn presets(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Preset");
            for preset in Preset::builtin() {
                // Only the presets that actually describe a compression
                // setting; "Company Standard" is a starting point, not a level.
                let selected = self.preset_name == preset.name();
                if ui.selectable_label(selected, preset.name()).clicked() {
                    self.preset_name = preset.name().to_string();
                    self.settings = preset.compression;
                    self.report = None;
                }
            }
        });
        if let Some(description) = Preset::builtin()
            .iter()
            .find(|p| p.name() == self.preset_name)
            .and_then(|p| p.preset.description.clone())
        {
            ui.weak(description);
        }
    }

    fn controls(&mut self, ui: &mut Ui) {
        let mut changed = false;

        egui::Grid::new("compress-settings")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Image resolution");
                changed |= ui
                    .add(egui::Slider::new(&mut self.settings.dpi, 50..=600).suffix(" dpi"))
                    .on_hover_text("Images drawn larger than this are downsampled")
                    .changed();
                ui.end_row();

                ui.label("JPEG quality");
                changed |= ui
                    .add(egui::Slider::new(&mut self.settings.quality, 20..=100))
                    .on_hover_text("100 keeps every pixel: resample only, never re-encode")
                    .changed();
                ui.end_row();
            });

        ui.add_space(4.0);
        changed |= ui
            .checkbox(
                &mut self.settings.remove_unused_objects,
                "Remove unused objects",
            )
            .changed();
        changed |= ui
            .checkbox(&mut self.settings.compress_streams, "Compress streams")
            .changed();
        changed |= ui
            .checkbox(&mut self.strip_metadata, "Remove metadata")
            .on_hover_text("Deletes the title, author, dates, and XMP packet")
            .changed();

        if self.settings.quality >= 100 {
            ui.add_space(4.0);
            ui.weak("At quality 100 images are only resampled, never re-encoded.");
        }

        if changed {
            self.preset_name = "Custom".to_string();
            self.report = None;
        }
    }

    fn results(&mut self, ui: &mut Ui) -> Option<Action> {
        let mut action = None;

        if self.running {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Compressing…");
            });
            return None;
        }

        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
            ui.add_space(4.0);
        }

        match &self.report {
            None => {
                ui.horizontal(|ui| {
                    if ui.button("Compress").clicked() {
                        action = Some(Action::Run);
                    }
                    ui.weak("The original file is not touched.");
                });
            }
            Some(report) => {
                stats(ui, report);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save as…").clicked() {
                        action = Some(Action::SaveAs);
                    }
                    if ui.button("Discard").clicked() {
                        action = Some(Action::Discard);
                    }
                    if ui.button("Compress again").clicked() {
                        action = Some(Action::Run);
                    }
                });
            }
        }

        action
    }
}

/// The before/after block from spec §4.
fn stats(ui: &mut Ui, report: &OptimizeReport) {
    egui::Grid::new("compress-stats")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            ui.label("Original");
            ui.label(format_size(report.before));
            ui.end_row();

            ui.label("Optimized");
            ui.label(format_size(report.after));
            ui.end_row();

            ui.label("Reduction");
            let text = format!("{:.1}%", report.reduction() * 100.0);
            if report.is_improvement() {
                ui.label(RichText::new(text).strong());
            } else {
                // Saying "0.0%" plainly is better than dressing up a failure.
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "none — this file is already small",
                );
            }
            ui.end_row();
        });

    if report.images_recompressed > 0 {
        ui.add_space(6.0);
        ui.label(format!(
            "{} image(s) recompressed, {} saved",
            report.images_recompressed,
            format_size(report.image_bytes_saved)
        ));
    }

    if !report.images_skipped.is_empty() {
        let detail: Vec<String> = report
            .images_skipped
            .iter()
            .map(|(why, n)| format!("{n} {why}"))
            .collect();
        ui.weak(format!("Left alone: {}", detail.join(", ")));
    }

    if !report.not_supported.is_empty() {
        ui.add_space(4.0);
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!("Not done: {}", report.not_supported.join(", ")),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_dialog_starts_closed_with_balanced_defaults() {
        let dialog = CompressDialog::default();
        assert!(!dialog.open);
        assert!(dialog.report.is_none());
        assert!(!dialog.strip_metadata, "metadata removal is opt-in");
        assert_eq!(dialog.settings.dpi, Compression::default().dpi);
    }

    #[test]
    fn every_builtin_preset_carries_compression_settings() {
        for preset in Preset::builtin() {
            assert!(
                preset.compression.dpi >= 50,
                "{} has an unusable dpi",
                preset.name()
            );
            assert!(
                preset.compression.quality >= 20,
                "{} has an unusable quality",
                preset.name()
            );
        }
    }

    #[test]
    fn maximum_compression_is_more_aggressive_than_print() {
        let presets = Preset::builtin();
        let max = presets
            .iter()
            .find(|p| p.name() == "Maximum Compression")
            .expect("preset");
        let print = presets
            .iter()
            .find(|p| p.name() == "Print PDF")
            .expect("preset");

        assert!(max.compression.dpi < print.compression.dpi);
        assert!(max.compression.quality < print.compression.quality);
    }
}
