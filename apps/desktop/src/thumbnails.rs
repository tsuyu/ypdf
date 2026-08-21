//! Thumbnail sidebar: page navigation, selection, and drag-to-reorder.
//!
//! Thumbnails share the page cache with the main canvas — they are just rasters
//! at a small width — and are always requested at the lowest priority, so a
//! sidebar scroll can never delay the page being read.

use egui::{Color32, CornerRadius, Rect, Sense, Stroke, Ui, pos2, vec2};
use ypdf_render::{PageIndex, Priority, RenderHandle, RenderRequest};

use crate::document::OpenDocument;
use crate::textures::PageKey;

/// Thumbnail width in screen points.
const THUMB_WIDTH: f32 = 132.0;

/// Rendered pixel width for thumbnails. Fixed, so zooming the document never
/// invalidates the sidebar.
const THUMB_RENDER_WIDTH: u32 = 192;

/// A page the user dragged somewhere else. Both 1-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageMove {
    /// Page being moved.
    pub from: u32,
    /// Position it was dropped at.
    pub to: u32,
}

/// Draw the sidebar, queue any thumbnails it needs, and report a reorder.
pub fn show(ui: &mut Ui, doc: &mut OpenDocument, render: &RenderHandle) -> Option<PageMove> {
    let page_count = doc.page_count();
    if page_count == 0 {
        ui.label("No pages.");
        return None;
    }

    let rotation = doc.rotation.normalized();
    let mut go_to = None;
    let mut clicked = None;
    let mut drop_on = None;
    let (ctrl, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for page in 0..page_count {
                let (w_pt, h_pt) = doc.page_size_pt(page);
                let height = if w_pt > 0.0 {
                    THUMB_WIDTH * (h_pt / w_pt)
                } else {
                    THUMB_WIDTH
                };

                ui.vertical_centered(|ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(THUMB_WIDTH, height), Sense::click_and_drag());

                    if ui.is_rect_visible(rect) {
                        draw_thumbnail(ui, doc, page, rect, rotation);
                        queue(doc, render, page, rotation);
                    }

                    if response.drag_started() {
                        doc.dragging_page = Some(page);
                    }
                    // The drop target is whatever the pointer is over: egui reports
                    // a drag to the widget it started on, not the one under it.
                    if doc.dragging_page.is_some() && response.contains_pointer() {
                        drop_on = Some(page);
                        ui.painter().rect_stroke(
                            rect,
                            CornerRadius::ZERO,
                            Stroke::new(2.0, ui.visuals().selection.bg_fill),
                            egui::StrokeKind::Outside,
                        );
                    }
                    if response.clicked() {
                        clicked = Some(page);
                        go_to = Some(page);
                    }

                    let number = egui::RichText::new(format!("{}", page + 1)).small();
                    if doc.selected_pages.contains(&page) {
                        ui.label(number.strong());
                    } else {
                        ui.label(number.weak());
                    }
                    ui.add_space(4.0);
                });
            }
        });

    if let Some(page) = clicked {
        select(doc, page, ctrl, shift);
    }
    if let Some(page) = go_to {
        doc.go_to(page);
    }

    // A drag ends wherever the pointer was released, which may be nowhere.
    if ui.input(|i| i.pointer.any_released()) {
        return match (doc.dragging_page.take(), drop_on) {
            (Some(from), Some(to)) if from != to => Some(PageMove {
                from: u32::try_from(from + 1).unwrap_or(1),
                to: u32::try_from(to + 1).unwrap_or(1),
            }),
            _ => None,
        };
    }

    None
}

/// Selection follows the conventions of every file list: a plain click
/// replaces, `Ctrl` toggles, `Shift` extends from the last click.
fn select(doc: &mut OpenDocument, page: PageIndex, ctrl: bool, shift: bool) {
    if shift {
        let anchor = doc.selection_anchor.unwrap_or(page);
        let (lo, hi) = if anchor <= page {
            (anchor, page)
        } else {
            (page, anchor)
        };
        doc.selected_pages = (lo..=hi).collect();
        return;
    }

    if ctrl {
        if !doc.selected_pages.remove(&page) {
            doc.selected_pages.insert(page);
        }
    } else if doc.selected_pages.len() == 1 && doc.selected_pages.contains(&page) {
        // Clicking the only selected page again clears it, so the edit
        // operations fall back to "the page I am reading".
        doc.selected_pages.clear();
    } else {
        doc.selected_pages.clear();
        doc.selected_pages.insert(page);
    }
    doc.selection_anchor = Some(page);
}

