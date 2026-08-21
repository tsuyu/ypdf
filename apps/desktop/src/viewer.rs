//! The page canvas: layout, drawing, and the interactive text layer.
//!
//! Every page gets its box in the scroll area whether or not its raster exists,
//! so the scrollbar is honest from the moment the document opens and never
//! jumps as pages arrive.
//!
//! Text, links, and selection all live in *unrotated PDF point space* — that is
//! what PDFium reports — and are mapped to the screen at draw time by
//! [`pt_to_screen`]. Keeping that mapping in one place is what lets rotation and
//! zoom work without any of the extraction code knowing about either.

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use ypdf_render::{LinkTarget, PageIndex, Priority, RectPt, RenderHandle, RenderRequest};

use crate::document::{OpenDocument, PAGE_MARGIN, Selection};
use crate::textures::PageKey;

/// Pages beyond the viewport to render at [`Priority::Nearby`].
const PREFETCH_RADIUS: PageIndex = 2;

/// Turn a drag on the page into an annotation.
fn mark_annotation(doc: &mut OpenDocument, page: PageIndex, rect: Rect, response: &egui::Response) {
    use crate::annotate::Tool;

    let (w, h) = doc.page_size_unrotated(page);
    let rotation = doc.rotation.normalized();

    if doc.annotate.tool == Tool::Note {
        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let point = screen_to_pt(pos, w, h, rotation, rect);
            if let Ok(number) = u32::try_from(page) {
                doc.pending_annotation = Some(doc.annotate.note_at(number + 1, point));
            }
        }
        return;
    }

    if response.drag_started()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let point = screen_to_pt(pos, w, h, rotation, rect);
        doc.annotate.drag_to(page, point, point);
    } else if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(from) = doc.annotate.drag.as_ref().map(|drag| drag.from)
    {
        let point = screen_to_pt(pos, w, h, rotation, rect);
        doc.annotate.drag_to(page, from, point);
    }

    if response.drag_stopped() {
        doc.pending_annotation = doc.annotate.finish_drag();
    }
}

/// Draw the shape being dragged out, before it becomes an annotation.
fn draw_pending_mark(ui: &Ui, doc: &OpenDocument, page: PageIndex, rect: Rect) {
    let Some(drag) = doc.annotate.drag.as_ref() else {
        return;
    };
    if drag.page != page {
        return;
    }

    let (w, h) = doc.page_size_unrotated(page);
    let rotation = doc.rotation.normalized();
    let painter = ui.painter_at(rect);

    let colour = doc.annotate.colour;
    let stroke = egui::Stroke::new(
        doc.annotate.width.max(1.0),
        egui::Color32::from_rgb(
            (colour.0 * 255.0) as u8,
            (colour.1 * 255.0) as u8,
            (colour.2 * 255.0) as u8,
        ),
    );

    let screen = pt_to_screen(
        RectPt {
            left: drag.from.0.min(drag.to.0),
            bottom: drag.from.1.min(drag.to.1),
            right: drag.from.0.max(drag.to.0),
            top: drag.from.1.max(drag.to.1),
        },
        w,
        h,
        rotation,
        rect,
    );

    match doc.annotate.tool {
        crate::annotate::Tool::Ellipse => {
            painter.circle_stroke(
                screen.center(),
                screen.width().max(screen.height()) / 2.0,
                stroke,
            );
        }
        crate::annotate::Tool::Ink => {
            // The path as drawn so far, so freehand follows the pointer.
            let points: Vec<egui::Pos2> = drag
                .points
                .iter()
                .map(|point| {
                    pt_to_screen(
                        RectPt {
                            left: point[0],
                            bottom: point[1],
                            right: point[0],
                            top: point[1],
                        },
                        w,
                        h,
                        rotation,
                        rect,
                    )
                    .center()
                })
                .collect();
            let _ = painter.add(egui::Shape::line(points, stroke));
        }
        crate::annotate::Tool::Arrow => {
            let from = pt_to_screen(
                RectPt {
                    left: drag.from.0,
                    bottom: drag.from.1,
                    right: drag.from.0,
                    top: drag.from.1,
                },
                w,
                h,
                rotation,
                rect,
            )
            .center();
            let to = pt_to_screen(
                RectPt {
                    left: drag.to.0,
                    bottom: drag.to.1,
                    right: drag.to.0,
                    top: drag.to.1,
                },
                w,
                h,
                rotation,
                rect,
            )
            .center();
            painter.line_segment([from, to], stroke);
        }
        _ => {
            painter.rect_stroke(screen, 0.0, stroke, egui::StrokeKind::Middle);
        }
    }
}

