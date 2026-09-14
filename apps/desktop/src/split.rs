//! The split dialog (spec §3.2).
//!
//! Splitting is the one page operation that leaves the open document alone: it
//! reads what is on screen and writes several new files beside it. That is why
//! it is not an [`crate::edit::Op`] and never enters the undo log — there is
//! nothing about the open document to undo.
//!
//! The pieces are cut from the document *as edited*, not from the file on disk,
//! so a rotation or a deletion made a moment ago is in them.

use std::path::{Path, PathBuf};

use egui::{Context, Ui};
use ypdf_core::{Error, Result};
use ypdf_doc::{PageSpec, Piece, SplitMode};

/// Which way to cut, as the dialog offers it.
///
/// A plain enum rather than [`SplitMode`] itself: the radio buttons have to
/// stay selected while the text beside them is still half-typed and would not
/// parse.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// One file per page.
    #[default]
    EachPage,
    /// One file per fixed-size run of pages.
    EveryN,
    /// One file per range in the expression.
    Ranges,
    /// One file per top-level bookmark.
    Bookmarks,
}

/// One file the last run wrote.
#[derive(Clone, Debug)]
pub struct Written {
    /// Where it landed.
    pub path: PathBuf,
    /// How many pages it holds.
    pub pages: usize,
}

/// State of the split dialog.
#[derive(Debug)]
pub struct SplitDialog {
    /// Is it showing?
    pub open: bool,
    /// Which way to cut.
    pub mode: Mode,
    /// Pages per piece for [`Mode::EveryN`].
    pub every: u32,
    /// Range expression for [`Mode::Ranges`], e.g. `1-4,5-8`.
    pub ranges: String,
    /// What the last run wrote, in order.
    pub written: Vec<Written>,
    /// Why the last run failed, or why the settings cannot be used.
    pub error: Option<String>,
}

impl Default for SplitDialog {
    fn default() -> Self {
        Self {
            open: false,
            mode: Mode::default(),
            // Two is the smallest split that is not "every page", and a size
            // the user will almost always change anyway.
            every: 2,
            ranges: String::new(),
            written: Vec::new(),
            error: None,
        }
    }
}

/// What the dialog is asking the application to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Cut the document up and write the pieces to a folder the user picks.
    Run,
}

