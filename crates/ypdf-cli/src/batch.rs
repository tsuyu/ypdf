//! Running a command over many files (spec §21).
//!
//! Three properties make a batch run trustworthy over a thousand files:
//!
//! * **Errors are isolated.** One unreadable file does not stop the other 1,247,
//!   and the summary says exactly which ones failed.
//! * **Order is stable.** Results come back in the order the inputs were given,
//!   whatever order the workers finished in, so two runs produce the same output
//!   and a diff means something.
//! * **Cancellation is immediate.** Ctrl-C stops new files from starting and the
//!   exit code says the run was cancelled rather than that it succeeded.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;
use serde_json::{Value, json};
use ypdf_core::{CancelToken, Error, Result};

use crate::output::{Out, error_json, thousands};

/// What one file's work produced.
#[derive(Clone, Debug, Default)]
pub struct FileReport {
    /// Human-readable body for this file.
    pub human: String,
    /// Machine-readable body for this file.
    pub json: Value,
    /// Input size, when the command is one where size is the point.
    pub bytes_in: u64,
    /// Output size, likewise.
    pub bytes_out: u64,
    /// Set when the file was processed but something is worth flagging, e.g. a
    /// security finding at or above `--fail-on`.
    pub flagged: bool,
}

impl FileReport {
    /// A report carrying only text.
    #[must_use]
    pub fn text(human: impl Into<String>, json: Value) -> Self {
        Self {
            human: human.into(),
            json,
            ..Self::default()
        }
    }
}

/// The outcome of a whole run.
#[derive(Debug)]
pub struct BatchOutcome {
    /// Per-file results, in input order.
    results: Vec<(PathBuf, Result<FileReport>)>,
}

impl BatchOutcome {
    /// Wrap results a command produced on its own.
    ///
    /// For work that cannot go through [`run`] — OCR rasterizes, and PDFium is
    /// single-threaded — so that it still reports and exits like every other
    /// batch.
    #[must_use]
    pub fn from_results(results: Vec<(PathBuf, Result<FileReport>)>) -> Self {
        Self { results }
    }

    /// Files that succeeded.
    #[must_use]
    pub fn processed(&self) -> usize {
        self.results.iter().filter(|(_, r)| r.is_ok()).count()
    }

    /// Files that failed.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.results.len() - self.processed()
    }

    /// The exit code for the process.
    ///
    /// A run where anything failed exits with the first failure's code rather
    /// than a generic 1: a script that branches on `E_PARSE_XREF` should still
    /// be able to when the file was one of many.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        for (_, result) in &self.results {
            if let Err(error) = result {
                return error.exit_code();
            }
        }
        if self
            .results
            .iter()
            .any(|(_, r)| r.as_ref().map(|report| report.flagged).unwrap_or(false))
        {
            // A clean run that tripped a gate: distinct from both success and a
            // processing failure.
            return 1;
        }
        0
    }

    /// Write everything out in whichever form was asked for.
    pub fn emit(&self, out: &Out, command: &str) {
        if out.is_json() {
            out.json(&self.to_json(command));
            return;
        }

        let many = self.results.len() > 1;
        for (path, result) in &self.results {
            match result {
                Ok(report) => {
                    if many {
                        out.human(format!("=== {}", path.display()));
                    }
                    if !report.human.is_empty() {
                        out.human(&report.human);
                    }
                }
                Err(error) => {
                    out.status(format!("--- {}", path.display()));
                    out.error(error);
                }
            }
        }

        if many {
            out.status(self.summary_text());
        }
    }

    /// The summary block from spec §21.
    fn summary_text(&self) -> String {
        let (before, after) = self.totals();
        let mut out = format!(
            "\n{} PDFs\nProcessed: {}\nFailed: {}",
            thousands(self.results.len()),
            thousands(self.processed()),
            thousands(self.failed())
        );
        if before > 0 && after > 0 {
            out.push_str(&format!(
                "\nTotal: {} → {}",
                ypdf_optimize::format_size(before),
                ypdf_optimize::format_size(after)
            ));
        }
        out
    }

    fn totals(&self) -> (u64, u64) {
        self.results
            .iter()
            .filter_map(|(_, r)| r.as_ref().ok())
            .fold((0, 0), |(a, b), report| {
                (a + report.bytes_in, b + report.bytes_out)
            })
    }

    fn to_json(&self, command: &str) -> Value {
        let files: Vec<Value> = self
            .results
            .iter()
            .map(|(path, result)| match result {
                Ok(report) => json!({
                    "path": path.display().to_string(),
                    "ok": true,
                    "result": report.json,
                }),
                Err(error) => json!({
                    "path": path.display().to_string(),
                    "ok": false,
                    "error": error_json(error),
                }),
            })
            .collect();

        let (before, after) = self.totals();
        json!({
            "command": command,
            "files": files,
            "summary": {
                "total": self.results.len(),
                "processed": self.processed(),
                "failed": self.failed(),
                "bytes_before": before,
                "bytes_after": after,
            },
            "exit_code": self.exit_code(),
        })
    }
}

/// Turn the command-line inputs into a list of files.
///
/// Windows shells do not expand globs, so a pattern that reaches the process
/// untouched is expanded here. A plain path is taken as given — including one
/// that does not exist, so the error names the file the user typed rather than
/// saying the glob matched nothing.
pub fn expand(inputs: &[String]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for input in inputs {
        if !input.contains(['*', '?', '[']) {
            files.push(PathBuf::from(input));
            continue;
        }

        let matches = glob::glob(input).map_err(|e| Error::Config {
            detail: format!("bad pattern {input:?}: {e}"),
            source_path: None,
        })?;

        // Sorted, so `chapter-*.pdf` merges in a predictable order rather than
        // in whatever order the filesystem returns.
        let mut matched: Vec<PathBuf> = matches.filter_map(std::result::Result::ok).collect();
        matched.sort();
        if matched.is_empty() {
            return Err(Error::Config {
                detail: format!("no files matched {input:?}"),
                source_path: None,
            });
        }
        files.extend(matched);
    }

    Ok(files)
}