/// Turn a drag on the page into a redaction mark.
fn mark_redaction(doc: &mut OpenDocument, page: PageIndex, rect: Rect, response: &egui::Response) {
    let (w, h) = doc.page_size_unrotated(page);
    let rotation = doc.rotation.normalized();

    if response.drag_started()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let point = screen_to_pt(pos, w, h, rotation, rect);
        doc.redact.drag(page, point, point);
        doc.redact.drag_origin = Some(point);
    } else if response.dragged()
        && let Some(pos) = response.interact_pointer_pos()
        && let Some(origin) = doc.redact.drag_origin
    {
        let point = screen_to_pt(pos, w, h, rotation, rect);
        doc.redact.drag(page, origin, point);
    }

    if response.drag_stopped() {
        doc.redact.finish_drag();
        doc.redact.drag_origin = None;
    }
}

/// Draw the marked areas, and the one being dragged.
fn draw_redaction_marks(ui: &Ui, doc: &OpenDocument, page: PageIndex, rect: Rect) {
    let (w, h) = doc.page_size_unrotated(page);
    let rotation = doc.rotation.normalized();
    let painter = ui.painter_at(rect);

    let draw = |area: &crate::redact::Area| {
        if area.page != page {
            return;
        }
        let screen = pt_to_screen(
            RectPt {
                left: area.rect.x0,
                bottom: area.rect.y0,
                right: area.rect.x1,
                top: area.rect.y1,
            },
            w,
            h,
            rotation,
            rect,
        );
        painter.rect_filled(screen, 0.0, crate::redact::MARK_COLOUR);
    };

    for area in &doc.redact.areas {
        draw(area);
    }
    if let Some(area) = &doc.redact.dragging {
        draw(area);
    }
}

