//! Application state and the egui frame loop.

use std::path::PathBuf;

use ypdf_core::Config;
use ypdf_render::{RenderEvent, RenderHandle};

use crate::document::{OpenDocument, Zoom};
use crate::edit::{self, Op};
use crate::inspect;
use crate::recent::Recent;
use crate::{thumbnails, viewer};

/// A page operation requested from the toolbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditAction {
    /// Rotate by quarter turns clockwise.
    Rotate(i32),
    /// Duplicate the target pages.
    Duplicate,
    /// Delete the target pages.
    Delete,
    /// Keep only the target pages.
    Extract,
    /// Reverse the whole document.
    Reverse,
    /// Insert another PDF, chosen from a file dialog.
    Insert,
    /// Undo the last edit.
    Undo,
}

/// Which form edit `apply_form` is carrying out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormEdit {
    /// Write the edited values.
    Fill,
    /// Empty every field.
    Clear,
    /// Draw the fields into the page and remove the form.
    Flatten,
}

/// Root application state.
pub struct YpdfApp {
    config: Config,
    /// The one connection to the one render thread.
    render: RenderHandle,
    /// Open documents, one per tab.
    documents: Vec<OpenDocument>,
    /// Index into [`Self::documents`].
    active: usize,
    /// Show the thumbnail sidebar.
    show_thumbnails: bool,
    /// Recently opened files (spec §28).
    recent: Recent,
    /// Most recent failure, shown in the status bar until the next one.
    status_error: Option<String>,
    /// Set for one frame when the find field should take keyboard focus.
    focus_search: bool,
}

