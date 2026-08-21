//! Redaction in the viewer (spec §11).
//!
//! Marking is separate from applying, and deliberately so. Areas are drawn on
//! the page, listed, and can be removed again; nothing is taken out of the
//! document until **Apply** is pressed. Up to that moment the marks are just
//! marks — which is the only safe way to build a feature that cannot be undone
//! afterwards.
//!
//! The marks are drawn in a colour that could not be mistaken for the finished
//! result, so nobody looks at a marked page and believes it is redacted.

use egui::{Color32, Context, Ui};
use ypdf_redact::{Rect as PdfRect, Redaction, Settings};

/// The colour marks are drawn in before they are applied.
///
/// Translucent red: unmistakably "pending", never "done".
pub const MARK_COLOUR: Color32 = Color32::from_rgba_premultiplied(180, 30, 30, 90);

/// One marked area, in unrotated PDF points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    /// Zero-based page index.
    pub page: i32,
    /// The rectangle.
    pub rect: PdfRect,
}

/// The redaction panel and the marks it has collected.
#[derive(Debug, Default)]
pub struct RedactState {
    /// Is the panel showing?
    pub open: bool,
    /// Is the pointer marking areas rather than selecting text?
    pub marking: bool,
    /// Areas marked so far.
    pub areas: Vec<Area>,
    /// The area being dragged out, if any.
    pub dragging: Option<Area>,
    /// Where the current drag started, in PDF points.
    pub drag_origin: Option<(f32, f32)>,
    /// Text to find and mark.
    pub find: String,
    /// Draw a box over each cleared area.
    pub cover: bool,
    /// The redacted document, waiting to be saved.
    pub result: Option<Vec<u8>>,
    /// What the last run removed.
    pub report: Option<ypdf_redact::Report>,
    /// Why the last attempt failed.
    pub error: Option<String>,
}

/// What the panel is asking for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Mark every occurrence of the search text.
    Find,
    /// Remove everything marked.
    Apply,
    /// Forget the marks.
    Clear,
    /// Keep the result and write it somewhere.
    SaveAs,
    /// Throw the result away.
    Discard,
}