/// Run `work` over every file, in parallel, isolating failures.
pub fn run<F>(
    files: &[PathBuf],
    workers: usize,
    cancel: &CancelToken,
    out: &Out,
    work: F,
) -> BatchOutcome
where
    F: Fn(&Path, &CancelToken) -> Result<FileReport> + Sync + Send,
{
    let total = files.len();
    let done = AtomicUsize::new(0);

    let run_one = |path: &PathBuf| -> (PathBuf, Result<FileReport>) {
        if cancel.is_cancelled() {
            return (path.clone(), Err(Error::Cancelled));
        }

        let started = std::time::Instant::now();
        let result = work(path, cancel);
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;

        if total > 1 {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            match &result {
                Ok(report) if report.bytes_in > 0 && report.bytes_out > 0 => out.status(format!(
                    "[{n}/{total}] {name}  {} → {}",
                    ypdf_optimize::format_size(report.bytes_in),
                    ypdf_optimize::format_size(report.bytes_out)
                )),
                Ok(_) => out.status(format!("[{n}/{total}] {name}")),
                Err(error) => {
                    out.status(format!("[{n}/{total}] {name}  failed: {}", error.message()))
                }
            }
            out.note(format!("      {:.2}s", started.elapsed().as_secs_f32()));
        }

        (path.clone(), result)
    };

    // One file is the common case and rayon's pool costs more than it saves.
    let results: Vec<(PathBuf, Result<FileReport>)> = if total == 1 || workers <= 1 {
        files.iter().map(run_one).collect()
    } else {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers.min(total))
            .build();
        match pool {
            // par_iter keeps input order in the collected output, which is what
            // makes two runs of the same batch comparable.
            Ok(pool) => pool.install(|| files.par_iter().map(run_one).collect()),
            Err(_) => files.iter().map(run_one).collect(),
        }
    };

    BatchOutcome { results }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(results: Vec<(PathBuf, Result<FileReport>)>) -> BatchOutcome {
        BatchOutcome { results }
    }

    #[test]
    fn a_plain_path_is_taken_as_typed_even_if_it_is_missing() {
        // The error should name the file the user asked for, not claim that a
        // pattern matched nothing.
        let files = expand(&["no-such-file.pdf".to_string()]).expect("no glob, no failure");
        assert_eq!(files, vec![PathBuf::from("no-such-file.pdf")]);
    }

    #[test]
    fn a_pattern_matching_nothing_is_an_error() {
        let result = expand(&["./definitely-not-here/*.pdf".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn a_pattern_expands_sorted() {
        let files = expand(&["../../tests/fixtures/*.pdf".to_string()]).expect("fixtures exist");
        assert!(files.len() > 1, "several fixtures expected");
        let mut sorted = files.clone();
        sorted.sort();
        assert_eq!(files, sorted);
    }

    #[test]
    fn one_failure_does_not_hide_the_successes() {
        let batch = outcome(vec![
            (PathBuf::from("a.pdf"), Ok(FileReport::default())),
            (PathBuf::from("b.pdf"), Err(Error::Cancelled)),
            (PathBuf::from("c.pdf"), Ok(FileReport::default())),
        ]);
        assert_eq!(batch.processed(), 2);
        assert_eq!(batch.failed(), 1);
        assert_eq!(
            batch.exit_code(),
            130,
            "the failure's own code, not a generic 1"
        );
    }

    #[test]
    fn the_summary_reports_totals_when_sizes_are_the_point() {
        let batch = outcome(vec![
            (
                PathBuf::from("a.pdf"),
                Ok(FileReport {
                    bytes_in: 2_000_000,
                    bytes_out: 1_000_000,
                    ..FileReport::default()
                }),
            ),
            (
                PathBuf::from("b.pdf"),
                Ok(FileReport {
                    bytes_in: 1_000_000,
                    bytes_out: 500_000,
                    ..FileReport::default()
                }),
            ),
        ]);
        let text = batch.summary_text();
        assert!(text.contains("Processed: 2"), "{text}");
        assert!(text.contains("Failed: 0"), "{text}");
        assert!(text.contains('→'), "{text}");
    }

    #[test]
    fn a_flagged_file_exits_non_zero_without_being_a_failure() {
        let batch = outcome(vec![(
            PathBuf::from("hostile.pdf"),
            Ok(FileReport {
                flagged: true,
                ..FileReport::default()
            }),
        )]);
        assert_eq!(batch.failed(), 0);
        assert_eq!(batch.exit_code(), 1);
    }

    #[test]
    fn json_output_names_every_file_and_its_state() {
        let batch = outcome(vec![
            (PathBuf::from("a.pdf"), Ok(FileReport::default())),
            (PathBuf::from("b.pdf"), Err(Error::Cancelled)),
        ]);
        let json = batch.to_json("info");
        assert_eq!(json["command"], "info");
        assert_eq!(json["summary"]["failed"], 1);
        assert_eq!(json["files"][0]["ok"], true);
        assert_eq!(json["files"][1]["ok"], false);
        assert_eq!(json["files"][1]["error"]["exit_code"], 130);
    }
}
