//! The watermark dialog (spec §12).
//!
//! Stamping is additive and reversible-by-deletion, but it still changes
//! someone's document, so it follows the same rule as compression and OCR: the
//! result is held in memory and written only when the user picks a file.

use egui::{Context, Ui};
use ypdf_watermark::{Colour, Content, Face, Placement, Position, Watermark};

/// State of the dialog.
#[derive(Debug)]
pub struct WatermarkDialog {
    /// Is it showing?
    pub open: bool,
    /// The words to stamp.
    pub text: String,
    /// An image to stamp instead, if one was chosen.
    pub image: Option<(std::path::PathBuf, Vec<u8>)>,
    /// Where it sits.
    pub position: Position,
    /// Degrees anticlockwise.
    pub rotation: f32,
    /// 0.0 to 1.0.
    pub opacity: f32,
    /// Size multiplier.
    pub scale: f32,
    /// Colour, as `#rrggbb`.
    pub colour: String,
    /// Bold rather than regular.
    pub bold: bool,
    /// Behind the page rather than on top.
    pub under: bool,
    /// Stamp every page, or only the one being read.
    pub all_pages: bool,
    /// The stamped document, waiting to be saved.
    pub result: Option<Vec<u8>>,
    /// How many pages the last run stamped.
    pub stamped: usize,
    /// Why the last attempt failed.
    pub error: Option<String>,
}

impl Default for WatermarkDialog {
    fn default() -> Self {
        Self {
            open: false,
            text: "CONFIDENTIAL".to_string(),
            image: None,
            position: Position::Center,
            rotation: 45.0,
            // Faint enough to read the page through, solid enough to see.
            opacity: 0.15,
            scale: 1.0,
            colour: "808080".to_string(),
            bold: true,
            under: false,
            all_pages: true,
            result: None,
            stamped: 0,
            error: None,
        }
    }
}

/// What the dialog is asking for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Stamp the document.
    Apply,
    /// Choose an image to stamp.
    ChooseImage,
    /// Go back to stamping text.
    ClearImage,
    /// Keep the result and write it somewhere.
    SaveAs,
    /// Throw it away.
    Discard,
}

impl WatermarkDialog {
    /// The watermark as configured, or the reason it cannot be built.
    pub fn watermark(&self) -> Result<Watermark, String> {
        let content = match &self.image {
            Some((_, bytes)) => Content::Image {
                bytes: bytes.clone(),
            },
            None => {
                if self.text.trim().is_empty() {
                    return Err("Type something to stamp.".to_string());
                }
                Content::Text {
                    text: self.text.clone(),
                    face: if self.bold { Face::Bold } else { Face::Regular },
                    size: None,
                    colour: Colour::parse(&self.colour)
                        .map_err(|_| "That is not a colour; try #808080.".to_string())?,
                }
            }
        };

        Ok(Watermark {
            content,
            position: self.position,
            rotation: self.rotation,
            opacity: self.opacity,
            scale: self.scale,
            placement: if self.under {
                Placement::Under
            } else {
                Placement::Over
            },
        })
    }

