//! OCR in the viewer (spec §7).
//!
//! Recognition is slow — a few hundred milliseconds a page at best — and
//! rasterizing has to go through the one render thread PDFium allows. So the
//! work is split three ways and none of it happens on the UI thread:
//!
//! * the **render thread** rasterizes one page at a time, as it already does
//!   for the viewer;
//! * a **worker thread** runs Tesseract and builds the text layer;
//! * the **UI thread** does nothing but pass rasters along and draw progress.
//!
//! The result is held in memory until the user saves it, like compression. A
//! text layer is an addition rather than a loss, but it is still a change to
//! someone's document, and it is not made to their file without being asked.

use std::path::PathBuf;

use crossbeam_channel::{Receiver, Sender};
use egui::{Context, Ui};
use ypdf_core::{Error, Result};
use ypdf_doc::Pdf;
use ypdf_ocr::{OcrSettings, Tesseract};

/// One rasterized page on its way to the worker.
struct Raster {
    page: u32,
    rgba: Vec<u8>,
    size: (u32, u32),
}

/// What a finished run produced.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    /// Pages recognized.
    pub pages: usize,
    /// Pages left alone because they already had text.
    pub skipped: usize,
    /// Words placed in the text layer.
    pub words: usize,
    /// Words dropped because the layer font cannot encode them.
    pub unencodable: usize,
    /// Mean confidence over the pages that were recognized.
    pub confidence: f32,
}

/// A run in progress.
pub struct Job {
    /// Pages still to rasterize, in order.
    pages: Vec<u32>,
    /// How many have been sent to the worker.
    cursor: usize,
    /// The page the render thread is currently working on.
    awaiting: Option<u32>,
    /// Pixel width asked for, so a viewer raster is not mistaken for ours.
    width: u32,
    /// Rasters going out.
    tx: Option<Sender<Raster>>,
    /// The finished document coming back.
    rx: Receiver<Result<(Vec<u8>, Summary)>>,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("pages", &self.pages.len())
            .field("cursor", &self.cursor)
            .field("awaiting", &self.awaiting)
            .finish_non_exhaustive()
    }
}

impl Job {
    /// How far along it is, as a fraction.
    #[must_use]
    pub fn progress(&self) -> f32 {
        if self.pages.is_empty() {
            return 1.0;
        }
        #[expect(clippy::cast_precision_loss, reason = "page counts are small")]
        let fraction = self.cursor as f32 / self.pages.len() as f32;
        fraction
    }

    /// The page currently being worked on, 1-based.
    #[must_use]
    pub fn current_page(&self) -> Option<u32> {
        self.awaiting.map(|page| page + 1)
    }
}

/// The OCR dialog and whatever run it is driving.
#[derive(Debug)]
pub struct OcrDialog {
    /// Is it showing?
    pub open: bool,
    /// Tesseract language specification.
    pub language: String,
    /// Resolution to rasterize at.
    pub dpi: u32,
    /// Recognize pages that already have text.
    pub force: bool,
    /// The run in progress.
    pub job: Option<Job>,
    /// The finished document, waiting to be saved.
    pub result: Option<Vec<u8>>,
    /// What the finished run produced.
    pub summary: Option<Summary>,
    /// Why the last attempt failed.
    pub error: Option<String>,
}

impl Default for OcrDialog {
    fn default() -> Self {
        Self {
            open: false,
            language: "eng".to_string(),
            // 300 is the usual sweet spot: below about 200 accuracy falls off,
            // above 400 the extra pixels mostly cost time.
            dpi: 300,
            force: false,
            job: None,
            result: None,
            summary: None,
            error: None,
        }
    }
}

/// What the dialog is asking for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Start recognizing.
    Run,
    /// Keep the result and write it somewhere.
    SaveAs,
    /// Throw it away.
    Discard,
    /// Stop a run in progress.
    Cancel,
}

impl OcrDialog {
    /// Is a run in progress?
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// Settings as they stand.
    #[must_use]
    pub fn settings(&self) -> OcrSettings {
        OcrSettings {
            language: self.language.clone(),
            min_confidence: 40.0,
            skip_pages_with_text: !self.force,
        }
    }

    /// Draw it.
    pub fn show(&mut self, ctx: &Context) -> Option<Action> {
        if !self.open {
            return None;
        }

        let mut action = None;
        let mut open = self.open;

        egui::Window::new("OCR")
            .open(&mut open)
            .resizable(false)
            .default_width(400.0)
            .show(ctx, |ui| {
                action = self.body(ui);
            });

        self.open = open;
        action
    }