impl YpdfApp {
    /// Build the app, apply visual defaults, and open any files given on the
    /// command line.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: Config,
        render: RenderHandle,
        open: Vec<PathBuf>,
    ) -> Self {
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        });

        let mut recent = Recent::load();
        recent.prune();

        let mut app = Self {
            config,
            render,
            documents: Vec::new(),
            active: 0,
            show_thumbnails: true,
            recent,
            status_error: None,
            focus_search: false,
        };
        for path in open {
            app.open(path);
        }
        app
    }

    /// Open a file in a new tab.
    fn open(&mut self, path: PathBuf) {
        tracing::info!(path = %path.display(), "opening document");
        let id = self.render.open(path.clone(), None);
        self.recent.push(&path);
        self.documents.push(OpenDocument::opening(
            id,
            path,
            self.config.cache.texture_budget_mb,
        ));
        self.active = self.documents.len() - 1;
    }

    /// Close a tab and release its engine-side document.
    fn close(&mut self, index: usize) {
        if index >= self.documents.len() {
            return;
        }
        let doc = self.documents.remove(index);
        self.render.close(doc.id);
        self.active = self.active.min(self.documents.len().saturating_sub(1));
    }

    fn active_mut(&mut self) -> Option<&mut OpenDocument> {
        self.documents.get_mut(self.active)
    }

    /// Absorb everything the render thread has sent since the last frame.
    fn take_render_events(&mut self, ctx: &egui::Context) {
        for event in self.render.drain_events() {
            match event {
                RenderEvent::Opened { doc, info } => {
                    if let Some(target) = self.documents.iter_mut().find(|d| d.id == doc) {
                        tracing::info!(pages = info.page_count, "document ready");
                        target.info = Some(*info);
                        target.inspection.invalidate();
                    }
                }
                RenderEvent::Page(page) => {
                    if let Some(target) = self.documents.iter_mut().find(|d| d.id == page.doc) {
                        if let Ok(index) = u32::try_from(page.page)
                            && target.ocr.accept_raster(
                                index,
                                page.width,
                                page.rgba.clone(),
                                page.height,
                            )
                        {
                            // Claimed by the OCR run. It is far larger than the
                            // viewer wants, so it is not cached as a texture.
                            continue;
                        }
                        // A raster for a superseded view is worthless: the user
                        // has zoomed or rotated since it was requested.
                        if page.generation >= target.generation {
                            let invert = target.invert;
                            target.cache.insert(ctx, &page, invert);
                        }
                    }
                }
                RenderEvent::Failed { doc, page, error } => {
                    let report = error.report().to_human();
                    tracing::warn!(?page, "{report}");

                    // A protected file is not a broken one. Asking for the
                    // password is the whole remedy, so the tab stays alive and
                    // says which of the two situations it is in.
                    let needs_password =
                        matches!(error.code(), "E_PASSWORD_REQUIRED" | "E_PASSWORD_WRONG");
                    if needs_password && page.is_none() {
                        if let Some(target) = self.documents.iter_mut().find(|d| d.id == doc) {
                            target
                                .password_prompt
                                .ask(error.code() == "E_PASSWORD_WRONG");
                        }
                        continue;
                    }

                    // A failure about a page the document no longer has is the
                    // tail of a request issued before an edit shrank it. The
                    // page is gone, not broken, and reporting it would call a
                    // successful edit a failure.
                    if let Some(index) = page
                        && self
                            .documents
                            .iter()
                            .any(|d| d.id == doc && index >= d.page_count())
                    {
                        continue;
                    }

                    if let Some(target) = self.documents.iter_mut().find(|d| d.id == doc) {
                        // A failure to open kills the tab's content; a failure
                        // on one page only belongs in the status bar.
                        match page {
                            None => target.error = Some(report.clone()),
                            // Settled, badly. Anything waiting for the whole
                            // document needs to know this page is never coming.
                            Some(index) => {
                                target.analysis_failed.insert(index);
                            }
                        }
                    }
                    self.status_error = Some(error.message());
                }
                RenderEvent::Analyzed { doc, analysis } => {
                    if let Some(target) = self.documents.iter_mut().find(|d| d.id == doc) {
                        // Feed the page to an in-flight search before storing
                        // it, so results appear as extraction catches up.
                        target.search.index_page(analysis.page, &analysis.text);
                        target.analyses.insert(analysis.page, *analysis);
                    }
                }
                RenderEvent::Closed { .. } => {}
            }
        }
    }

    /// Absorb files dropped onto the window (spec §28).
    fn take_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            if let Some(path) = file.path {
                self.open(path);
            }
        }
    }

    /// Keyboard shortcuts (spec §28).
    fn take_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::Key;

        // Ctrl+F opens the find bar, Escape closes it, Ctrl+C copies the
        // selection. These are checked before the plain-key shortcuts below so
        // typing in the find field never rotates the page.
        let (find, copy, escape, enter, shift) = ctx.input(|i| {
            (
                i.modifiers.command && i.key_pressed(Key::F),
                i.modifiers.command && i.key_pressed(Key::C),
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::Enter),
                i.modifiers.shift,
            )
        });

        if find {
            if let Some(doc) = self.active_mut() {
                doc.search.open = true;
            }
            self.focus_search = true;
        }
        let (save, undo) = ctx.input(|i| {
            (
                i.modifiers.command && i.key_pressed(Key::S),
                i.modifiers.command && i.key_pressed(Key::Z),
            )
        });
        if save {
            let dirty = self
                .documents
                .get(self.active)
                .is_some_and(|d| d.edits.is_dirty());
            if dirty {
                self.save(false);
            }
        }
        if undo {
            self.undo();
        }
        if copy && let Some(text) = self.active_mut().and_then(|d| d.selected_text()) {
            tracing::debug!(chars = text.chars().count(), "copying selection");
            ctx.copy_text(text);
        }
        if let Some(doc) = self.active_mut() {
            if escape {
                doc.search.open = false;
                doc.selection = None;
            }
            if enter && doc.search.open && !doc.search.query.is_empty() {
                doc.focus_hit(!shift);
            }
        }

        // Typing in the find field must not also drive the viewer.
        if ctx.egui_wants_keyboard_input() {
            return;
        }

        let keys: Vec<Key> = ctx.input(|i| {
            [
                Key::PageDown,
                Key::PageUp,
                Key::Home,
                Key::End,
                Key::Plus,
                Key::Equals,
                Key::Minus,
                Key::Num0,
                Key::Num1,
                Key::R,
                Key::I,
                Key::F,
                Key::F11,
            ]
            .into_iter()
            .filter(|k| i.key_pressed(*k))
            .collect()
        });

        let Some(doc) = self.active_mut() else { return };
        for key in keys {
            match key {
                Key::PageDown => doc.go_to(doc.current_page + 1),
                Key::PageUp => doc.go_to(doc.current_page - 1),
                Key::Home => doc.go_to(0),
                Key::End => doc.go_to(doc.page_count() - 1),
                Key::Plus | Key::Equals => doc.set_zoom(Zoom::zoom_in(doc.scale)),
                Key::Minus => doc.set_zoom(Zoom::zoom_out(doc.scale)),
                Key::Num0 => doc.set_zoom(Zoom::FitPage),
                Key::Num1 => doc.set_zoom(Zoom::Scale(1.0)),
                Key::R => doc.rotate(),
                Key::I => doc.toggle_invert(),
                Key::F => doc.search.open = true,
                _ => {}
            }
        }

        if ctx.input(|i| i.key_pressed(Key::F11)) {
            let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
        }
    }

    /// The find bar (spec §2.1). Only shown while a search is open.
    fn search_bar(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        if !doc.search.open {
            return;
        }

        let mut jump_to = None;
        let mut restart = false;

        egui::Panel::top(egui::Id::new("find_bar")).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Find");

                let field = ui.add(
                    egui::TextEdit::singleline(&mut doc.search.query)
                        .desired_width(240.0)
                        .hint_text("Search this document"),
                );
                if std::mem::take(&mut self.focus_search) {
                    field.request_focus();
                }
                if field.changed() {
                    restart = true;
                }
                let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                let mut options = doc.search.options;
                let case = ui
                    .toggle_value(&mut options.case_sensitive, "Aa")
                    .on_hover_text("Case sensitive");
                let word = ui
                    .toggle_value(&mut options.whole_word, "|ab|")
                    .on_hover_text("Whole word");
                if case.changed() || word.changed() {
                    doc.search.options = options;
                    restart = true;
                }

                if ui
                    .button("<")
                    .on_hover_text("Previous (Shift+Enter)")
                    .clicked()
                {
                    jump_to = Some(false);
                }
                if ui.button(">").on_hover_text("Next (Enter)").clicked() || submitted {
                    jump_to = Some(true);
                }

                let total = doc.search.total();
                if doc.search.query.is_empty() {
                    ui.weak("");
                } else if total == 0 {
                    ui.colored_label(ui.visuals().warn_fg_color, "No results");
                } else {
                    ui.weak(format!("{} of {total}", doc.search.current_number()));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        doc.search.open = false;
                    }
                });
            });
        });

        if restart {
            Self::restart_search(doc, &self.render);
        }
        if let Some(forward) = jump_to {
            doc.focus_hit(forward);
        }
    }

    /// Re-run the query from scratch.
    ///
    /// Searching needs the text of pages the user has never scrolled to, so
    /// this is the one place that asks for the whole document. Extraction stays
    /// behind rendering on the render thread, so it costs reading nothing.
    fn restart_search(doc: &mut OpenDocument, render: &RenderHandle) {
        doc.search.clear_results();
        if doc.search.query.is_empty() {
            return;
        }

        for page in 0..doc.page_count() {
            match doc.analyses.get(&page) {
                Some(analysis) => {
                    let text = analysis.text.clone();
                    doc.search.index_page(page, &text);
                }
                None => {
                    if doc.analysis_requested.insert(page) {
                        render.analyze(doc.id, page);
                    }
                }
            }
        }
    }

    /// Page operations (spec §3). Enabled once a document is open.
    fn edit_controls(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.documents.get(self.active) else {
            return;
        };
        let selected = doc.selected_pages.len();
        let can_undo = doc.edits.can_undo();
        let dirty = doc.edits.is_dirty();
        let undo_hint = doc.edits.undo_description().map_or_else(
            || "Nothing to undo".to_string(),
            |what| format!("{what}  ({} edits)", doc.edits.len()),
        );

        let mut op = None;
        let mut save = None;
        let mut open_split = false;
        let mut open_merge = false;

        ui.menu_button("Pages", |ui| {
            let label = if selected == 0 {
                "current page".to_string()
            } else {
                format!("{selected} selected")
            };
            ui.weak(format!("Acting on: {label}"));
            ui.separator();

            if ui.button("Rotate 90°").clicked() {
                op = Some(EditAction::Rotate(1));
                ui.close();
            }
            if ui.button("Duplicate").clicked() {
                op = Some(EditAction::Duplicate);
                ui.close();
            }
            if ui.button("Delete").clicked() {
                op = Some(EditAction::Delete);
                ui.close();
            }
            if ui.button("Keep only these").clicked() {
                op = Some(EditAction::Extract);
                ui.close();
            }
            ui.separator();
            if ui.button("Reverse document").clicked() {
                op = Some(EditAction::Reverse);
                ui.close();
            }
            if ui.button("Insert another PDF…").clicked() {
                op = Some(EditAction::Insert);
                ui.close();
            }
            ui.separator();
            if ui
                .button("Split…")
                .on_hover_text("Write several files, leaving this one as it is")
                .clicked()
            {
                open_split = true;
                ui.close();
            }
            if ui
                .button("Merge…")
                .on_hover_text("Join this document with others into a new file")
                .clicked()
            {
                open_merge = true;
                ui.close();
            }
        });

        if open_split && let Some(doc) = self.documents.get_mut(self.active) {
            doc.split.open = true;
        }

        if open_merge && let Some(doc) = self.documents.get_mut(self.active) {
            // Seeded here rather than in the dialog: only the application knows
            // what the open document is called and how long it is.
            let title = doc.title();
            let pages = u32::try_from(doc.page_count()).unwrap_or(0);
            doc.merge.seed(title, pages);
            doc.merge.open = true;
        }

        if ui
            .add_enabled(can_undo, egui::Button::new("Undo"))
            .on_hover_text(format!("Undo: {undo_hint}"))
            .on_disabled_hover_text("Nothing to undo")
            .clicked()
        {
            op = Some(EditAction::Undo);
        }

        if ui
            .add_enabled(dirty, egui::Button::new("Save"))
            .on_hover_text("Overwrite the file (Ctrl+S)")
            .clicked()
        {
            save = Some(false);
        }
        if ui.button("Save as…").clicked() {
            save = Some(true);
        }
        if dirty {
            ui.colored_label(ui.visuals().warn_fg_color, "•")
                .on_hover_text("Unsaved edits");
        }

        if let Some(action) = op {
            self.edit(action);
        }
        if let Some(pick_path) = save {
            self.save(pick_path);
        }
    }

    /// Apply an edit, then hand the edited bytes back to the renderer.
    ///
    /// The document keeps its id, so the tab stays put; everything derived from
    /// the old contents is dropped, because page 4 after a delete is not the
    /// page 4 that was rendered.
    fn edit(&mut self, action: EditAction) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };

        let pages = doc.target_pages();
        let op = match action {
            EditAction::Rotate(turns) => Some(Op::Rotate(pages, turns)),
            EditAction::Duplicate => Some(Op::Duplicate(pages)),
            EditAction::Delete => Some(Op::Delete(pages)),
            EditAction::Extract => Some(Op::Extract(pages)),
            EditAction::Reverse => Some(Op::Reverse),
            EditAction::Insert => rfd::FileDialog::new()
                .add_filter("PDF", &["pdf"])
                .set_title("Insert a PDF")
                .pick_file()
                .map(|path| Op::Insert {
                    path,
                    at: u32::try_from(doc.current_page + 1).unwrap_or(1),
                }),
            EditAction::Undo => None,
        };

        match (action, op) {
            (EditAction::Undo, _) => self.undo(),
            (_, Some(op)) => self.apply_op(op),
            (_, None) => {}
        }
    }

    /// Record an operation, then hand the edited bytes back to the renderer.
    fn apply_op(&mut self, op: Op) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };

        let edited = doc
            .edits
            .apply(op.clone())
            .and_then(|mut pdf| Ok((pdf.page_count(), pdf.to_bytes()?)));

        let (pages, bytes) = match edited {
            Ok(edited) => edited,
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                self.status_error = Some(edit::describe_failure(&op, &e));
                return;
            }
        };

        tracing::info!(op = %op.describe(), "applied edit");
        doc.invalidate_content();
        doc.set_page_count(page_index(pages));
        self.render.reopen(doc.id, bytes);
        self.status_error = None;
    }

    /// Undo the last edit and show the document as it now stands.
    fn undo(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };

        match doc.edits.undo() {
            Ok(None) => {}
            Ok(Some(mut pdf)) => {
                let pages = pdf.page_count();
                match pdf.to_bytes() {
                    Ok(bytes) => {
                        doc.invalidate_content();
                        doc.set_page_count(page_index(pages));
                        self.render.reopen(doc.id, bytes);
                        self.status_error = None;
                    }
                    Err(e) => self.status_error = Some(e.message()),
                }
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                self.status_error = Some(e.message());
            }
        }
    }

    /// Save the edited document, optionally asking where.
    fn save(&mut self, ask_where: bool) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };

        let target = if ask_where {
            let suggestion = doc.edits.source().file_stem().map_or_else(
                || "document".to_string(),
                |s| s.to_string_lossy().into_owned(),
            );
            rfd::FileDialog::new()
                .add_filter("PDF", &["pdf"])
                .set_file_name(format!("{suggestion}-edited.pdf"))
                .set_title("Save PDF as")
                .save_file()
        } else {
            Some(doc.edits.source().to_path_buf())
        };

        let Some(target) = target else { return };
        match doc.edits.save_as(&target) {
            Ok(()) => {
                tracing::info!(path = %target.display(), "saved");
                doc.path = target;
                self.status_error = None;
            }
            Err(e) => {
                self.status_error = Some(e.message());
                tracing::warn!("{}", e.report().to_human());
            }
        }
    }

    /// Keep the inspect panel's reports in step with the document.
    ///
    /// Reading them re-parses the file with `ypdf-doc`, which never touches
    /// PDFium, so it cannot interfere with rendering. It happens only when the
    /// panel is open and the reports are stale.
    fn refresh_inspection(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        if !doc.inspection.open || doc.inspection.loaded {
            return;
        }

        let source = doc.edits.source().to_path_buf();
        let edited = doc.edits.is_dirty();
        match doc.edits.replay().and_then(|pdf| {
            doc.inspection.refresh(&pdf, &source, edited)?;
            Ok(())
        }) {
            Ok(()) => {
                tracing::debug!(
                    findings = doc.inspection.security.findings.len(),
                    issues = doc.inspection.diagnostics.issues.len(),
                    "inspected document"
                );
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                doc.inspection.error = Some(e.message());
                doc.inspection.loaded = true;
            }
        }
    }

    /// Write the drafted metadata edit into the document (spec §13).
    fn apply_metadata(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let edit = doc.inspection.pending_edit();
        if edit.is_empty() {
            return;
        }

        // Metadata is not a page operation, so it is applied to the document
        // directly and saved through the edit session's source file. It is
        // deliberately outside the undo log: the log replays page operations
        // from the file on disk, and a metadata change would be replayed onto
        // a file that already has it.
        let result = doc.edits.replay().and_then(|mut pdf| {
            pdf.set_metadata(&edit)?;
            let path = doc.edits.source().to_path_buf();
            pdf.save(&path)?;
            pdf.to_bytes()
        });

        match result {
            Ok(bytes) => {
                tracing::info!("metadata updated");
                doc.inspection.invalidate();
                self.render.reopen(doc.id, bytes);
                self.status_error = None;
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                self.status_error = Some(e.message());
            }
        }
    }

    /// Run, keep, or discard a compression (spec §4).
    fn handle_compression(&mut self, ctx: &egui::Context) {
        let Some(action) = self
            .documents
            .get_mut(self.active)
            .and_then(|doc| doc.compress.show(ctx))
        else {
            return;
        };

        match action {
            crate::compress::Action::Run => self.run_compression(),
            crate::compress::Action::SaveAs => self.save_compressed(),
            crate::compress::Action::Discard => self.discard_compression(),
        }
    }

    /// Convert the document to Markdown or Word (spec §5).
    fn handle_export(&mut self, ctx: &egui::Context) {
        self.collect_export();

        let Some(action) = self.documents.get_mut(self.active).and_then(|doc| {
            let total = usize::try_from(doc.page_count()).unwrap_or(0);
            let settled = doc.analyses.len() + doc.analysis_failed.len();
            doc.export.show(ctx, settled.min(total), total)
        }) else {
            return;
        };

        match action {
            crate::export::Action::Convert => self.start_export(),
            crate::export::Action::SaveMarkdown => self.save_export(Format::Markdown),
            crate::export::Action::SaveDocx => self.save_export(Format::Docx),
            crate::export::Action::Copy => self.copy_markdown(ctx),
        }
    }

    /// Ask for the text of every page, then wait.
    ///
    /// Like a search, this is one of the few places that wants the whole
    /// document rather than the pages on screen. Extraction sits behind
    /// rendering on the render thread, so it costs reading nothing.
    fn start_export(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };

        doc.export.result = None;
        doc.export.preview.clear();
        doc.export.error = None;
        doc.export.waiting = true;

        for page in 0..doc.page_count() {
            if !doc.analyses.contains_key(&page) && doc.analysis_requested.insert(page) {
                self.render.analyze(doc.id, page);
            }
        }
    }

    /// Rebuild the structure once every page has settled, one way or the other.
    fn collect_export(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        if !doc.export.waiting {
            return;
        }

        let total = doc.page_count();
        if total <= 0 {
            doc.export.waiting = false;
            return;
        }

        let outstanding = (0..total)
            .any(|page| !doc.analyses.contains_key(&page) && !doc.analysis_failed.contains(&page));
        if outstanding {
            return;
        }

        // A page whose text could not be read contributes an empty page, which
        // the converter reports as having no text layer. Dropping it instead
        // would quietly renumber every warning after it.
        let pages: Vec<ypdf_render::PageText> = (0..total)
            .map(|page| {
                doc.analyses
                    .get(&page)
                    .map(|analysis| analysis.text.clone())
                    .unwrap_or_default()
            })
            .collect();

        let document = ypdf_convert::read(&pages);
        doc.export.preview = ypdf_convert::markdown::render(&document.blocks);
        doc.export.result = Some(document);
        doc.export.waiting = false;
    }

    /// Write the conversion where the user says, in the format they asked for.
    fn save_export(&mut self, format: Format) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let Some(document) = doc.export.result.as_ref() else {
            return;
        };

        // Rendered at the point of saving rather than held in two forms: the
        // reconstruction is the answer, and both files are written from it.
        let bytes = match format {
            Format::Markdown => doc.export.preview.clone().into_bytes(),
            Format::Docx => ypdf_convert::docx::write(&document.blocks),
        };

        let stem = doc.edits.source().file_stem().map_or_else(
            || "document".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        let Some(target) = rfd::FileDialog::new()
            .add_filter(format.label(), &[format.extension()])
            .set_file_name(format!("{stem}.{}", format.extension()))
            .set_title(format.title())
            .save_file()
        else {
            return;
        };

        match std::fs::write(&target, &bytes) {
            Ok(()) => {
                tracing::info!(path = %target.display(), format = format.label(), "saved export");
                doc.export.error = None;
                self.status_error = None;
            }
            Err(e) => {
                let error = ypdf_core::Error::io(&target, e);
                tracing::warn!("{}", error.report().to_human());
                doc.export.error = Some(error.message());
            }
        }
    }

    /// Put the Markdown on the clipboard.
    fn copy_markdown(&mut self, ctx: &egui::Context) {
        let Some(text) = self
            .documents
            .get(self.active)
            .map(|doc| doc.export.preview.clone())
            .filter(|text| !text.is_empty())
        else {
            return;
        };
        tracing::debug!(chars = text.chars().count(), "copying markdown");
        ctx.copy_text(text);
    }

    /// Cut the document into several files (spec §3.2).
    fn handle_split(&mut self, ctx: &egui::Context) {
        let Some(action) = self.documents.get_mut(self.active).and_then(|doc| {
            let pages = u32::try_from(doc.page_count()).unwrap_or(0);
            doc.split.show(ctx, pages)
        }) else {
            return;
        };

        match action {
            crate::split::Action::Run => self.run_split(),
        }
    }

    /// Write every piece into a folder the user picks.
    ///
    /// The document is split as edited rather than as last saved, so the pieces
    /// match what is on screen. Nothing is overwritten: piece names come from
    /// page numbers, so a second run with different settings would quietly
    /// replace the first run's work, and a file already there stops the run and
    /// says which one it was.
    fn run_split(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        doc.split.error = None;

        let mode = match doc.split.resolve() {
            Ok(mode) => mode,
            Err(e) => {
                doc.split.error = Some(e.message());
                return;
            }
        };

        let pieces = match doc
            .edits
            .replay()
            .and_then(|pdf| ypdf_doc::split(&pdf, &mode))
        {
            Ok(pieces) => pieces,
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                doc.split.error = Some(e.message());
                return;
            }
        };

        if pieces.is_empty() {
            doc.split.error = Some("Those settings cut nothing out.".to_string());
            return;
        }

        // Asked for only once the settings are known to work: a folder dialog
        // for a run that was never going to happen is pure noise.
        let Some(folder) = rfd::FileDialog::new()
            .set_title("Folder for the split files")
            .pick_folder()
        else {
            return;
        };

        let stem = doc.edits.source().file_stem().map_or_else(
            || "document".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );

        let (written, failure) = crate::split::write_pieces(&folder, &stem, pieces);
        tracing::info!(
            files = written.len(),
            folder = %folder.display(),
            "split document"
        );
        doc.split.error = failure;
        doc.split.written = written;
    }

    /// Add files to, or run, a merge (spec §3.1).
    fn handle_merge(&mut self, ctx: &egui::Context) {
        let Some(action) = self.documents.get_mut(self.active).and_then(|doc| {
            // The document can be edited while the dialog is up, so its length
            // is re-read every frame rather than trusted from when it was added.
            let pages = u32::try_from(doc.page_count()).unwrap_or(0);
            doc.merge.refresh_open(pages);
            doc.merge.show(ctx)
        }) else {
            return;
        };

        match action {
            crate::merge::Action::AddFiles => self.add_merge_files(),
            crate::merge::Action::Run => self.run_merge(),
        }
    }

    /// Put more documents on the end of the merge list.
    ///
    /// Each is opened as it is added so the list can show its length, and so a
    /// file that cannot be read is caught now rather than halfway through a
    /// merge the user has already committed to.
    fn add_merge_files(&mut self) {
        let Some(paths) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_title("Add PDFs to merge")
            .pick_files()
        else {
            return;
        };

        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };

        for path in paths {
            match ypdf_doc::Pdf::open(&path) {
                Ok(pdf) => {
                    let pages = pdf.page_count();
                    doc.merge.push_file(path, pages);
                    doc.merge.error = None;
                }
                Err(e) => {
                    tracing::warn!("{}", e.report().to_human());
                    doc.merge.error = Some(format!("{}: {}", path.display(), e.message()));
                }
            }
        }
    }

    /// Join the listed documents and write the result where the user says.
    ///
    /// The open document contributes its edits, not the file on disk, which is
    /// the same promise the split dialog makes.
    fn run_merge(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        doc.merge.error = None;

        // Cloned so the loop can report a failure into the dialog it is
        // reading from.
        let entries = doc.merge.entries.clone();
        let mut documents = Vec::with_capacity(entries.len());
        for entry in &entries {
            let opened = match &entry.source {
                crate::merge::Source::Open => doc.edits.replay(),
                crate::merge::Source::File(path) => ypdf_doc::Pdf::open(path),
            };
            match opened {
                Ok(pdf) => documents.push(pdf),
                Err(e) => {
                    tracing::warn!("{}", e.report().to_human());
                    doc.merge.error = Some(format!("{}: {}", entry.label, e.message()));
                    return;
                }
            }
        }

        let bytes = match ypdf_doc::Pdf::merge(&documents).and_then(|mut pdf| pdf.to_bytes()) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                doc.merge.error = Some(e.message());
                return;
            }
        };

        let stem = doc.edits.source().file_stem().map_or_else(
            || "document".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        // A save dialog rather than a bare folder: merging produces one file,
        // and the native dialog is already the place where overwriting is
        // asked about properly.
        let Some(target) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("{stem}-merged.pdf"))
            .set_title("Save merged PDF")
            .save_file()
        else {
            return;
        };

        match std::fs::write(&target, &bytes) {
            Ok(()) => {
                tracing::info!(
                    documents = entries.len(),
                    path = %target.display(),
                    "merged documents"
                );
                doc.merge.written = Some(target);
            }
            Err(e) => {
                let error = ypdf_core::Error::io(&target, e);
                tracing::warn!("{}", error.report().to_human());
                doc.merge.error = Some(error.message());
            }
        }
    }

    /// Compress the current document into memory.
    ///
    /// Nothing is written: the result is held until the user saves it, because
    /// compression throws pixels away and the original is the only copy.
    fn run_compression(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };

        let settings = doc.compress.settings.clone();
        let strip = doc.compress.strip_metadata;
        doc.compress.error = None;

        let outcome = doc.edits.replay().and_then(|mut pdf| {
            if strip {
                ypdf_optimize::strip_metadata(&mut pdf);
            }
            let mut report = ypdf_optimize::optimize(&mut pdf, &settings)?;
            report.metadata_removed = strip;
            Ok((report, pdf.to_bytes()?))
        });

        match outcome {
            Ok((report, bytes)) => {
                tracing::info!(
                    before = report.before,
                    after = report.after,
                    "compressed into memory"
                );
                doc.compressed = Some(bytes.clone());
                doc.compress.report = Some(report);
                doc.invalidate_content();
                self.render.reopen(doc.id, bytes);
                self.status_error = None;
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                doc.compress.error = Some(e.message());
            }
        }
    }

    /// Write the compressed copy to a file the user picks.
    fn save_compressed(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let Some(bytes) = doc.compressed.clone() else {
            return;
        };

        let stem = doc.edits.source().file_stem().map_or_else(
            || "document".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        let Some(target) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("{stem}-compressed.pdf"))
            .set_title("Save compressed PDF")
            .save_file()
        else {
            return;
        };

        match std::fs::write(&target, &bytes) {
            Ok(()) => {
                tracing::info!(path = %target.display(), "saved compressed document");
                doc.compressed = None;
                doc.compress.open = false;
                doc.compress.report = None;
                self.status_error = None;
            }
            Err(e) => {
                let error = ypdf_core::Error::io(&target, e);
                tracing::warn!("{}", error.report().to_human());
                doc.compress.error = Some(error.message());
            }
        }
    }

    /// Throw the compressed copy away and show the document as it was.
    fn discard_compression(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        doc.compressed = None;
        doc.compress.report = None;

        match doc.edits.replay().and_then(|mut pdf| pdf.to_bytes()) {
            Ok(bytes) => {
                doc.invalidate_content();
                self.render.reopen(doc.id, bytes);
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                self.status_error = Some(e.message());
            }
        }
    }

    /// Ask for a password, and retry the open with it (spec §8).
    fn handle_password_prompt(&mut self, ctx: &egui::Context) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let name = doc.path.file_name().map_or_else(
            || doc.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );

        let Some(password) = doc.password_prompt.show(ctx, &name) else {
            return;
        };

        // Every replay re-opens the original file, so the edit session needs
        // the password too.
        doc.edits.set_password(&password);
        doc.info = None;
        doc.error = None;
        doc.inspection.invalidate();

        let path = doc.path.clone();
        let old = doc.id;
        let id = self.render.open(path, Some(password));
        if let Some(doc) = self.documents.get_mut(self.active) {
            doc.id = id;
        }
        self.render.close(old);
    }

    /// Write a protected copy of the document (spec §8).
    fn handle_protect(&mut self, ctx: &egui::Context) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        if !doc.protect.show(ctx) {
            return;
        }

        let settings = doc.protect.settings();
        let outcome = doc
            .edits
            .replay()
            .and_then(|mut pdf| ypdf_crypt::encrypt(&mut pdf, &settings));

        let bytes = match outcome {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                doc.protect.error = Some(e.message());
                return;
            }
        };

        let stem = doc.edits.source().file_stem().map_or_else(
            || "document".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        let Some(target) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("{stem}-protected.pdf"))
            .set_title("Save protected PDF")
            .save_file()
        else {
            return;
        };

        match std::fs::write(&target, &bytes) {
            Ok(()) => {
                tracing::info!(
                    path = %target.display(),
                    algorithm = settings.algorithm.label(),
                    "wrote protected document"
                );
                doc.protect.error = None;
                doc.protect.open = false;
                // The passwords have done their work; keeping them in a dialog
                // that is still on screen serves nothing.
                doc.protect.user_password.clear();
                doc.protect.owner_password.clear();
                self.status_error = None;
            }
            Err(e) => {
                let error = ypdf_core::Error::io(&target, e);
                tracing::warn!("{}", error.report().to_human());
                doc.protect.error = Some(error.message());
            }
        }
    }

    /// Drive an OCR run (spec §7).
    ///
    /// Three things happen here, all of them cheap: collect a finished run,
    /// answer the dialog, and ask the render thread for the next page. The
    /// recognition itself is on a worker thread and the rasterizing is on the
    /// render thread, so the UI keeps drawing throughout.
    fn handle_ocr(&mut self, ctx: &egui::Context) {
        if let Some(doc) = self.documents.get_mut(self.active) {
            doc.ocr.poll();
        }

        let action = self
            .documents
            .get_mut(self.active)
            .and_then(|doc| doc.ocr.show(ctx));

        match action {
            Some(crate::ocr::Action::Run) => self.start_ocr(),
            Some(crate::ocr::Action::SaveAs) => self.save_ocr(),
            Some(crate::ocr::Action::Discard) => {
                if let Some(doc) = self.documents.get_mut(self.active) {
                    doc.ocr.result = None;
                    doc.ocr.summary = None;
                }
            }
            Some(crate::ocr::Action::Cancel) => {
                if let Some(doc) = self.documents.get_mut(self.active) {
                    doc.ocr.cancel();
                }
            }
            None => {}
        }

        self.request_ocr_page();
    }

    /// Begin a run over every page of the document as it currently stands.
    fn start_ocr(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let Some(info) = doc.info.as_ref() else {
            return;
        };
        if doc.ocr.is_running() {
            // A second worker would race the first for the same document and
            // produce a text layer nobody asked for.
            return;
        }

        let pages: Vec<u32> = (0..info.page_count)
            .filter_map(|page| u32::try_from(page).ok())
            .collect();

        // The width is picked from the first page: pages of different sizes in
        // one document would each want their own, but asking for one width per
        // run keeps the raster/OCR pairing unambiguous, and a page that is a
        // different size is still recognized — just at a different effective
        // resolution.
        let width_pt = info.page_sizes_pt.first().map_or(612.0, |(w, _)| *w);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped"
        )]
        let width = (((width_pt / 72.0) * doc.ocr.dpi as f32).round().max(1.0) as u32).min(10_000);

        let source = match doc.edits.replay() {
            Ok(pdf) => pdf,
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                doc.ocr.error = Some(e.message());
                return;
            }
        };

        if let Err(e) = doc.ocr.start(source, pages, width, None) {
            // A missing Tesseract lists every path it looked in; that whole
            // message is the useful part, so it goes in the dialog rather than
            // being reduced to a headline.
            tracing::warn!("{}", e.report().to_human());
            doc.ocr.error = Some(e.report().to_human());
        }
    }

    /// Ask the render thread for the page OCR is waiting on.
    fn request_ocr_page(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let Some((page, width)) = doc.ocr.next_page() else {
            return;
        };
        let Ok(page) = i32::try_from(page) else {
            return;
        };

        self.render.request(ypdf_render::RenderRequest {
            doc: doc.id,
            page,
            target_width: width,
            rotation: ypdf_render::QuarterTurns(0),
            priority: ypdf_render::Priority::Nearby,
            generation: doc.generation,
        });
    }

    /// Write the recognized document to a file the user picks.
    fn save_ocr(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let Some(bytes) = doc.ocr.result.clone() else {
            return;
        };

        let stem = doc.edits.source().file_stem().map_or_else(
            || "document".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        let Some(target) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("{stem}-ocr.pdf"))
            .set_title("Save searchable PDF")
            .save_file()
        else {
            return;
        };

        match std::fs::write(&target, &bytes) {
            Ok(()) => {
                tracing::info!(path = %target.display(), "saved searchable document");
                doc.ocr.result = None;
                doc.ocr.summary = None;
                doc.ocr.open = false;
                self.status_error = None;
            }
            Err(e) => {
                let error = ypdf_core::Error::io(&target, e);
                tracing::warn!("{}", error.report().to_human());
                doc.ocr.error = Some(error.message());
            }
        }
    }

    /// Stamp, keep, or discard a watermark (spec §12).
    fn handle_watermark(&mut self, ctx: &egui::Context) {
        let Some(action) = self
            .documents
            .get_mut(self.active)
            .and_then(|doc| doc.watermark.show(ctx))
        else {
            return;
        };

        match action {
            crate::watermark::Action::Apply => self.run_watermark(),
            crate::watermark::Action::ChooseImage => self.choose_watermark_image(),
            crate::watermark::Action::ClearImage => {
                if let Some(doc) = self.documents.get_mut(self.active) {
                    doc.watermark.image = None;
                }
            }
            crate::watermark::Action::SaveAs => self.save_watermarked(),
            crate::watermark::Action::Discard => self.discard_watermark(),
        }
    }

    /// Stamp into memory and show the result on the page.
    fn run_watermark(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };

        let watermark = match doc.watermark.watermark() {
            Ok(watermark) => watermark,
            Err(message) => {
                doc.watermark.error = Some(message);
                return;
            }
        };

        let pages: Vec<u32> = if doc.watermark.all_pages {
            let count = doc.info.as_ref().map_or(0, |info| info.page_count);
            (1..=count)
                .filter_map(|page| u32::try_from(page).ok())
                .collect()
        } else {
            u32::try_from(doc.current_page + 1)
                .map(|page| vec![page])
                .unwrap_or_default()
        };

        let outcome = doc.edits.replay().and_then(|mut pdf| {
            let report = ypdf_watermark::apply(&mut pdf, &pages, &watermark)?;
            Ok((report, pdf.to_bytes()?))
        });

        match outcome {
            Ok((report, bytes)) => {
                if report.pages == 0 {
                    // A copy that looks identical, reported as done, sends
                    // someone looking for the problem in the wrong place.
                    doc.watermark.error = Some(if report.unencodable > 0 {
                        "The built-in fonts cannot draw that text.".to_string()
                    } else {
                        "No pages were stamped.".to_string()
                    });
                    return;
                }

                tracing::info!(pages = report.pages, "watermark applied");
                doc.watermark.error = None;
                doc.watermark.stamped = report.pages;
                doc.watermark.result = Some(bytes.clone());
                doc.invalidate_content();
                self.render.reopen(doc.id, bytes);
                self.status_error = None;
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                doc.watermark.error = Some(e.message());
            }
        }
    }

    /// Pick an image to stamp.
    fn choose_watermark_image(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg"])
            .set_title("Choose a watermark image")
            .pick_file()
        else {
            return;
        };

        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        match std::fs::read(&path) {
            Ok(bytes) => {
                doc.watermark.image = Some((path, bytes));
                doc.watermark.error = None;
                // An upright logo is what people mean; a diagonal one is not.
                doc.watermark.rotation = 0.0;
            }
            Err(e) => {
                let error = ypdf_core::Error::io(&path, e);
                tracing::warn!("{}", error.report().to_human());
                doc.watermark.error = Some(error.message());
            }
        }
    }

    /// Write the stamped copy to a file the user picks.
    fn save_watermarked(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let Some(bytes) = doc.watermark.result.clone() else {
            return;
        };

        let stem = doc.edits.source().file_stem().map_or_else(
            || "document".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        let Some(target) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("{stem}-watermarked.pdf"))
            .set_title("Save watermarked PDF")
            .save_file()
        else {
            return;
        };

        match std::fs::write(&target, &bytes) {
            Ok(()) => {
                tracing::info!(path = %target.display(), "saved watermarked document");
                doc.watermark.result = None;
                doc.watermark.open = false;
                self.status_error = None;
            }
            Err(e) => {
                let error = ypdf_core::Error::io(&target, e);
                tracing::warn!("{}", error.report().to_human());
                doc.watermark.error = Some(error.message());
            }
        }
    }

    /// Throw the stamped copy away and show the document as it was.
    fn discard_watermark(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        doc.watermark.result = None;
        doc.watermark.stamped = 0;

        match doc.edits.replay().and_then(|mut pdf| pdf.to_bytes()) {
            Ok(bytes) => {
                doc.invalidate_content();
                self.render.reopen(doc.id, bytes);
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                self.status_error = Some(e.message());
            }
        }
    }

    /// Mark, apply, keep, or discard a redaction (spec §11).
    fn handle_redaction(&mut self, ctx: &egui::Context) {
        let Some(action) = self
            .documents
            .get_mut(self.active)
            .and_then(|doc| doc.redact.show(ctx))
        else {
            return;
        };

        match action {
            crate::redact::Action::Find => self.mark_matches(),
            crate::redact::Action::Apply => self.apply_redaction(),
            crate::redact::Action::Clear => {
                if let Some(doc) = self.documents.get_mut(self.active) {
                    doc.redact.areas.clear();
                    doc.redact.error = None;
                }
            }
            crate::redact::Action::SaveAs => self.save_redacted(),
            crate::redact::Action::Discard => self.discard_redaction(),
        }
    }

    /// Mark every occurrence of the panel's search text.
    ///
    /// Only the pages already read: the text layer arrives page by page, and a
    /// mark cannot be placed on a page nobody has looked at yet. The panel says
    /// how many were found so a search that reached nothing is visible.
    fn mark_matches(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let needle = doc.redact.find.trim().to_string();
        if needle.is_empty() {
            doc.redact.error = Some("Type something to find.".to_string());
            return;
        }

        let options = ypdf_render::SearchOptions {
            case_sensitive: false,
            whole_word: false,
        };

        let mut found = 0;
        let mut areas = Vec::new();
        for (page, analysis) in &doc.analyses {
            for hit in analysis.text.search(&needle, options) {
                for rect in analysis.text.rects_for(hit.start, hit.len) {
                    areas.push(crate::redact::Area {
                        page: *page,
                        rect: ypdf_redact::Rect::new(rect.left, rect.bottom, rect.right, rect.top),
                    });
                }
                found += 1;
            }
        }

        doc.redact.areas.extend(areas);
        doc.redact.error = if found == 0 {
            // Silence here is the dangerous answer: someone would apply an
            // empty redaction and believe the text was gone.
            Some(format!(
                "No match for {needle:?} on the pages read so far. Scroll through the \
                 document and try again."
            ))
        } else {
            None
        };
    }

    /// Remove everything marked.
    fn apply_redaction(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };

        let redactions = doc.redact.redactions();
        if redactions.is_empty() {
            return;
        }
        let settings = doc.redact.settings();

        let outcome = doc.edits.replay().and_then(|mut pdf| {
            let report = ypdf_redact::redact(&mut pdf, &redactions, &settings)?;
            Ok((report, pdf.to_bytes()?))
        });

        match outcome {
            Ok((report, bytes)) => {
                tracing::info!(
                    glyphs = report.glyphs,
                    pages = report.pages,
                    "redaction applied"
                );
                doc.redact.report = Some(report);
                doc.redact.result = Some(bytes.clone());
                doc.redact.areas.clear();
                doc.redact.marking = false;
                doc.redact.error = None;
                doc.invalidate_content();
                self.render.reopen(doc.id, bytes);
                self.status_error = None;
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                doc.redact.error = Some(e.message());
            }
        }
    }

    /// Write the redacted copy to a file the user picks.
    fn save_redacted(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let Some(bytes) = doc.redact.result.clone() else {
            return;
        };

        let stem = doc.edits.source().file_stem().map_or_else(
            || "document".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        let Some(target) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("{stem}-redacted.pdf"))
            .set_title("Save redacted PDF")
            .save_file()
        else {
            return;
        };

        match std::fs::write(&target, &bytes) {
            Ok(()) => {
                tracing::info!(path = %target.display(), "saved redacted document");
                doc.redact.result = None;
                doc.redact.report = None;
                doc.redact.open = false;
                self.status_error = None;
            }
            Err(e) => {
                let error = ypdf_core::Error::io(&target, e);
                tracing::warn!("{}", error.report().to_human());
                doc.redact.error = Some(error.message());
            }
        }
    }

    /// Throw the redacted copy away and show the document as it was.
    fn discard_redaction(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        doc.redact.result = None;
        doc.redact.report = None;

        match doc.edits.replay().and_then(|mut pdf| pdf.to_bytes()) {
            Ok(bytes) => {
                doc.invalidate_content();
                self.render.reopen(doc.id, bytes);
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                self.status_error = Some(e.message());
            }
        }
    }

    /// Read, navigate, and edit the outline (spec §17).
    fn handle_outline(&mut self, ctx: &egui::Context) {
        // The tree is read from the document as it currently stands, edits and
        // all, so a bookmark added after a page move points where it should.
        if let Some(doc) = self.documents.get_mut(self.active)
            && doc.outline.open
            && !doc.outline.loaded
        {
            match doc.edits.replay() {
                Ok(pdf) => doc.outline.load(ypdf_outline::bookmarks::read(&pdf)),
                Err(e) => {
                    tracing::warn!("{}", e.report().to_human());
                    doc.outline.error = Some(e.message());
                    doc.outline.load(Vec::new());
                }
            }
        }

        let current = self
            .documents
            .get(self.active)
            .map_or(1, |doc| u32::try_from(doc.current_page + 1).unwrap_or(1));

        let Some(action) = self
            .documents
            .get_mut(self.active)
            .and_then(|doc| doc.outline.show(ctx, current))
        else {
            return;
        };

        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };

        match action {
            crate::outline::Action::Go(page) => {
                if let Ok(index) = i32::try_from(page.saturating_sub(1)) {
                    doc.current_page = index;
                    doc.scroll_to_current = true;
                }
            }
            crate::outline::Action::Add => {
                let title = doc.outline.new_title.trim().to_string();
                if !title.is_empty() {
                    doc.outline
                        .tree
                        .push(ypdf_outline::Bookmark::new(title, current));
                    doc.outline.new_title.clear();
                    doc.outline.dirty = true;
                }
            }
            crate::outline::Action::Remove(index) => {
                doc.outline.remove_at(index);
            }
            crate::outline::Action::Rename(index, title) => {
                if let Some(bookmark) = doc.outline.at_mut(index) {
                    bookmark.title = title;
                }
                doc.outline.renaming = None;
                doc.outline.dirty = true;
            }
            crate::outline::Action::Save => self.save_outline(),
        }
    }

    /// Record the edited outline as an operation.
    ///
    /// It goes into the edit log rather than straight into a copy, so it
    /// survives the next replay, takes part in undo, and is written when the
    /// document is saved — like every other edit.
    fn save_outline(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let tree = doc.outline.tree.clone();
        let count = tree.len();

        match doc
            .edits
            .apply(crate::edit::Op::SetOutline(tree))
            .and_then(|mut pdf| pdf.to_bytes())
        {
            Ok(bytes) => {
                tracing::info!(bookmarks = count, "outline recorded");
                doc.outline.dirty = false;
                doc.outline.error = None;
                doc.invalidate_content();
                self.render.reopen(doc.id, bytes);
                self.status_error = None;
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                doc.outline.error = Some(e.message());
            }
        }
    }

    /// Read and edit the form (spec §16).
    fn handle_forms(&mut self, ctx: &egui::Context) {
        if let Some(doc) = self.documents.get_mut(self.active)
            && doc.forms.open
            && !doc.forms.loaded
        {
            match doc.edits.replay() {
                Ok(pdf) => {
                    let has_form = ypdf_forms::has_form(&pdf);
                    doc.forms.load(ypdf_forms::read(&pdf), has_form);
                }
                Err(e) => {
                    tracing::warn!("{}", e.report().to_human());
                    doc.forms.error = Some(e.message());
                    doc.forms.load(Vec::new(), false);
                }
            }
        }

        let Some(action) = self
            .documents
            .get_mut(self.active)
            .and_then(|doc| doc.forms.show(ctx))
        else {
            return;
        };

        match action {
            crate::forms::Action::Apply => self.apply_form(FormEdit::Fill),
            crate::forms::Action::Clear => self.apply_form(FormEdit::Clear),
            crate::forms::Action::Flatten => self.apply_form(FormEdit::Flatten),
            crate::forms::Action::Export => self.export_form_data(),
            crate::forms::Action::Import => self.import_form_data(),
        }
    }

    /// Fill, clear, or flatten, and show the result.
    ///
    /// The edited document is held in memory like every other destructive
    /// operation; the file on disk is untouched until it is saved.
    fn apply_form(&mut self, edit: FormEdit) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let values = doc.forms.changes();

        let outcome = doc.edits.replay().and_then(|mut pdf| {
            let report = match edit {
                FormEdit::Fill => ypdf_forms::fill(&mut pdf, &values)?,
                FormEdit::Clear => ypdf_forms::clear(&mut pdf)?,
                FormEdit::Flatten => {
                    // Whatever was typed goes in before the fields disappear,
                    // or flattening would burn in the old values.
                    let mut report = ypdf_forms::fill(&mut pdf, &values)?;
                    report.flattened = ypdf_forms::flatten(&mut pdf)?.flattened;
                    report
                }
            };
            Ok((report, pdf.to_bytes()?))
        });

        match outcome {
            Ok((report, bytes)) => {
                tracing::info!(
                    filled = report.filled,
                    cleared = report.cleared,
                    flattened = report.flattened,
                    "form edited"
                );
                doc.forms.report = Some(report);
                doc.forms.error = None;
                doc.form_result = Some(bytes.clone());
                doc.invalidate_content();
                // `invalidate_content` clears the panel, so the fields are read
                // back from the edited document on the next frame.
                self.render.reopen(doc.id, bytes);
                self.status_error = None;
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                doc.forms.error = Some(e.message());
            }
        }
    }

    /// Write the current values to a data file.
    fn export_form_data(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let values = ypdf_forms::values_of(&doc.forms.fields);

        let Some(target) = rfd::FileDialog::new()
            .add_filter("Form data", &["json", "fdf", "xfdf"])
            .set_file_name("form-data.json")
            .set_title("Export form data")
            .save_file()
        else {
            return;
        };

        let format = ypdf_forms::Format::from_path(&target);
        let outcome = ypdf_forms::export(&values, format).and_then(|text| {
            std::fs::write(&target, text).map_err(|e| ypdf_core::Error::io(&target, e))
        });

        match outcome {
            Ok(()) => {
                tracing::info!(path = %target.display(), "form data written");
                doc.forms.error = None;
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                doc.forms.error = Some(e.message());
            }
        }
    }

    /// Read values from a data file into the draft.
    ///
    /// Into the draft rather than the document: the values appear in the panel
    /// where they can be read before Apply writes them.
    fn import_form_data(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Form data", &["json", "fdf", "xfdf"])
            .set_title("Import form data")
            .pick_file()
        else {
            return;
        };

        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let format = ypdf_forms::Format::from_path(&path);

        let outcome = std::fs::read_to_string(&path)
            .map_err(|e| ypdf_core::Error::io(&path, e))
            .and_then(|text| ypdf_forms::import(&text, format));

        match outcome {
            Ok(values) => {
                let mut unknown = Vec::new();
                for (name, value) in values {
                    match doc.forms.draft.entry(name) {
                        std::collections::btree_map::Entry::Occupied(mut entry) => {
                            entry.insert(value);
                        }
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            unknown.push(entry.into_key());
                        }
                    }
                }
                // Names the form does not have are named, rather than dropped:
                // otherwise importing the wrong file looks like success.
                doc.forms.error = (!unknown.is_empty())
                    .then(|| format!("Not in this form: {}", unknown.join(", ")));
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                doc.forms.error = Some(e.message());
            }
        }
    }

    /// Drive the annotation tools and panel (spec §10).
    fn handle_annotations(&mut self, ctx: &egui::Context) {
        if let Some(doc) = self.documents.get_mut(self.active)
            && doc.annotate.open
            && !doc.annotate.loaded
        {
            match doc.edits.replay() {
                Ok(pdf) => doc.annotate.load(ypdf_annot::list(&pdf)),
                Err(e) => {
                    tracing::warn!("{}", e.report().to_human());
                    doc.annotate.error = Some(e.message());
                    doc.annotate.load(Vec::new());
                }
            }
        }

        // A text tool marks whatever the selection ended up covering.
        self.mark_selection();

        if let Some(annotation) = self
            .documents
            .get_mut(self.active)
            .and_then(|doc| doc.pending_annotation.take())
        {
            self.apply_annotation(&annotation);
        }

        let Some(action) = self
            .documents
            .get_mut(self.active)
            .and_then(|doc| doc.annotate.show(ctx))
        else {
            return;
        };

        match action {
            crate::annotate::Action::Remove(index) => self.remove_annotation(index),
            crate::annotate::Action::Clear => self.clear_annotations(),
            crate::annotate::Action::Go(page) => {
                if let Some(doc) = self.documents.get_mut(self.active)
                    && let Ok(index) = i32::try_from(page.saturating_sub(1))
                {
                    doc.current_page = index;
                    doc.scroll_to_current = true;
                }
            }
        }
    }

    /// Mark the current selection, when a text tool is in hand.
    fn mark_selection(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        if !doc.annotate.tool.marks_text() || doc.selecting {
            return;
        }
        let Some(selection) = doc.selection.as_ref() else {
            return;
        };

        let (start, len) = selection.range();
        if len == 0 {
            return;
        }
        let Some(analysis) = doc.analyses.get(&selection.page) else {
            return;
        };
        let Ok(page) = u32::try_from(selection.page) else {
            return;
        };

        let quads: Vec<[f32; 4]> = analysis
            .text
            .rects_for(start, len)
            .into_iter()
            .map(|rect| [rect.left, rect.bottom, rect.right, rect.top])
            .collect();

        let Some(annotation) = doc.annotate.mark_text(page + 1, quads) else {
            return;
        };
        // Cleared first, so releasing the pointer marks the words once rather
        // than on every frame the selection survives.
        doc.selection = None;
        self.apply_annotation(&annotation);
    }

    /// Add one annotation to the document.
    fn apply_annotation(&mut self, annotation: &ypdf_annot::Annotation) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };

        let outcome = doc.edits.replay().and_then(|mut pdf| {
            ypdf_annot::add(&mut pdf, annotation)?;
            pdf.to_bytes()
        });

        match outcome {
            Ok(bytes) => {
                tracing::info!(kind = annotation.shape.label(), "annotation added");
                doc.annotate.error = None;
                doc.annotation_bytes = Some(bytes.clone());
                doc.invalidate_content();
                self.render.reopen(doc.id, bytes);
                self.status_error = None;
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                doc.annotate.error = Some(e.message());
            }
        }
    }

    /// Remove the annotation at a position in the panel's list.
    fn remove_annotation(&mut self, index: usize) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };
        let Some(target) = doc.annotate.items.get(index).cloned() else {
            return;
        };

        let mut removed_one = false;
        let outcome = doc.edits.replay().and_then(|mut pdf| {
            ypdf_annot::remove_where(&mut pdf, |annotation| {
                // The first exact match, so two identical marks do not both go.
                if !removed_one && *annotation == target {
                    removed_one = true;
                    return true;
                }
                false
            });
            pdf.to_bytes()
        });

        match outcome {
            Ok(bytes) => {
                doc.annotation_bytes = Some(bytes.clone());
                doc.invalidate_content();
                self.render.reopen(doc.id, bytes);
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                doc.annotate.error = Some(e.message());
            }
        }
    }

    /// Remove every annotation, leaving form fields and links alone.
    fn clear_annotations(&mut self) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            return;
        };

        let outcome = doc.edits.replay().and_then(|mut pdf| {
            let removed = ypdf_annot::remove_where(&mut pdf, |_| true);
            tracing::info!(removed, "annotations cleared");
            pdf.to_bytes()
        });

        match outcome {
            Ok(bytes) => {
                doc.annotation_bytes = Some(bytes.clone());
                doc.invalidate_content();
                self.render.reopen(doc.id, bytes);
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                doc.annotate.error = Some(e.message());
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top(egui::Id::new("top_bar")).show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.label(egui::RichText::new("yPDF").strong());
                ui.separator();
                ui.toggle_value(&mut self.show_thumbnails, "Thumbnails");
                if ui.button("Open…").clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("PDF", &["pdf"])
                        .set_title("Open a PDF")
                        .pick_file()
                {
                    self.open(path);
                }
                if let Some(doc) = self.documents.get_mut(self.active) {
                    let (label, severity) = doc.inspection.security_badge();
                    let text = if doc.inspection.loaded {
                        egui::RichText::new(format!("Inspect · {label}"))
                            .color(inspect::severity_colour(ui, severity))
                    } else {
                        egui::RichText::new("Inspect")
                    };
                    ui.toggle_value(&mut doc.inspection.open, text)
                        .on_hover_text("Metadata, diagnostics, and the security scan");
                }
                if let Some(doc) = self.documents.get_mut(self.active) {
                    ui.toggle_value(&mut doc.compress.open, "Compress")
                        .on_hover_text("Reduce the file size (spec §4)");
                    ui.toggle_value(&mut doc.protect.open, "Protect")
                        .on_hover_text("Password and permissions (spec §8)");
                    ui.toggle_value(&mut doc.ocr.open, "OCR")
                        .on_hover_text("Make a scan searchable (spec §7)");
                    ui.toggle_value(&mut doc.watermark.open, "Watermark")
                        .on_hover_text("Stamp the pages (spec §12)");
                    ui.toggle_value(&mut doc.redact.open, "Redact")
                        .on_hover_text("Remove content permanently (spec §11)");
                    ui.toggle_value(&mut doc.outline.open, "Bookmarks")
                        .on_hover_text("The document outline (spec §17)");
                    ui.toggle_value(&mut doc.forms.open, "Form")
                        .on_hover_text("Fill in the form fields (spec §16)");
                    ui.toggle_value(&mut doc.export.open, "Export")
                        .on_hover_text("Convert the text to Markdown or Word (spec §5)");
                    ui.toggle_value(&mut doc.annotate.open, "Annotate")
                        .on_hover_text("Highlight, comment, and draw (spec §10)");
                }
                ui.separator();
                self.edit_controls(ui);
                ui.separator();
                self.view_controls(ui);
            });
            if self.documents.len() > 1 || !self.documents.is_empty() {
                self.tab_strip(ui);
            }
        });
    }

    fn view_controls(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.documents.get_mut(self.active) else {
            ui.weak("No document");
            return;
        };

        if ui.button("−").on_hover_text("Zoom out (−)").clicked() {
            doc.set_zoom(Zoom::zoom_out(doc.scale));
        }
        #[expect(clippy::cast_possible_truncation, reason = "display only")]
        let percent = (doc.scale * 100.0).round() as i32;
        ui.label(format!("{percent}%"));
        if ui.button("+").on_hover_text("Zoom in (+)").clicked() {
            doc.set_zoom(Zoom::zoom_in(doc.scale));
        }

        ui.separator();
        if ui
            .selectable_label(doc.zoom == Zoom::FitWidth, "Fit width")
            .clicked()
        {
            doc.set_zoom(Zoom::FitWidth);
        }
        if ui
            .selectable_label(doc.zoom == Zoom::FitPage, "Fit page")
            .clicked()
        {
            doc.set_zoom(Zoom::FitPage);
        }
        if ui
            .button("Find")
            .on_hover_text("Search this document (Ctrl+F)")
            .clicked()
        {
            doc.search.open = true;
        }
        if ui
            .button("Rotate")
            .on_hover_text("Rotate 90° clockwise (R)")
            .clicked()
        {
            doc.rotate();
        }
        let mut invert = doc.invert;
        if ui
            .toggle_value(&mut invert, "Invert")
            .on_hover_text("Invert page colours for dark reading (I)")
            .changed()
        {
            doc.toggle_invert();
        }

        ui.separator();
        let count = doc.page_count();
        let mut page_number = doc.current_page + 1;
        let response = ui.add(
            egui::DragValue::new(&mut page_number)
                .range(1..=count.max(1))
                .speed(0.25),
        );
        if response.changed() {
            doc.go_to(page_number - 1);
        }
        ui.label(format!("/ {count}"));
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let mut close = None;
        ui.horizontal(|ui| {
            for (index, doc) in self.documents.iter().enumerate() {
                let selected = index == self.active;
                if ui.selectable_label(selected, doc.title()).clicked() {
                    self.active = index;
                }
                if selected && ui.small_button("x").clicked() {
                    close = Some(index);
                }
            }
        });
        if let Some(index) = close {
            self.close(index);
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom(egui::Id::new("status_bar")).show(ui, |ui| {
            ui.horizontal(|ui| {
                match (&self.status_error, self.documents.get(self.active)) {
                    (Some(error), _) => {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                    (None, Some(doc)) => {
                        ui.label(format!(
                            "Page {} of {}",
                            doc.current_page + 1,
                            doc.page_count()
                        ));
                        if doc.edits.password().is_some() {
                            ui.separator();
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                "Protected — saving writes an unprotected copy",
                            );
                        }
                        if doc.annotation_bytes.is_some() {
                            ui.separator();
                            ui.colored_label(ui.visuals().warn_fg_color, "Annotated — not saved");
                        }
                        if doc.form_result.is_some() {
                            ui.separator();
                            ui.colored_label(ui.visuals().warn_fg_color, "Form edited — not saved");
                        }
                        if doc.redact.result.is_some() {
                            ui.separator();
                            ui.colored_label(ui.visuals().warn_fg_color, "Redacted — not saved");
                        }
                        if doc.watermark.result.is_some() {
                            ui.separator();
                            ui.colored_label(ui.visuals().warn_fg_color, "Watermarked — not saved");
                        }
                        if doc.ocr.result.is_some() {
                            ui.separator();
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                "Text layer added — not saved",
                            );
                        }
                        if doc.compressed.is_some() {
                            ui.separator();
                            ui.colored_label(ui.visuals().warn_fg_color, "Compressed — not saved");
                        }
                        if doc.edits.is_dirty() {
                            ui.separator();
                            ui.colored_label(ui.visuals().warn_fg_color, "Unsaved edits");
                        }
                        if !doc.selected_pages.is_empty() {
                            ui.separator();
                            ui.label(format!("{} selected", doc.selected_pages.len()));
                        }
                        ui.separator();
                        ui.weak(format!(
                            "{} rasters · {:.0} MB",
                            doc.cache.len(),
                            doc.cache.bytes() as f64 / (1024.0 * 1024.0)
                        ));
                    }
                    (None, None) => {
                        ui.label("Ready");
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!("{} workers", self.config.engine.workers));
                });
            });
        });
    }

    fn body(&mut self, ui: &mut egui::Ui) {
        if self.documents.is_empty() {
            let mut open = None;
            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.25);
                    ui.heading("Drop a PDF here");
                    ui.add_space(4.0);
                    ui.weak("Local-first. Nothing leaves this machine.");

                    let entries = self.recent.entries();
                    if !entries.is_empty() {
                        ui.add_space(24.0);
                        ui.label(egui::RichText::new("Recent").strong());
                        ui.add_space(4.0);
                        for path in entries {
                            let name = path.file_name().map_or_else(
                                || path.display().to_string(),
                                |n| n.to_string_lossy().into_owned(),
                            );
                            if ui
                                .link(name)
                                .on_hover_text(path.display().to_string())
                                .clicked()
                            {
                                open = Some(path.clone());
                            }
                        }
                    }
                });
            });
            if let Some(path) = open {
                self.open(path);
            }
            return;
        }

        let active = self.active;
        let render = &self.render;
        let mut page_move = None;

        if self.show_thumbnails {
            egui::Panel::left(egui::Id::new("thumbnails"))
                .default_size(thumbnails::preferred_width())
                .show(ui, |ui| {
                    if let Some(doc) = self.documents.get_mut(active) {
                        page_move = thumbnails::show(ui, doc, render);
                    }
                });
        }

        let mut apply_metadata = false;
        if self
            .documents
            .get(active)
            .is_some_and(|d| d.inspection.open)
        {
            egui::Panel::right(egui::Id::new("inspect"))
                .default_size(inspect::preferred_width())
                .show(ui, |ui| {
                    if let Some(doc) = self.documents.get_mut(active) {
                        apply_metadata = inspect::show(ui, &mut doc.inspection);
                    }
                });
        }

        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(doc) = self.documents.get_mut(active) {
                viewer::show(ui, doc, render);
            }
        });

        if apply_metadata {
            self.apply_metadata();
        }

        if let Some(moved) = page_move {
            self.apply_op(Op::Move {
                from: moved.from,
                to: moved.to,
            });
        }
    }
}

