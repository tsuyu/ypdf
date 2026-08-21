//! Annotation tools in the viewer (spec §10).
//!
//! Three ways to make a mark, chosen because they are the three people actually
//! use: **highlight** what is selected, drop a **note** where they click, and
//! drag a **shape**. Everything else the engine can draw is reachable from the
//! command line, where naming coordinates is reasonable; on a page it is not.
//!
//! Marks are applied as they are made rather than collected and applied at the
//! end. An annotation is one object in a list, so undoing one is deleting it,
//! and the panel does exactly that.

use egui::{Context, Ui};
use ypdf_annot::{Annotation, Colour, Shape};

/// Which tool the pointer is holding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    /// The pointer selects text and follows links, as usual.
    #[default]
    None,
    /// Dragging marks the text under it.
    Highlight,
    /// Dragging underlines the text under it.
    Underline,
    /// Dragging strikes through the text under it.
    StrikeOut,
    /// Clicking drops a sticky note.
    Note,
    /// Dragging draws a rectangle.
    Rectangle,
    /// Dragging draws an ellipse.
    Ellipse,
    /// Dragging draws an arrow.
    Arrow,
    /// Dragging draws freehand.
    Ink,
}

impl Tool {
    /// Does this tool mark text rather than draw on the page?
    #[must_use]
    pub const fn marks_text(self) -> bool {
        matches!(self, Self::Highlight | Self::Underline | Self::StrikeOut)
    }

    /// The word on the button.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "Off",
            Self::Highlight => "Highlight",
            Self::Underline => "Underline",
            Self::StrikeOut => "Strike",
            Self::Note => "Note",
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Arrow => "Arrow",
            Self::Ink => "Draw",
        }
    }
}

/// State of the annotation panel and the tool in hand.
#[derive(Debug)]
pub struct AnnotatePanel {
    /// Is the panel showing?
    pub open: bool,
    /// The tool the pointer is holding.
    pub tool: Tool,
    /// Colour for new marks.
    pub colour: Colour,
    /// Opacity for new marks.
    pub opacity: f32,
    /// Stroke width for new marks.
    pub width: f32,
    /// The comment attached to the next mark.
    pub comment: String,
    /// Who is making them.
    pub author: String,
    /// What is on the document now.
    pub items: Vec<Annotation>,
    /// True once they have been read.
    pub loaded: bool,
    /// A drag in progress, in PDF points.
    pub drag: Option<Drag>,
    /// Why the last attempt failed.
    pub error: Option<String>,
}

/// A drag being made on a page.
#[derive(Clone, Debug)]
pub struct Drag {
    /// Zero-based page.
    pub page: i32,
    /// Where it started.
    pub from: (f32, f32),
    /// Where the pointer is now.
    pub to: (f32, f32),
    /// Every point, for freehand.
    pub points: Vec<[f32; 2]>,
}

impl Default for AnnotatePanel {
    fn default() -> Self {
        Self {
            open: false,
            tool: Tool::None,
            colour: Colour::default(),
            opacity: 1.0,
            width: 1.5,
            comment: String::new(),
            author: String::new(),
            items: Vec::new(),
            loaded: false,
            drag: None,
            error: None,
        }
    }
}

/// What the panel is asking the application to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Remove the one at this position in the list.
    Remove(usize),
    /// Jump to the page an annotation is on.
    Go(u32),
    /// Remove every annotation.
    Clear,
}

impl AnnotatePanel {
    /// Take the annotations read from the document.
    pub fn load(&mut self, items: Vec<Annotation>) {
        self.items = items;
        self.loaded = true;
    }

    /// Forget them, so they are read again.
    pub fn invalidate(&mut self) {
        self.loaded = false;
        self.drag = None;
    }

    /// Start or continue a drag.
    pub fn drag_to(&mut self, page: i32, from: (f32, f32), to: (f32, f32)) {
        match &mut self.drag {
            Some(drag) if drag.page == page => {
                drag.to = to;
                drag.points.push([to.0, to.1]);
            }
            _ => {
                self.drag = Some(Drag {
                    page,
                    from,
                    to,
                    points: vec![[from.0, from.1], [to.0, to.1]],
                });
            }
        }
    }

    /// Finish a drag, returning the annotation it made.
    ///
    /// A drag that covered nothing makes nothing: a stray click while a tool is
    /// in hand should not leave an invisible mark on the page.
    pub fn finish_drag(&mut self) -> Option<Annotation> {
        let drag = self.drag.take()?;
        let page = u32::try_from(drag.page).ok()? + 1;

        let rect = [
            drag.from.0.min(drag.to.0),
            drag.from.1.min(drag.to.1),
            drag.from.0.max(drag.to.0),
            drag.from.1.max(drag.to.1),
        ];
        let tiny = (rect[2] - rect[0]) < 2.0 && (rect[3] - rect[1]) < 2.0;

        let shape = match self.tool {
            Tool::Rectangle if !tiny => Shape::Rectangle { rect, fill: None },
            Tool::Ellipse if !tiny => Shape::Ellipse { rect, fill: None },
            Tool::Arrow if !tiny => Shape::Line {
                from: [drag.from.0, drag.from.1],
                to: [drag.to.0, drag.to.1],
                arrow: true,
            },
            Tool::Ink if drag.points.len() > 2 => Shape::Ink {
                strokes: vec![drag.points],
            },
            _ => return None,
        };

        Some(self.dress(Annotation::new(page, shape)))
    }