    fn body(&mut self, ui: &mut Ui) -> Option<Action> {
        let mut action = None;

        if let Some(job) = &self.job {
            ui.horizontal(|ui| {
                ui.spinner();
                match job.current_page() {
                    Some(page) => ui.label(format!("Reading page {page}…")),
                    None => ui.label("Finishing…"),
                };
            });
            ui.add(egui::ProgressBar::new(job.progress()).show_percentage());
            ui.add_space(6.0);
            if ui.button("Stop").clicked() {
                action = Some(Action::Cancel);
            }
            return action;
        }

        if let Some(error) = &self.error {
            // A missing Tesseract prints every path it looked in, so the box
            // has to be able to show more than one line of it.
            egui::ScrollArea::vertical()
                .max_height(160.0)
                .show(ui, |ui| {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                });
            ui.add_space(6.0);
        }

        if let Some(summary) = &self.summary {
            self.results(ui, &summary.clone(), &mut action);
            return action;
        }

        self.controls(ui);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Read the pages").clicked() {
                action = Some(Action::Run);
            }
            ui.weak("The scan is not changed; the text goes in an invisible layer.");
        });

        action
    }

    fn controls(&mut self, ui: &mut Ui) {
        egui::Grid::new("ocr-settings")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Language");
                ui.add(
                    egui::TextEdit::singleline(&mut self.language)
                        .hint_text("eng, or eng+msa")
                        .desired_width(200.0),
                )
                .on_hover_text("Tesseract language codes, joined with +");
                ui.end_row();

                ui.label("Resolution");
                ui.add(egui::Slider::new(&mut self.dpi, 150..=600).suffix(" dpi"))
                    .on_hover_text("300 is the usual sweet spot");
                ui.end_row();
            });

        ui.checkbox(&mut self.force, "Read pages that already have text")
            .on_hover_text(
                "Off by default: laying a guess over real text gives search two answers",
            );
    }

    fn results(&self, ui: &mut Ui, summary: &Summary, action: &mut Option<Action>) {
        ui.label(format!(
            "{} words over {} page(s), {:.0}% confidence",
            summary.words, summary.pages, summary.confidence
        ));
        if summary.skipped > 0 {
            ui.weak(format!(
                "{} page(s) already had text and were left alone.",
                summary.skipped
            ));
        }
        if summary.unencodable > 0 {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "{} word(s) could not be written: the text layer font covers Latin scripts only.",
                    summary.unencodable
                ),
            );
        }
        if summary.pages > 0 && summary.words == 0 {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Nothing was recognized. Check the language and the scan quality.",
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

    /// Start a run over `pages` of `source`.
    ///
    /// The worker owns its own copy of the document: the UI thread keeps
    /// drawing while it works, and a half-finished text layer must never be
    /// something the viewer can show.
    pub fn start(
        &mut self,
        source: Pdf,
        pages: Vec<u32>,
        width: u32,
        tesseract: Option<PathBuf>,
    ) -> Result<()> {
        let engine = match tesseract {
            Some(path) => Tesseract::at(path),
            None => Tesseract::find()?,
        };
        engine.check_languages(&self.language)?;

        let (tx_raster, rx_raster) = crossbeam_channel::bounded::<Raster>(2);
        let (tx_done, rx_done) = crossbeam_channel::bounded(1);
        let settings = self.settings();

        std::thread::spawn(move || {
            let outcome = recognize(engine, source, &rx_raster, &settings);
            let _ = tx_done.send(outcome);
        });

        self.job = Some(Job {
            pages,
            cursor: 0,
            awaiting: None,
            width,
            tx: Some(tx_raster),
            rx: rx_done,
        });
        self.error = None;
        self.summary = None;
        self.result = None;
        Ok(())
    }

    /// The next page to rasterize, if the run is waiting for one.
    pub fn next_page(&mut self) -> Option<(u32, u32)> {
        let job = self.job.as_mut()?;
        if job.awaiting.is_some() {
            return None;
        }
        let page = *job.pages.get(job.cursor)?;
        job.awaiting = Some(page);
        Some((page, job.width))
    }

    /// Hand a rasterized page to the worker.
    ///
    /// Returns true when the raster was one this run asked for, in which case
    /// the viewer should not treat it as a texture.
    pub fn accept_raster(&mut self, page: u32, width: u32, rgba: Vec<u8>, height: u32) -> bool {
        let Some(job) = self.job.as_mut() else {
            return false;
        };
        if job.awaiting != Some(page) || width != job.width {
            return false;
        }

        if let Some(tx) = &job.tx {
            // A full channel means the worker is behind; blocking here would
            // freeze the UI, so the page waits for the next frame instead.
            if tx
                .try_send(Raster {
                    page,
                    rgba,
                    size: (width, height),
                })
                .is_err()
            {
                return true;
            }
        }

        job.awaiting = None;
        job.cursor += 1;

        if job.cursor >= job.pages.len() {
            // Closing the channel is how the worker knows to finish.
            job.tx = None;
        }
        true
    }

    /// Collect the finished document, if the worker has produced one.
    pub fn poll(&mut self) {
        let Some(job) = self.job.as_ref() else {
            return;
        };
        let Ok(outcome) = job.rx.try_recv() else {
            return;
        };

        self.job = None;
        match outcome {
            Ok((bytes, summary)) => {
                self.result = Some(bytes);
                self.summary = Some(summary);
            }
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                self.error = Some(e.message());
            }
        }
    }

    /// Abandon a run in progress.
    pub fn cancel(&mut self) {
        // Dropping the sender ends the worker's loop; its result is ignored.
        self.job = None;
    }
}