impl eframe::App for YpdfApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.take_render_events(&ctx);
        self.take_dropped_files(&ctx);
        self.take_shortcuts(&ctx);

        self.refresh_inspection();

        self.handle_compression(&ctx);
        self.handle_split(&ctx);
        self.handle_merge(&ctx);
        self.handle_export(&ctx);
        self.handle_password_prompt(&ctx);
        self.handle_ocr(&ctx);
        self.handle_watermark(&ctx);
        self.handle_redaction(&ctx);
        self.handle_outline(&ctx);
        self.handle_forms(&ctx);
        self.handle_annotations(&ctx);
        self.handle_protect(&ctx);

        self.top_bar(ui);
        self.search_bar(ui);
        self.status_bar(ui);
        self.body(ui);
    }

    fn on_exit(&mut self) {
        tracing::info!("yPDF shutting down");
    }
}

impl std::fmt::Debug for YpdfApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YpdfApp")
            .field("documents", &self.documents.len())
            .field("active", &self.active)
            .finish()
    }
}

/// Page counts cross from `ypdf-doc`'s `u32` to the renderer's signed index.
///
/// A document with more pages than an `i32` can hold does not exist, but
/// saturating is still better than a panic in the frame loop.
fn page_index(count: u32) -> ypdf_render::PageIndex {
    ypdf_render::PageIndex::try_from(count).unwrap_or(ypdf_render::PageIndex::MAX)
}

/// Which file an export is being written as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Markdown,
    Docx,
}

impl Format {
    const fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Docx => "docx",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Markdown => "Markdown",
            Self::Docx => "Word document",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Markdown => "Save Markdown",
            Self::Docx => "Save Word document",
        }
    }
}