/// Draw the document, queue what still needs rendering, and handle interaction.
pub fn show(ui: &mut Ui, doc: &mut OpenDocument, render: &RenderHandle) {
    if let Some(error) = &doc.error {
        ui.vertical_centered(|ui| {
            ui.add_space(48.0);
            ui.colored_label(ui.visuals().error_fg_color, error);
        });
        return;
    }

    let Some(page_count) = doc.info.as_ref().map(|i| i.page_count) else {
        ui.vertical_centered(|ui| {
            ui.add_space(48.0);
            ui.spinner();
            ui.label("Opening…");
        });
        return;
    };

    let viewport = ui.available_size();
    doc.scale = doc.resolve_scale(viewport, doc.current_page);
    let pixels_per_point = ui.ctx().pixels_per_point();

    let mut scroll = egui::ScrollArea::both().auto_shrink([false, false]);
    if std::mem::take(&mut doc.scroll_to_current) {
        scroll = scroll.vertical_scroll_offset(offset_of_page(doc, doc.current_page));
    }

    let mut first_visible = None;
    let mut last_visible = None;
    let mut follow_link = None;

    scroll.show(ui, |ui| {
        ui.vertical_centered(|ui| {
            for page in 0..page_count {
                let (w_pt, h_pt) = doc.page_size_pt(page);
                let size = vec2(w_pt * doc.scale, h_pt * doc.scale);
                ui.add_space(PAGE_MARGIN);
                let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());

                if !ui.is_rect_visible(rect) {
                    continue;
                }
                if first_visible.is_none() {
                    first_visible = Some(page);
                }
                last_visible = Some(page);

                let target_width = target_width_px(size.x, pixels_per_point);
                draw_page(ui, doc, page, rect, target_width);
                draw_search_hits(ui, doc, page, rect);
                draw_redaction_marks(ui, doc, page, rect);
                draw_pending_mark(ui, doc, page, rect);
                draw_selection(ui, doc, page, rect);

                if doc.annotate.tool != crate::annotate::Tool::None
                    && !doc.annotate.tool.marks_text()
                {
                    // A drawing tool takes the drag; text tools leave it to the
                    // selection, and mark whatever it ends up covering.
                    mark_annotation(doc, page, rect, &response);
                } else if doc.redact.marking {
                    // While marking, a drag draws an area instead of selecting
                    // text: one pointer, two meanings, and the checkbox in the
                    // panel is what says which.
                    mark_redaction(doc, page, rect, &response);
                } else if let Some(target) = interact(ui, doc, page, rect, &response) {
                    follow_link = Some(target);
                }
            }
            ui.add_space(PAGE_MARGIN);
        });
    });

    if let Some(target) = follow_link {
        follow(ui, doc, target);
    }

    let Some(first) = first_visible else { return };
    let last = last_visible.unwrap_or(first);

    // The page indicator follows the top of the viewport, which is what a
    // reader means by "the page I am on".
    doc.current_page = first;

    queue_renders(doc, render, first, last, page_count, pixels_per_point);
    queue_analysis(doc, render, first, last, page_count);

    // Pages are still arriving: keep the frame loop alive without spinning.
    if !doc.cache.contains(PageKey {
        page: first,
        width: target_width_px(doc.page_size_pt(first).0 * doc.scale, pixels_per_point),
        rotation: doc.rotation.normalized(),
        invert: doc.invert,
    }) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    }
}

fn follow(ui: &Ui, doc: &mut OpenDocument, target: LinkTarget) {
    match target {
        LinkTarget::Url(url) => {
            // Handing a URL to the system browser is the one place this
            // application reaches outside itself, and only on an explicit click.
            tracing::info!(%url, "opening external link");
            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
        }
        LinkTarget::Page(page) => doc.go_to(page),
        // A launch action runs a program. yPDF reports it (spec §20); it does
        // not offer it as something to click.
        LinkTarget::Unsupported(kind) => tracing::warn!("refusing to follow a {kind} link"),
    }
}

/// Draw one page: the sharp raster if it exists, the nearest stand-in if not,
/// and a placeholder if there is nothing at all.
fn draw_page(ui: &Ui, doc: &mut OpenDocument, page: PageIndex, rect: Rect, target_width: u32) {
    let painter = ui.painter();
    let rotation = doc.rotation.normalized();
    let exact = PageKey {
        page,
        width: target_width,
        rotation,
        invert: doc.invert,
    };

    // Paper first, so a partially transparent or missing raster still reads as
    // a page rather than a hole in the panel.
    let paper = if doc.invert {
        Color32::BLACK
    } else {
        Color32::WHITE
    };
    painter.rect_filled(rect, CornerRadius::ZERO, paper);

    let key = if doc.cache.contains(exact) {
        Some(exact)
    } else {
        doc.cache.nearest(page, rotation, doc.invert, target_width)
    };

    if let Some(key) = key
        && let Some(texture) = doc.cache.get(key)
    {
        painter.image(
            texture.id(),
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{}", page + 1),
            egui::FontId::proportional(14.0),
            Color32::from_gray(160),
        );
    }

    painter.rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, Color32::from_gray(90)),
        egui::StrokeKind::Outside,
    );
}