/// The worker: recognize every raster that arrives, then serialize.
fn recognize(
    engine: Tesseract,
    mut pdf: Pdf,
    rasters: &Receiver<Raster>,
    settings: &OcrSettings,
) -> Result<(Vec<u8>, Summary)> {
    let mut summary = Summary::default();
    let mut confidence_total = 0.0_f32;

    while let Ok(raster) = rasters.recv() {
        let report = ypdf_ocr::ocr_page(
            &engine,
            &mut pdf,
            raster.page,
            &raster.rgba,
            raster.size,
            settings,
        )?;

        if report.skipped_has_text {
            summary.skipped += 1;
            continue;
        }
        summary.pages += 1;
        summary.words += report.words;
        summary.unencodable += report.unencodable;
        confidence_total += report.confidence;
    }

    if summary.pages > 0 {
        #[expect(clippy::cast_precision_loss, reason = "page counts are small")]
        let pages = summary.pages as f32;
        summary.confidence = confidence_total / pages;
    }

    if summary.pages == 0 && summary.skipped == 0 {
        return Err(Error::Cancelled);
    }

    let bytes = pdf.to_bytes()?;
    Ok((bytes, summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_dialog_is_closed_and_does_not_overwrite_real_text() {
        let dialog = OcrDialog::default();
        assert!(!dialog.open);
        assert!(!dialog.force);
        assert!(dialog.settings().skip_pages_with_text);
        assert_eq!(dialog.dpi, 300);
    }

    #[test]
    fn forcing_turns_off_the_skip() {
        let dialog = OcrDialog {
            force: true,
            ..OcrDialog::default()
        };
        assert!(!dialog.settings().skip_pages_with_text);
    }

    #[test]
    fn nothing_is_asked_for_when_no_run_is_going() {
        let mut dialog = OcrDialog::default();
        assert!(dialog.next_page().is_none());
        assert!(!dialog.accept_raster(0, 100, Vec::new(), 100));
    }

    #[test]
    fn a_raster_for_another_purpose_is_not_taken() {
        // The viewer is rendering the same pages at its own widths; taking one
        // of those would recognize a thumbnail and skip a page.
        let (tx, _rx_raster) = crossbeam_channel::bounded(2);
        let (_tx_done, rx) = crossbeam_channel::bounded(1);
        let mut dialog = OcrDialog {
            job: Some(Job {
                pages: vec![0, 1],
                cursor: 0,
                awaiting: Some(0),
                width: 2550,
                tx: Some(tx),
                rx,
            }),
            ..OcrDialog::default()
        };

        assert!(
            !dialog.accept_raster(0, 800, vec![0; 4], 100),
            "wrong width"
        );
        assert!(
            !dialog.accept_raster(1, 2550, vec![0; 4], 100),
            "wrong page"
        );
        assert!(dialog.accept_raster(0, 2550, vec![0; 4], 100), "ours");
    }

    #[test]
    fn pages_are_asked_for_one_at_a_time_and_in_order() {
        let (tx, _rx_raster) = crossbeam_channel::bounded(2);
        let (_tx_done, rx) = crossbeam_channel::bounded(1);
        let mut dialog = OcrDialog {
            job: Some(Job {
                pages: vec![3, 4],
                cursor: 0,
                awaiting: None,
                width: 100,
                tx: Some(tx),
                rx,
            }),
            ..OcrDialog::default()
        };

        assert_eq!(dialog.next_page(), Some((3, 100)));
        assert_eq!(dialog.next_page(), None, "one at a time");

        dialog.accept_raster(3, 100, vec![0; 4], 10);
        assert_eq!(dialog.next_page(), Some((4, 100)));
    }

    #[test]
    fn the_last_page_closes_the_channel_so_the_worker_finishes() {
        let (tx, rx_raster) = crossbeam_channel::bounded(2);
        let (_tx_done, rx) = crossbeam_channel::bounded(1);
        let mut dialog = OcrDialog {
            job: Some(Job {
                pages: vec![0],
                cursor: 0,
                awaiting: Some(0),
                width: 100,
                tx: Some(tx),
                rx,
            }),
            ..OcrDialog::default()
        };

        dialog.accept_raster(0, 100, vec![0; 4], 10);
        drop(dialog);

        // Draining what was sent leaves the channel closed, which is the signal
        // the worker waits for.
        assert!(rx_raster.recv().is_ok());
        assert!(
            rx_raster.recv().is_err(),
            "the sender must have been dropped"
        );
    }

    #[test]
    fn progress_runs_from_zero_to_one() {
        let (tx, _rx_raster) = crossbeam_channel::bounded(2);
        let (_tx_done, rx) = crossbeam_channel::bounded(1);
        let job = Job {
            pages: vec![0, 1, 2, 3],
            cursor: 1,
            awaiting: Some(1),
            width: 100,
            tx: Some(tx),
            rx,
        };
        assert!((job.progress() - 0.25).abs() < f32::EPSILON);
        assert_eq!(job.current_page(), Some(2), "shown 1-based");
    }
}