    /// Mark the given text rectangles with the tool in hand.
    pub fn mark_text(&self, page: u32, quads: Vec<[f32; 4]>) -> Option<Annotation> {
        if quads.is_empty() {
            return None;
        }
        let shape = match self.tool {
            Tool::Highlight => Shape::Highlight { quads },
            Tool::Underline => Shape::Underline { quads },
            Tool::StrikeOut => Shape::StrikeOut { quads },
            _ => return None,
        };
        Some(self.dress(Annotation::new(page, shape)))
    }

    /// A note at a point.
    pub fn note_at(&self, page: u32, at: (f32, f32)) -> Annotation {
        self.dress(Annotation::new(page, Shape::Note { at: [at.0, at.1] }))
    }

    /// Apply the panel's colour, opacity, width, comment, and author.
    fn dress(&self, annotation: Annotation) -> Annotation {
        let mut annotation = annotation
            .with_colour(self.colour)
            .with_opacity(self.opacity)
            .with_width(self.width);
        if !self.comment.trim().is_empty() {
            annotation = annotation.with_contents(self.comment.trim());
        }
        if !self.author.trim().is_empty() {
            annotation = annotation.with_author(self.author.trim());
        }
        annotation
    }

    /// Draw the panel.
    pub fn show(&mut self, ctx: &Context) -> Option<Action> {
        if !self.open {
            // A tool left in hand with the panel closed turns every drag into a
            // mark with nothing on screen to explain it.
            self.tool = Tool::None;
            return None;
        }

        let mut action = None;
        let mut open = self.open;

        egui::Window::new("Annotate")
            .open(&mut open)
            .default_width(380.0)
            .show(ctx, |ui| {
                action = self.body(ui);
            });

        self.open = open;
        if !self.open {
            self.tool = Tool::None;
        }
        action
    }

    fn body(&mut self, ui: &mut Ui) -> Option<Action> {
        let mut action = None;

        ui.horizontal_wrapped(|ui| {
            for tool in [
                Tool::None,
                Tool::Highlight,
                Tool::Underline,
                Tool::StrikeOut,
                Tool::Note,
                Tool::Rectangle,
                Tool::Ellipse,
                Tool::Arrow,
                Tool::Ink,
            ] {
                ui.selectable_value(&mut self.tool, tool, tool.label());
            }
        });

        if self.tool.marks_text() {
            ui.weak("Select text on the page, and it is marked as you release.");
        } else if self.tool == Tool::Note {
            ui.weak("Click on the page to leave a note there.");
        } else if self.tool != Tool::None {
            ui.weak("Drag on the page to draw.");
        }

        ui.separator();
        self.settings(ui);
        ui.separator();

        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
            ui.add_space(4.0);
        }