/// Highlight search hits; the current one gets a stronger colour.
fn draw_search_hits(ui: &Ui, doc: &OpenDocument, page: PageIndex, rect: Rect) {
    if doc.search.query.is_empty() {
        return;
    }
    let (w, h) = doc.page_size_unrotated(page);
    let rotation = doc.rotation.normalized();
    let painter = ui.painter();

    for hit in doc.search.hits_on(page) {
        let colour = if doc.search.is_current(page, hit.start) {
            Color32::from_rgba_unmultiplied(255, 145, 0, 130)
        } else {
            Color32::from_rgba_unmultiplied(255, 235, 59, 90)
        };
        for r in &hit.rects {
            painter.rect_filled(
                pt_to_screen(*r, w, h, rotation, rect),
                CornerRadius::ZERO,
                colour,
            );
        }
    }
}

fn draw_selection(ui: &Ui, doc: &OpenDocument, page: PageIndex, rect: Rect) {
    let Some(selection) = doc.selection else {
        return;
    };
    if selection.page != page {
        return;
    }
    let Some(analysis) = doc.analyses.get(&page) else {
        return;
    };

    let (w, h) = doc.page_size_unrotated(page);
    let rotation = doc.rotation.normalized();
    let (start, len) = selection.range();
    let colour = ui.visuals().selection.bg_fill.gamma_multiply(0.45);

    for r in analysis.text.rects_for(start, len) {
        ui.painter().rect_filled(
            pt_to_screen(r, w, h, rotation, rect),
            CornerRadius::ZERO,
            colour,
        );
    }
}

/// Handle hover, clicks, and selection drags on one page.
///
/// Returns a link target when one was clicked.
fn interact(
    ui: &Ui,
    doc: &mut OpenDocument,
    page: PageIndex,
    rect: Rect,
    response: &egui::Response,
) -> Option<LinkTarget> {
    let analysis = doc.analyses.get(&page)?;
    let (w, h) = doc.page_size_unrotated(page);
    let rotation = doc.rotation.normalized();

    let pointer = response
        .hover_pos()
        .or_else(|| response.interact_pointer_pos());
    let page_point = pointer.map(|p| screen_to_pt(p, w, h, rotation, rect));

    // A link under the pointer wins over text selection: clicking a link is
    // what the user meant.
    let link = page_point.and_then(|(x, y)| {
        analysis
            .links
            .iter()
            .find(|l| l.rect.contains(x, y))
            .map(|l| l.target.clone())
    });

    if link.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        return if response.clicked() { link } else { None };
    }

    let has_text = !analysis.text.is_empty();
    if has_text && response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }

    let char_under_pointer = page_point.and_then(|(x, y)| {
        doc.analyses
            .get(&page)
            .and_then(|analysis| analysis.text.char_at(x, y))
    });

    if response.drag_started() {
        if let Some(index) = char_under_pointer {
            doc.selection = Some(Selection {
                page,
                anchor: index,
                head: index,
            });
            doc.selecting = true;
        }
    } else if doc.selecting
        && response.dragged()
        && let Some(index) = char_under_pointer
        && let Some(selection) = doc.selection.as_mut()
        && selection.page == page
    {
        selection.head = index;
    }

    if response.drag_stopped() {
        doc.selecting = false;
    }
    // A plain click with no drag clears the selection, as every text view does.
    if response.clicked() {
        doc.selection = None;
    }

    None
}