impl RedactState {
    /// Start with the cover box on, which is what most people expect to see.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cover: true,
            ..Self::default()
        }
    }

    /// The marks, as the engine wants them.
    #[must_use]
    pub fn redactions(&self) -> Vec<Redaction> {
        self.areas
            .iter()
            .filter_map(|area| {
                Some(Redaction {
                    page: u32::try_from(area.page).ok()? + 1,
                    rect: area.rect,
                })
            })
            .collect()
    }

    /// The settings, as the engine wants them.
    #[must_use]
    pub fn settings(&self) -> Settings {
        Settings {
            draw_cover: self.cover,
            ..Settings::default()
        }
    }

    /// Record a drag on `page`, from one point to another, in PDF points.
    pub fn drag(&mut self, page: i32, from: (f32, f32), to: (f32, f32)) {
        self.dragging = Some(Area {
            page,
            rect: PdfRect::new(from.0, from.1, to.0, to.1),
        });
    }

    /// Finish the drag, keeping it if it covers anything at all.
    pub fn finish_drag(&mut self) {
        self.drag_origin = None;
        let Some(area) = self.dragging.take() else {
            return;
        };
        // A click that did not move is not an area. Marking a zero-width strip
        // would look like a mark and remove nothing.
        if area.rect.width() < 2.0 || area.rect.height() < 2.0 {
            return;
        }
        self.areas.push(area);
    }

    /// Draw the panel.
    pub fn show(&mut self, ctx: &Context) -> Option<Action> {
        if !self.open {
            self.marking = false;
            return None;
        }

        let mut action = None;
        let mut open = self.open;

        egui::Window::new("Redact")
            .open(&mut open)
            .resizable(false)
            .default_width(380.0)
            .show(ctx, |ui| {
                action = self.body(ui);
            });

        self.open = open;
        if !self.open {
            self.marking = false;
        }
        action
    }

    fn body(&mut self, ui: &mut Ui) -> Option<Action> {
        let mut action = None;

        if let Some(report) = self.report {
            self.results(ui, report, &mut action);
            return action;
        }

        ui.checkbox(&mut self.marking, "Mark areas by dragging on the page")
            .on_hover_text("While this is on, dragging marks instead of selecting text");

        ui.horizontal(|ui| {
            ui.label("Find");
            ui.add(
                egui::TextEdit::singleline(&mut self.find)
                    .desired_width(160.0)
                    .hint_text("text to remove"),
            );
            if ui.button("Mark all").clicked() {
                action = Some(Action::Find);
            }
        });

        ui.separator();
        ui.label(format!("{} area(s) marked", self.areas.len()));
        ui.checkbox(&mut self.cover, "Draw a box over each area")
            .on_hover_text("The box is not the redaction; the content is removed either way");

        if let Some(error) = &self.error {
            ui.add_space(4.0);
            ui.colored_label(ui.visuals().error_fg_color, error);
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let ready = !self.areas.is_empty();
            if ui
                .add_enabled(ready, egui::Button::new("Apply"))
                .on_hover_text("Removes the content. This cannot be undone")
                .clicked()
            {
                action = Some(Action::Apply);
            }
            if ui
                .add_enabled(ready, egui::Button::new("Clear marks"))
                .clicked()
            {
                action = Some(Action::Clear);
            }
        });

        ui.add_space(4.0);
        // Said before the button is pressed, not after.
        ui.weak(
            "Applying deletes the text and pixels from the document. The original file is \
             not touched, and the result is not saved until you say so.",
        );

        action
    }

    fn results(&self, ui: &mut Ui, report: ypdf_redact::Report, action: &mut Option<Action>) {
        ui.label(format!(
            "{} glyph(s) removed from {} page(s).",
            report.glyphs, report.pages
        ));
        if report.images_redacted > 0 {
            ui.label(format!("{} image(s) cleared.", report.images_redacted));
        }
        if report.annotations > 0 {
            ui.label(format!("{} annotation(s) removed.", report.annotations));
        }
        if report.coarse_runs > 0 {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "{} text run(s) used a font this build cannot measure, so more text came \
                     out than was marked.",
                    report.coarse_runs
                ),
            );
        }
        if report.images_removed > 0 {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "{} image(s) could not be edited, so they were removed entirely.",
                    report.images_removed
                ),
            );
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Save as…").clicked() {
                *action = Some(Action::SaveAs);
            }
            if ui.button("Discard").clicked() {
                *action = Some(Action::Discard);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_panel_has_nothing_marked_and_is_not_marking() {
        let state = RedactState::new();
        assert!(state.areas.is_empty());
        assert!(!state.marking);
        assert!(state.cover, "the box is on by default");
    }

    #[test]
    fn a_drag_becomes_an_area_whichever_way_it_was_dragged() {
        let mut state = RedactState::new();
        state.drag(0, (200.0, 700.0), (100.0, 650.0));
        state.finish_drag();

        assert_eq!(state.areas.len(), 1);
        assert_eq!(
            state.areas[0].rect,
            PdfRect::new(100.0, 650.0, 200.0, 700.0)
        );
    }

    #[test]
    fn a_click_that_did_not_move_marks_nothing() {
        // Otherwise the page fills with marks that remove nothing, and one of
        // them looks exactly like a real one.
        let mut state = RedactState::new();
        state.drag(0, (100.0, 100.0), (100.5, 100.5));
        state.finish_drag();
        assert!(state.areas.is_empty());
    }

    #[test]
    fn marks_become_one_based_page_numbers_for_the_engine() {
        let mut state = RedactState::new();
        state.drag(2, (10.0, 10.0), (100.0, 100.0));
        state.finish_drag();

        let redactions = state.redactions();
        assert_eq!(redactions.len(), 1);
        assert_eq!(redactions[0].page, 3, "page 2 in the viewer is page 3 here");
    }

    #[test]
    fn the_cover_switch_reaches_the_engine() {
        let state = RedactState {
            cover: false,
            ..RedactState::new()
        };
        assert!(!state.settings().draw_cover);
    }

    #[test]
    fn closing_the_panel_stops_marking() {
        // Leaving marking on with the panel closed would turn every drag on the
        // page into a redaction mark with nothing on screen to explain it.
        let mut state = RedactState::new();
        state.marking = true;
        state.open = false;

        let ctx = Context::default();
        let _ = ctx.run_ui(Default::default(), |ctx| {
            state.show(ctx);
        });
        assert!(!state.marking);
    }
}