impl SplitDialog {
    /// Draw the dialog. Returns what the user asked for, if anything.
    ///
    /// `pages` is the page count of the open document, used for the preview
    /// line and to keep the settings inside what the document can answer.
    pub fn show(&mut self, ctx: &Context, pages: u32) -> Option<Action> {
        if !self.open {
            return None;
        }

        let mut action = None;
        let mut open = self.open;

        egui::Window::new("Split")
            .open(&mut open)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                self.modes(ui, pages);
                ui.separator();
                ui.weak(self.preview(pages));
                ui.add_space(6.0);
                action = self.controls(ui, pages);
                self.results(ui);
            });

        self.open = open;
        action
    }

    fn modes(&mut self, ui: &mut Ui, pages: u32) {
        let mut changed = false;

        changed |= ui
            .radio_value(&mut self.mode, Mode::EachPage, "One file per page")
            .clicked();

        ui.horizontal(|ui| {
            changed |= ui
                .radio_value(&mut self.mode, Mode::EveryN, "Every")
                .clicked();
            let enabled = self.mode == Mode::EveryN;
            // The top of the range is the document itself: a piece larger than
            // the document is the document, which is not a split.
            let most = pages.max(1);
            changed |= ui
                .add_enabled(
                    enabled,
                    egui::DragValue::new(&mut self.every).range(1..=most),
                )
                .changed();
            ui.label("pages");
        });

        ui.horizontal(|ui| {
            changed |= ui
                .radio_value(&mut self.mode, Mode::Ranges, "Ranges")
                .clicked();
            let enabled = self.mode == Mode::Ranges;
            changed |= ui
                .add_enabled(
                    enabled,
                    egui::TextEdit::singleline(&mut self.ranges)
                        .hint_text("1-4,5-8")
                        .desired_width(180.0),
                )
                .changed();
        });

        changed |= ui
            .radio_value(
                &mut self.mode,
                Mode::Bookmarks,
                "One file per top-level bookmark",
            )
            .clicked();

        // Both the file list and the refusal describe a run made under other
        // settings. Leaving either on screen next to changed settings invites
        // reading it as the answer to the new ones.
        if changed {
            self.written.clear();
            self.error = None;
        }
    }

    /// A sentence saying what pressing Split would produce.
    fn preview(&self, pages: u32) -> String {
        if pages == 0 {
            return "Nothing is open.".to_string();
        }
        match self.mode {
            Mode::EachPage => format!("{pages} pages, one file each"),
            Mode::EveryN => {
                let n = self.every.max(1);
                let files = pages.div_ceil(n);
                let last = pages % n;
                let tail = if last == 0 {
                    String::new()
                } else {
                    format!(", the last holding {last}")
                };
                format!("{pages} pages into {files} files of {n}{tail}")
            }
            Mode::Ranges => match self.specs() {
                Ok(specs) => format!("{pages} pages into {} files", specs.len()),
                // Not an error yet: the expression is still being typed.
                Err(_) => "Ranges such as 1-4,5-8. Pages left out are not written.".to_string(),
            },
            Mode::Bookmarks => {
                "One file per top-level bookmark. A document without an outline stays whole."
                    .to_string()
            }
        }
    }

    fn controls(&mut self, ui: &mut Ui, pages: u32) -> Option<Action> {
        let mut action = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(pages > 0, egui::Button::new("Split…"))
                .on_hover_text("Choose a folder for the pieces")
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

        action
    }

    fn results(&mut self, ui: &mut Ui) {
        if self.written.is_empty() {
            return;
        }

        ui.separator();
        ui.label(format!("Wrote {} files:", self.written.len()));
        egui::ScrollArea::vertical()
            .max_height(160.0)
            .show(ui, |ui| {
                for piece in &self.written {
                    let name = piece.path.file_name().map_or_else(
                        || piece.path.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    ui.horizontal(|ui| {
                        ui.label(name);
                        ui.weak(format!("{} page(s)", piece.pages));
                    });
                }
            });
    }

    /// The range expression, parsed.
    fn specs(&self) -> Result<Vec<PageSpec>> {
        let specs = self
            .ranges
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PageSpec::parse)
            .collect::<Result<Vec<_>>>()?;
        if specs.is_empty() {
            return Err(Error::InvalidPageRange {
                spec: self.ranges.clone(),
            });
        }
        Ok(specs)
    }

    /// Turn the settings into something the engine can act on.
    ///
    /// Errors here are the user's settings, not a failure of the document, so
    /// they belong in the dialog rather than the status bar.
    pub fn resolve(&self) -> Result<SplitMode> {
        match self.mode {
            Mode::EachPage => Ok(SplitMode::EachPage),
            Mode::EveryN => {
                if self.every == 0 {
                    return Err(Error::InvalidPageRange {
                        spec: "a piece needs at least one page".into(),
                    });
                }
                Ok(SplitMode::EveryN(self.every))
            }
            Mode::Ranges => Ok(SplitMode::Ranges(self.specs()?)),
            Mode::Bookmarks => Ok(SplitMode::Bookmarks),
        }
    }
}