fn draw_thumbnail(ui: &Ui, doc: &mut OpenDocument, page: PageIndex, rect: Rect, rotation: u8) {
    let selected = doc.selected_pages.contains(&page);
    let current = page == doc.current_page;
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::WHITE);

    let key = PageKey {
        page,
        width: THUMB_RENDER_WIDTH,
        rotation,
        invert: doc.invert,
    };
    if let Some(texture) = doc.cache.get(key) {
        painter.image(
            texture.id(),
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    if selected {
        painter.rect_filled(
            rect,
            CornerRadius::ZERO,
            ui.visuals().selection.bg_fill.gamma_multiply(0.35),
        );
    }

    // The border carries both states: selection is thicker, and the page being
    // read stays marked even when it is not selected.
    let (width, colour) = match (selected, current) {
        (true, _) => (3.0, ui.visuals().selection.stroke.color),
        (false, true) => (2.0, ui.visuals().selection.bg_fill),
        (false, false) => (1.0, Color32::from_gray(90)),
    };
    painter.rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(width, colour),
        egui::StrokeKind::Outside,
    );
}

fn queue(doc: &OpenDocument, render: &RenderHandle, page: PageIndex, rotation: u8) {
    let key = PageKey {
        page,
        width: THUMB_RENDER_WIDTH,
        rotation,
        invert: doc.invert,
    };
    if doc.cache.contains(key) {
        return;
    }
    render.request(RenderRequest {
        doc: doc.id,
        page,
        target_width: THUMB_RENDER_WIDTH,
        rotation: doc.rotation,
        priority: Priority::Thumbnail,
        generation: doc.generation,
    });
}

/// Width the sidebar wants, including padding.
#[must_use]
pub const fn preferred_width() -> f32 {
    THUMB_WIDTH + 32.0
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ypdf_render::{DocumentId, DocumentInfo};

    use super::*;

    fn document() -> OpenDocument {
        let mut doc = OpenDocument::opening(DocumentId::new(), PathBuf::from("t.pdf"), 16);
        doc.info = Some(DocumentInfo {
            path: PathBuf::from("t.pdf"),
            page_count: 10,
            page_sizes_pt: vec![(612.0, 792.0); 10],
        });
        doc
    }

    fn selected(doc: &OpenDocument) -> Vec<PageIndex> {
        doc.selected_pages.iter().copied().collect()
    }

    #[test]
    fn a_plain_click_replaces_the_selection() {
        let mut doc = document();
        select(&mut doc, 3, false, false);
        select(&mut doc, 5, false, false);
        assert_eq!(selected(&doc), vec![5]);
    }

    #[test]
    fn ctrl_click_toggles_pages() {
        let mut doc = document();
        select(&mut doc, 1, true, false);
        select(&mut doc, 4, true, false);
        assert_eq!(selected(&doc), vec![1, 4]);

        select(&mut doc, 1, true, false);
        assert_eq!(selected(&doc), vec![4]);
    }

    #[test]
    fn shift_click_extends_from_the_last_click_in_either_direction() {
        let mut doc = document();
        select(&mut doc, 2, false, false);
        select(&mut doc, 5, false, true);
        assert_eq!(selected(&doc), vec![2, 3, 4, 5]);

        select(&mut doc, 7, false, false);
        select(&mut doc, 4, false, true);
        assert_eq!(selected(&doc), vec![4, 5, 6, 7]);
    }

    #[test]
    fn clicking_the_only_selected_page_clears_the_selection() {
        // So the edit operations fall back to the page being read.
        let mut doc = document();
        select(&mut doc, 3, false, false);
        select(&mut doc, 3, false, false);
        assert!(doc.selected_pages.is_empty());
    }

    #[test]
    fn target_pages_falls_back_to_the_current_page() {
        let mut doc = document();
        doc.current_page = 6;
        assert_eq!(doc.target_pages(), vec![7], "page numbers are 1-based");

        select(&mut doc, 0, false, false);
        select(&mut doc, 2, false, true);
        assert_eq!(doc.target_pages(), vec![1, 2, 3]);
    }
}