/// Ask for what is visible, then for what is about to be.
fn queue_renders(
    doc: &OpenDocument,
    render: &RenderHandle,
    first: PageIndex,
    last: PageIndex,
    page_count: PageIndex,
    pixels_per_point: f32,
) {
    let rotation = doc.rotation.normalized();

    let request = |page: PageIndex, priority: Priority| {
        if page < 0 || page >= page_count {
            return;
        }
        let (w_pt, _) = doc.page_size_pt(page);
        let target_width = target_width_px(w_pt * doc.scale, pixels_per_point);
        if doc.cache.contains(PageKey {
            page,
            width: target_width,
            rotation,
            invert: doc.invert,
        }) {
            return;
        }
        render.request(RenderRequest {
            doc: doc.id,
            page,
            target_width,
            rotation: doc.rotation,
            priority,
            generation: doc.generation,
        });
    };

    for page in first..=last {
        request(page, Priority::Visible);
    }
    for offset in 1..=PREFETCH_RADIUS {
        request(last + offset, Priority::Nearby);
        request(first - offset, Priority::Nearby);
    }
}

/// Ask for the text layer of the pages on screen.
///
/// Only visible pages: extracting a whole document up front would spend the
/// render thread on pages nobody has looked at. A search widens this to every
/// page, deliberately and only when asked.
fn queue_analysis(
    doc: &mut OpenDocument,
    render: &RenderHandle,
    first: PageIndex,
    last: PageIndex,
    page_count: PageIndex,
) {
    for page in first..=last {
        if page < 0 || page >= page_count {
            continue;
        }
        if doc.analyses.contains_key(&page) || !doc.analysis_requested.insert(page) {
            continue;
        }
        render.analyze(doc.id, page);
    }
}

/// Map a rectangle from unrotated PDF points to screen space.
///
/// PDF points put the origin at the bottom-left and count upwards; screen space
/// starts at the top-left and counts down. The view rotation is applied after
/// that flip, in normalized space, so it holds at any zoom.
#[must_use]
pub fn pt_to_screen(rect: RectPt, page_w: f32, page_h: f32, rotation: u8, screen: Rect) -> Rect {
    if page_w <= 0.0 || page_h <= 0.0 {
        return Rect::NOTHING;
    }
    let a = map_point(rect.left, rect.top, page_w, page_h, rotation, screen);
    let b = map_point(rect.right, rect.bottom, page_w, page_h, rotation, screen);
    Rect::from_two_pos(a, b)
}

fn map_point(x: f32, y: f32, page_w: f32, page_h: f32, rotation: u8, screen: Rect) -> Pos2 {
    // Normalized, with y already flipped to "down".
    let u = x / page_w;
    let d = 1.0 - (y / page_h);
    let (u, d) = match rotation % 4 {
        1 => (1.0 - d, u),
        2 => (1.0 - u, 1.0 - d),
        3 => (d, 1.0 - u),
        _ => (u, d),
    };
    pos2(
        screen.min.x + u * screen.width(),
        screen.min.y + d * screen.height(),
    )
}

/// Map a screen position back to unrotated PDF points.
///
/// The inverse of [`map_point`], used to hit-test links and place the text
/// cursor.
#[must_use]
pub fn screen_to_pt(pos: Pos2, page_w: f32, page_h: f32, rotation: u8, screen: Rect) -> (f32, f32) {
    if screen.width() <= 0.0 || screen.height() <= 0.0 {
        return (0.0, 0.0);
    }
    let u = (pos.x - screen.min.x) / screen.width();
    let d = (pos.y - screen.min.y) / screen.height();
    let (u, d) = match rotation % 4 {
        1 => (d, 1.0 - u),
        2 => (1.0 - u, 1.0 - d),
        3 => (1.0 - d, u),
        _ => (u, d),
    };
    (u * page_w, (1.0 - d) * page_h)
}

/// Convert a width in screen points to the pixel width to render at.
///
/// Quantized to 64-pixel steps so a smooth zoom or a window drag does not
/// invalidate the cache on every single frame.
fn target_width_px(width_points: f32, pixels_per_point: f32) -> u32 {
    let raw = (width_points * pixels_per_point).max(1.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped above"
    )]
    let px = raw.ceil() as u32;
    px.div_ceil(64) * 64
}