    /// Draw it.
    pub fn show(&mut self, ctx: &Context) -> Option<Action> {
        if !self.open {
            return None;
        }

        let mut action = None;
        let mut open = self.open;

        egui::Window::new("Watermark")
            .open(&mut open)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                action = self.body(ui);
            });

        self.open = open;
        action
    }

    fn body(&mut self, ui: &mut Ui) -> Option<Action> {
        let mut action = None;

        self.source(ui, &mut action);
        ui.separator();
        self.placement(ui);
        ui.separator();

        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
            ui.add_space(4.0);
        }

        if self.result.is_some() {
            ui.label(format!("{} page(s) stamped.", self.stamped));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Save as…").clicked() {
                    action = Some(Action::SaveAs);
                }
                if ui.button("Discard").clicked() {
                    action = Some(Action::Discard);
                }
                if ui.button("Stamp again").clicked() {
                    action = Some(Action::Apply);
                }
            });
            return action;
        }

        ui.horizontal(|ui| {
            if ui.button("Stamp").clicked() {
                action = Some(Action::Apply);
            }
            ui.weak("The original file is not touched.");
        });

        action
    }

    fn source(&mut self, ui: &mut Ui, action: &mut Option<Action>) {
        match &self.image {
            Some((path, bytes)) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Image");
                    ui.strong(
                        path.file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                    );
                    ui.weak(ypdf_optimize::format_size(bytes.len() as u64));
                });
                if ui.button("Use text instead").clicked() {
                    *action = Some(Action::ClearImage);
                }
            }
            None => {
                ui.horizontal(|ui| {
                    ui.label("Text");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.text)
                            .desired_width(200.0)
                            .hint_text("CONFIDENTIAL"),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Colour");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.colour)
                            .desired_width(90.0)
                            .hint_text("#808080"),
                    );
                    ui.checkbox(&mut self.bold, "Bold");
                    if ui.button("Use an image…").clicked() {
                        *action = Some(Action::ChooseImage);
                    }
                });

                ui.horizontal_wrapped(|ui| {
                    for (label, text) in [
                        ("CONFIDENTIAL", "CONFIDENTIAL"),
                        ("DRAFT", "DRAFT"),
                        ("COPY", "COPY"),
                        ("INTERNAL", "INTERNAL"),
                    ] {
                        if ui.selectable_label(self.text == text, label).clicked() {
                            self.text = text.to_string();
                        }
                    }
                });
            }
        }
    }

    fn placement(&mut self, ui: &mut Ui) {
        egui::Grid::new("watermark-placement")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Position");
                egui::ComboBox::from_id_salt("watermark-position")
                    .selected_text(position_label(self.position))
                    .show_ui(ui, |ui| {
                        for position in [
                            Position::Center,
                            Position::TopLeft,
                            Position::TopCenter,
                            Position::TopRight,
                            Position::BottomLeft,
                            Position::BottomCenter,
                            Position::BottomRight,
                            Position::Tiled,
                        ] {
                            ui.selectable_value(
                                &mut self.position,
                                position,
                                position_label(position),
                            );
                        }
                    });
                ui.end_row();

                ui.label("Rotation");
                ui.add(egui::Slider::new(&mut self.rotation, -90.0..=90.0).suffix("°"));
                ui.end_row();

                ui.label("Opacity");
                ui.add(egui::Slider::new(&mut self.opacity, 0.02..=1.0));
                ui.end_row();

                ui.label("Size");
                ui.add(egui::Slider::new(&mut self.scale, 0.1..=2.0).suffix("x"));
                ui.end_row();
            });

        ui.checkbox(&mut self.under, "Draw behind the page")
            .on_hover_text("Nothing is obscured, but opaque content on the page hides it");
        ui.checkbox(&mut self.all_pages, "Every page");
    }
}

const fn position_label(position: Position) -> &'static str {
    match position {
        Position::Center => "Centre",
        Position::TopLeft => "Top left",
        Position::TopCenter => "Top centre",
        Position::TopRight => "Top right",
        Position::BottomLeft => "Bottom left",
        Position::BottomCenter => "Bottom centre",
        Position::BottomRight => "Bottom right",
        Position::Tiled => "Tiled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_a_faint_diagonal_confidential() {
        let dialog = WatermarkDialog::default();
        assert_eq!(dialog.text, "CONFIDENTIAL");
        assert!((dialog.rotation - 45.0).abs() < f32::EPSILON);
        assert!(dialog.opacity < 0.3);
        assert!(dialog.all_pages);
    }

    #[test]
    fn empty_text_is_refused_with_something_a_person_can_act_on() {
        let dialog = WatermarkDialog {
            text: "   ".into(),
            ..WatermarkDialog::default()
        };
        let error = dialog.watermark().expect_err("refused");
        assert!(error.contains("Type something"), "{error}");
    }

    #[test]
    fn a_bad_colour_says_what_a_colour_looks_like() {
        let dialog = WatermarkDialog {
            colour: "purple-ish".into(),
            ..WatermarkDialog::default()
        };
        let error = dialog.watermark().expect_err("refused");
        assert!(error.contains("#808080"), "{error}");
    }

    #[test]
    fn choosing_an_image_takes_precedence_over_the_text() {
        let dialog = WatermarkDialog {
            image: Some((std::path::PathBuf::from("logo.png"), vec![1, 2, 3])),
            ..WatermarkDialog::default()
        };
        let watermark = dialog.watermark().expect("builds");
        assert!(matches!(watermark.content, Content::Image { .. }));
    }

    #[test]
    fn the_placement_switch_reaches_the_engine() {
        let dialog = WatermarkDialog {
            under: true,
            ..WatermarkDialog::default()
        };
        assert_eq!(
            dialog.watermark().expect("builds").placement,
            Placement::Under
        );
    }
}