        if !self.loaded {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading…");
            });
            return action;
        }

        if self.items.is_empty() {
            ui.weak("Nothing on this document yet.");
            return action;
        }

        egui::ScrollArea::vertical()
            .max_height(240.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let rows: Vec<(usize, u32, String, String, String)> = self
                    .items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        (
                            index,
                            item.page,
                            item.shape.label().to_string(),
                            item.author.clone(),
                            item.contents.clone(),
                        )
                    })
                    .collect();

                for (index, page, kind, author, contents) in rows {
                    ui.horizontal(|ui| {
                        if ui
                            .add(
                                egui::Label::new(format!("p{page}  {kind}"))
                                    .sense(egui::Sense::click()),
                            )
                            .on_hover_text("Click to go there")
                            .clicked()
                        {
                            action = Some(Action::Go(page));
                        }
                        if !author.is_empty() {
                            ui.weak(author);
                        }
                        if !contents.is_empty() {
                            ui.weak(contents.chars().take(30).collect::<String>());
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("✕").on_hover_text("Remove").clicked() {
                                action = Some(Action::Remove(index));
                            }
                        });
                    });
                }
            });

        ui.add_space(6.0);
        if ui.button("Remove all").clicked() {
            action = Some(Action::Clear);
        }

        action
    }

    fn settings(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Colour");
            for (label, colour) in [
                ("Yellow", Colour(1.0, 0.92, 0.23)),
                ("Green", Colour(0.3, 0.85, 0.4)),
                ("Blue", Colour(0.25, 0.55, 0.95)),
                ("Red", Colour(0.9, 0.2, 0.2)),
            ] {
                let selected = (self.colour.0 - colour.0).abs() < 0.01
                    && (self.colour.1 - colour.1).abs() < 0.01
                    && (self.colour.2 - colour.2).abs() < 0.01;
                if ui.selectable_label(selected, label).clicked() {
                    self.colour = colour;
                }
            }
        });

        egui::Grid::new("annotate-settings")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Opacity");
                ui.add(egui::Slider::new(&mut self.opacity, 0.1..=1.0));
                ui.end_row();

                ui.label("Width");
                ui.add(egui::Slider::new(&mut self.width, 0.5..=8.0).suffix(" pt"));
                ui.end_row();

                ui.label("Comment");
                ui.add(
                    egui::TextEdit::singleline(&mut self.comment)
                        .desired_width(200.0)
                        .hint_text("attached to the next mark"),
                );
                ui.end_row();

                ui.label("Author");
                ui.add(
                    egui::TextEdit::singleline(&mut self.author)
                        .desired_width(200.0)
                        .hint_text("your name"),
                );
                ui.end_row();
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(tool: Tool) -> AnnotatePanel {
        AnnotatePanel {
            tool,
            author: "Ada".into(),
            ..AnnotatePanel::default()
        }
    }

    #[test]
    fn a_drag_becomes_the_shape_the_tool_names() {
        let mut panel = panel(Tool::Rectangle);
        panel.drag_to(0, (100.0, 700.0), (300.0, 600.0));
        let annotation = panel.finish_drag().expect("a rectangle");

        assert!(matches!(annotation.shape, Shape::Rectangle { .. }));
        assert_eq!(annotation.page, 1, "pages are 1-based for the engine");
        assert_eq!(annotation.author, "Ada");
    }

    #[test]
    fn a_stray_click_with_a_tool_in_hand_marks_nothing() {
        // Otherwise the page collects invisible annotations nobody meant.
        let mut panel = panel(Tool::Rectangle);
        panel.drag_to(0, (100.0, 700.0), (100.4, 700.4));
        assert!(panel.finish_drag().is_none());
    }

    #[test]
    fn freehand_keeps_every_point_it_passed_through() {
        let mut panel = panel(Tool::Ink);
        panel.drag_to(0, (10.0, 10.0), (20.0, 20.0));
        panel.drag_to(0, (10.0, 10.0), (30.0, 15.0));
        panel.drag_to(0, (10.0, 10.0), (40.0, 25.0));

        let annotation = panel.finish_drag().expect("a drawing");
        let Shape::Ink { strokes } = annotation.shape else {
            panic!("not a drawing");
        };
        assert_eq!(strokes.len(), 1);
        assert!(strokes[0].len() >= 4, "{strokes:?}");
    }

    #[test]
    fn a_drag_that_starts_on_another_page_starts_a_new_one() {
        let mut panel = panel(Tool::Rectangle);
        panel.drag_to(0, (10.0, 10.0), (50.0, 50.0));
        panel.drag_to(1, (60.0, 60.0), (90.0, 90.0));

        let annotation = panel.finish_drag().expect("a rectangle");
        assert_eq!(annotation.page, 2);
    }

    #[test]
    fn marking_text_uses_the_tool_in_hand() {
        let quads = vec![[72.0, 700.0, 300.0, 714.0]];

        let highlight = panel(Tool::Highlight)
            .mark_text(1, quads.clone())
            .expect("a highlight");
        assert!(matches!(highlight.shape, Shape::Highlight { .. }));

        let strike = panel(Tool::StrikeOut)
            .mark_text(1, quads.clone())
            .expect("a strike-through");
        assert!(matches!(strike.shape, Shape::StrikeOut { .. }));

        // A tool that does not mark text produces nothing from a selection.
        assert!(panel(Tool::Rectangle).mark_text(1, quads).is_none());
    }

    #[test]
    fn a_selection_of_nothing_marks_nothing() {
        assert!(panel(Tool::Highlight).mark_text(1, Vec::new()).is_none());
    }

    #[test]
    fn the_panels_settings_reach_the_annotation() {
        let mut panel = panel(Tool::Note);
        panel.colour = Colour(0.9, 0.2, 0.2);
        panel.opacity = 0.5;
        panel.comment = "  look here  ".into();

        let annotation = panel.note_at(3, (100.0, 700.0));
        assert_eq!(annotation.colour.to_hex(), "#e63333");
        assert!((annotation.opacity - 0.5).abs() < f32::EPSILON);
        assert_eq!(annotation.contents, "look here", "trimmed");
        assert_eq!(annotation.page, 3);
    }

    #[test]
    fn closing_the_panel_puts_the_tool_down() {
        let mut panel = panel(Tool::Highlight);
        panel.open = false;

        let ctx = Context::default();
        let _ = ctx.run_ui(Default::default(), |ctx| {
            panel.show(ctx);
        });
        assert_eq!(panel.tool, Tool::None);
    }
}