/// Write the pieces of a split into `folder`, named after `stem`.
///
/// Returns what it managed to write and, if it stopped early, why. Pieces are
/// written in order and the first failure ends the run: the files already
/// written stay, because half a split the user can see is more useful than a
/// folder silently rolled back.
///
/// Nothing is overwritten. Piece names come from page numbers, so a second run
/// with different settings would land on the first run's names and quietly
/// replace its work.
///
/// The naming — `{stem}-{piece name}.pdf` — is the CLI's, so a document split
/// here and the same document split by `ypdf-cli split` produce the same set of
/// files.
pub fn write_pieces(
    folder: &Path,
    stem: &str,
    pieces: Vec<Piece>,
) -> (Vec<Written>, Option<String>) {
    let mut written = Vec::with_capacity(pieces.len());

    for mut piece in pieces {
        let target = folder.join(format!("{stem}-{}.pdf", piece.name));
        if target.exists() {
            return (
                written,
                Some(format!(
                    "{} is already there — stopped rather than replace it.",
                    target.display()
                )),
            );
        }

        let bytes = match piece.pdf.to_bytes() {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!("{}", e.report().to_human());
                return (written, Some(e.message()));
            }
        };

        if let Err(e) = std::fs::write(&target, &bytes) {
            let error = Error::io(&target, e);
            tracing::warn!("{}", error.report().to_human());
            return (written, Some(error.message()));
        }

        written.push(Written {
            path: target,
            pages: piece.pages.len(),
        });
    }

    (written, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_dialog_is_closed_and_cuts_every_page() {
        let dialog = SplitDialog::default();
        assert!(!dialog.open);
        assert!(dialog.written.is_empty());
        assert_eq!(dialog.resolve().expect("mode"), SplitMode::EachPage);
    }

    #[test]
    fn every_n_carries_the_size_through() {
        let mut dialog = SplitDialog {
            mode: Mode::EveryN,
            every: 4,
            ..SplitDialog::default()
        };
        assert_eq!(dialog.resolve().expect("mode"), SplitMode::EveryN(4));

        dialog.every = 0;
        assert!(
            dialog.resolve().is_err(),
            "a piece of no pages is not a piece"
        );
    }

    #[test]
    fn ranges_are_parsed_and_an_empty_expression_is_refused() {
        let mut dialog = SplitDialog {
            mode: Mode::Ranges,
            ranges: "1-4, 5-8".to_string(),
            ..SplitDialog::default()
        };
        match dialog.resolve().expect("mode") {
            SplitMode::Ranges(specs) => assert_eq!(specs.len(), 2),
            other => panic!("expected ranges, got {other:?}"),
        }

        dialog.ranges = "  ,  ".to_string();
        assert!(dialog.resolve().is_err(), "no range is not every range");
    }

    #[test]
    fn the_preview_counts_the_files_that_would_be_written() {
        let dialog = SplitDialog {
            mode: Mode::EveryN,
            every: 4,
            ..SplitDialog::default()
        };
        assert_eq!(dialog.preview(12), "12 pages into 3 files of 4");
        // A short last piece is worth saying out loud: it is the one thing
        // about a fixed-size split that surprises people.
        assert_eq!(
            dialog.preview(14),
            "14 pages into 4 files of 4, the last holding 2"
        );
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    /// A directory of its own per test, so two running at once cannot collide
    /// over the piece names.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("ypdf-split-test").join(name);
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn pieces_of(name: &str, mode: &SplitMode) -> Vec<Piece> {
        let pdf = ypdf_doc::Pdf::open(fixture(name)).expect("fixture opens");
        ypdf_doc::split(&pdf, mode).expect("splits")
    }

    #[test]
    fn pieces_are_written_under_the_names_the_cli_uses() {
        let dir = scratch("every-four");
        let pieces = pieces_of("many-pages.pdf", &SplitMode::EveryN(4));

        let (written, failure) = write_pieces(&dir, "many-pages", pieces);
        assert!(failure.is_none(), "{failure:?}");
        assert_eq!(written.len(), 3);

        let names: Vec<String> = written
            .iter()
            .map(|w| {
                w.path
                    .file_name()
                    .expect("name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            names,
            vec![
                "many-pages-pages-1-4.pdf",
                "many-pages-pages-5-8.pdf",
                "many-pages-pages-9-12.pdf",
            ],
            "the GUI and the CLI must not drift apart on naming"
        );

        for piece in &written {
            assert_eq!(piece.pages, 4);
            let reopened = ypdf_doc::Pdf::open(&piece.path).expect("piece opens");
            assert_eq!(reopened.page_count(), 4, "what was written is what was cut");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_name_already_taken_stops_the_run_and_keeps_the_earlier_pieces() {
        let dir = scratch("collision");
        // Stand something in the way of the second piece.
        let blocker = dir.join("many-pages-pages-5-8.pdf");
        std::fs::write(&blocker, b"not a pdf").expect("blocker");

        let pieces = pieces_of("many-pages.pdf", &SplitMode::EveryN(4));
        let (written, failure) = write_pieces(&dir, "many-pages", pieces);

        assert_eq!(written.len(), 1, "the run stops at the file in the way");
        let failure = failure.expect("a refusal to overwrite is reported");
        assert!(
            failure.contains("many-pages-pages-5-8.pdf"),
            "the message names the file: {failure}"
        );
        assert_eq!(
            std::fs::read(&blocker).expect("blocker still there"),
            b"not a pdf",
            "the file in the way is untouched"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nothing_open_says_so_rather_than_promising_files() {
        let dialog = SplitDialog::default();
        assert_eq!(dialog.preview(0), "Nothing is open.");
    }
}