/// Scroll offset that puts the top of `page` at the top of the viewport.
fn offset_of_page(doc: &OpenDocument, page: PageIndex) -> f32 {
    let mut offset = 0.0;
    for index in 0..page {
        let (_, h_pt) = doc.page_size_pt(index);
        offset += PAGE_MARGIN + h_pt * doc.scale;
    }
    offset + PAGE_MARGIN
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ypdf_render::{DocumentId, DocumentInfo};

    use super::*;
    use crate::document::Zoom;

    fn document(pages: PageIndex) -> OpenDocument {
        let mut doc = OpenDocument::opening(DocumentId::new(), PathBuf::from("t.pdf"), 64);
        doc.info = Some(DocumentInfo {
            path: PathBuf::from("t.pdf"),
            page_count: pages,
            page_sizes_pt: vec![(612.0, 792.0); pages as usize],
        });
        doc.zoom = Zoom::Scale(1.0);
        doc.scale = 1.0;
        doc
    }

    fn screen() -> Rect {
        Rect::from_min_size(pos2(100.0, 50.0), vec2(612.0, 792.0))
    }

    #[test]
    fn render_widths_are_quantized() {
        // Two nearby zoom levels must map to the same raster.
        assert_eq!(target_width_px(600.0, 1.0), target_width_px(610.0, 1.0));
        assert_eq!(target_width_px(600.0, 1.0) % 64, 0);
    }

    #[test]
    fn render_width_follows_display_scaling() {
        assert!(target_width_px(600.0, 2.0) > target_width_px(600.0, 1.0));
    }

    #[test]
    fn page_offsets_accumulate_page_heights_and_margins() {
        let doc = document(5);
        assert_eq!(offset_of_page(&doc, 0), PAGE_MARGIN);
        assert_eq!(offset_of_page(&doc, 1), PAGE_MARGIN * 2.0 + 792.0);
    }

    #[test]
    fn pdf_points_flip_to_screen_space() {
        // A box at the top of the page must land at the top on screen.
        let rect = RectPt {
            left: 0.0,
            bottom: 782.0,
            right: 100.0,
            top: 792.0,
        };
        let mapped = pt_to_screen(rect, 612.0, 792.0, 0, screen());
        assert!((mapped.min.x - 100.0).abs() < 0.01);
        assert!(
            (mapped.min.y - 50.0).abs() < 0.01,
            "top of the page is the top of the box"
        );
        assert!((mapped.height() - 10.0).abs() < 0.01);
    }

    #[test]
    fn mapping_round_trips_at_every_rotation() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(612.0, 792.0));
        for rotation in 0..4 {
            let (x, y) = (150.0, 600.0);
            let pos = map_point(x, y, 612.0, 792.0, rotation, screen);
            let (bx, by) = screen_to_pt(pos, 612.0, 792.0, rotation, screen);
            assert!((bx - x).abs() < 0.01, "rotation {rotation}: x {bx} != {x}");
            assert!((by - y).abs() < 0.01, "rotation {rotation}: y {by} != {y}");
        }
    }

    #[test]
    fn a_quarter_turn_moves_the_top_left_to_the_top_right() {
        let rect = RectPt {
            left: 0.0,
            bottom: 782.0,
            right: 20.0,
            top: 792.0,
        };
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(792.0, 612.0));
        let mapped = pt_to_screen(rect, 612.0, 792.0, 1, screen);
        assert!(
            mapped.max.x > screen.width() * 0.9,
            "expected the right edge, got {mapped:?}"
        );
        assert!(
            mapped.min.y < screen.height() * 0.1,
            "expected the top edge, got {mapped:?}"
        );
    }

    #[test]
    fn a_degenerate_page_maps_to_nothing_instead_of_panicking() {
        let rect = RectPt {
            left: 0.0,
            bottom: 0.0,
            right: 1.0,
            top: 1.0,
        };
        assert_eq!(pt_to_screen(rect, 0.0, 792.0, 0, screen()), Rect::NOTHING);
    }
}
