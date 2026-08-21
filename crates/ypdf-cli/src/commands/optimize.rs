//! Commands that make files smaller or pull things out of them (spec §4, §6).

use std::path::Path;

use serde_json::json;
use ypdf_core::preset::{Compression, Preset};
use ypdf_core::{CancelToken, Error, OperationId, ProgressReporter, Result};

use crate::batch::FileReport;
use crate::cli::{CompressArgs, ImagesArgs};
use crate::output::Out;
use crate::paths;

/// Turn `--preset`, `--dpi`, and `--quality` into settings.
///
/// Explicit flags win over the preset, so `--preset "Print PDF" --quality 60`
/// means what it looks like.
pub fn settings_from(args: &CompressArgs) -> Result<Compression> {
    let mut settings = match &args.preset {
        None => Compression::default(),
        Some(name) => {
            let presets = Preset::builtin();
            let found = presets
                .iter()
                .find(|p| p.name().eq_ignore_ascii_case(name))
                .ok_or_else(|| Error::Config {
                    detail: format!(
                        "no preset called {name:?}. Available: {}",
                        presets
                            .iter()
                            .map(Preset::name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    source_path: None,
                })?;
            found.compression.clone()
        }
    };

    if let Some(dpi) = args.dpi {
        if dpi == 0 {
            return Err(Error::Config {
                detail: "--dpi needs a value above zero".into(),
                source_path: None,
            });
        }
        settings.dpi = dpi;
    }
    if let Some(quality) = args.quality {
        if quality == 0 || quality > 100 {
            return Err(Error::Config {
                detail: "--quality is 1-100".into(),
                source_path: None,
            });
        }
        settings.quality = quality;
    }

    Ok(settings)
}

/// `compress` (spec §4).
///
/// The compressed document is written to a new path; the input is never the
/// thing being replaced unless `--overwrite` names it explicitly.
#[expect(clippy::too_many_arguments, reason = "one call site, all of it needed")]
pub fn compress(
    path: &Path,
    args: &CompressArgs,
    settings: &Compression,
    password: Option<&str>,
    many: bool,
    overwrite: bool,
    cancel: &CancelToken,
    out: &Out,
) -> Result<FileReport> {
    let target = paths::output_for(&args.output, path, many, "-compressed")?;
    // Check before doing the work rather than after: a refusal an hour into a
    // batch is worse than a refusal at the start.
    paths::guard(&target, overwrite)?;

    let mut pdf = super::open_input(path, password)?;
    if args.strip_metadata {
        ypdf_optimize::strip_metadata(&mut pdf);
    }

    let mut progress = reporter(out);
    let mut report = ypdf_optimize::optimize_with(&mut pdf, settings, cancel, &mut progress)?;
    report.metadata_removed = args.strip_metadata;

    let bytes = pdf.to_bytes()?;
    paths::write(&target, &bytes, overwrite)?;

    let mut human = report.to_human();
    human.push_str(&format!("\nWritten to {}\n", target.display()));

    Ok(FileReport {
        human,
        json: json!({
            "input": path.display().to_string(),
            "output": target.display().to_string(),
            "bytes_before": report.before,
            "bytes_after": report.after,
            "reduction": report.reduction(),
            "images_recompressed": report.images_recompressed,
            "image_bytes_saved": report.image_bytes_saved,
            "images_skipped": report.images_skipped,
            "objects_removed": report.objects_removed,
            "streams_compressed": report.streams_compressed,
            "metadata_removed": report.metadata_removed,
            "not_supported": report.not_supported,
        }),
        bytes_in: report.before,
        bytes_out: report.after,
        flagged: false,
    })
}

/// `images` — write every embedded image out as a file (spec §6).
pub fn images(
    path: &Path,
    args: &ImagesArgs,
    password: Option<&str>,
    many: bool,
    overwrite: bool,
    cancel: &CancelToken,
    out: &Out,
) -> Result<FileReport> {
    let pdf = super::open_input(path, password)?;
    let inventory = ypdf_optimize::extract_images(&pdf);

    // With several inputs each document gets its own directory, or images from
    // different files would collide on `page-001-img-01.jpg`.
    let directory = if many {
        args.output.join(paths::stem_of(path))
    } else {
        args.output.clone()
    };
    paths::ensure_dir(&directory)?;

    let mut written = Vec::with_capacity(inventory.images.len());
    for image in &inventory.images {
        cancel.check()?;
        let target = directory.join(image.file_name());
        paths::write(&target, &image.bytes, overwrite)?;
        out.note(format!(
            "  {} ({}x{}, {})",
            target.display(),
            image.width,
            image.height,
            ypdf_optimize::format_size(image.bytes.len() as u64)
        ));
        written.push(json!({
            "path": target.display().to_string(),
            "page": image.page,
            "width": image.width,
            "height": image.height,
            "bytes": image.bytes.len(),
        }));
    }

    let mut human = format!(
        "{} images written to {} ({})",
        written.len(),
        directory.display(),
        ypdf_optimize::format_size(inventory.total_bytes())
    );
    if !inventory.skipped.is_empty() {
        let detail: Vec<String> = inventory
            .skipped
            .iter()
            .map(|(why, n)| format!("{n} {why}"))
            .collect();
        human.push_str(&format!("\nLeft behind: {}", detail.join(", ")));
    }

    Ok(FileReport {
        human,
        json: json!({
            "input": path.display().to_string(),
            "output_dir": directory.display().to_string(),
            "count": written.len(),
            "images": written,
            "skipped": inventory.skipped,
        }),
        bytes_in: 0,
        bytes_out: 0,
        flagged: false,
    })
}

/// A progress reporter that prints stages to stderr with `--verbose`, and does
/// nothing otherwise.
///
/// The receiver runs on its own thread and stops when the sender is dropped at
/// the end of the operation.
fn reporter(out: &Out) -> ProgressReporter {
    if !out.is_verbose() {
        return ProgressReporter::silent();
    }

    let (tx, rx) = ypdf_core::progress::channel();
    let out = out.clone();
    std::thread::spawn(move || {
        for progress in rx {
            match progress.fraction() {
                Some(fraction) => {
                    out.note(format!("  {} {:.0}%", progress.stage, fraction * 100.0));
                }
                None => out.note(format!("  {}", progress.stage)),
            }
        }
    });
    ProgressReporter::new(OperationId::new(), tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use ypdf_doc::Pdf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("ypdf-cli-tests").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");
        dir
    }

    fn quiet() -> Out {
        Out::new(&crate::cli::Global {
            json: false,
            quiet: true,
            verbose: false,
            config: None,
            overwrite: false,
            workers: None,
            password: None,
        })
    }

    fn compress_args(output: PathBuf) -> CompressArgs {
        CompressArgs {
            inputs: Vec::new(),
            output,
            preset: None,
            dpi: None,
            quality: None,
            strip_metadata: false,
        }
    }

    #[test]
    fn an_explicit_flag_beats_the_preset_it_sits_next_to() {
        let mut args = compress_args(PathBuf::from("out.pdf"));
        args.preset = Some("Print PDF".into());
        args.quality = Some(55);

        let settings = settings_from(&args).expect("resolves");
        assert_eq!(settings.quality, 55);
        // The preset still supplies everything not overridden.
        let print = Preset::builtin()
            .into_iter()
            .find(|p| p.name() == "Print PDF")
            .expect("preset");
        assert_eq!(settings.dpi, print.compression.dpi);
    }

    #[test]
    fn preset_names_are_matched_without_regard_to_case() {
        let mut args = compress_args(PathBuf::from("out.pdf"));
        args.preset = Some("maximum compression".into());
        assert!(settings_from(&args).is_ok());
    }

    #[test]
    fn an_unknown_preset_lists_the_real_ones() {
        let mut args = compress_args(PathBuf::from("out.pdf"));
        args.preset = Some("Tiny".into());
        let error = settings_from(&args).expect_err("no such preset");
        let human = error.report().to_human();
        assert!(human.contains("Web PDF"), "{human}");
        assert!(human.contains("Maximum Compression"), "{human}");
    }

    #[test]
    fn an_out_of_range_quality_is_refused() {
        let mut args = compress_args(PathBuf::from("out.pdf"));
        args.quality = Some(0);
        assert!(settings_from(&args).is_err());
    }

    #[test]
    fn compressing_writes_a_new_file_and_leaves_the_input_alone() {
        let dir = scratch("compress");
        let input = fixture("many-pages.pdf");
        let before = std::fs::read(&input).expect("reads");

        let args = compress_args(dir.join("out.pdf"));
        let settings = settings_from(&args).expect("settings");
        let report = compress(
            &input,
            &args,
            &settings,
            None,
            false,
            false,
            &CancelToken::never(),
            &quiet(),
        )
        .expect("compresses");

        assert!(dir.join("out.pdf").is_file());
        assert_eq!(std::fs::read(&input).expect("reads"), before);
        assert_eq!(
            Pdf::open(dir.join("out.pdf"))
                .expect("re-opens")
                .page_count(),
            12
        );
        assert!(report.bytes_in > 0 && report.bytes_out > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compressing_refuses_an_existing_output_before_doing_the_work() {
        let dir = scratch("compress-guard");
        let target = dir.join("out.pdf");
        std::fs::write(&target, b"existing").expect("writes");

        let args = compress_args(target.clone());
        let settings = settings_from(&args).expect("settings");
        let result = compress(
            &fixture("many-pages.pdf"),
            &args,
            &settings,
            None,
            false,
            false,
            &CancelToken::never(),
            &quiet(),
        );

        assert!(result.is_err());
        assert_eq!(std::fs::read(&target).expect("reads"), b"existing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_run_stops_rather_than_writing_a_half_result() {
        let dir = scratch("compress-cancel");
        let args = compress_args(dir.join("out.pdf"));
        let settings = settings_from(&args).expect("settings");

        let cancel = CancelToken::new();
        cancel.cancel();
        let result = compress(
            &fixture("many-pages.pdf"),
            &args,
            &settings,
            None,
            false,
            false,
            &cancel,
            &quiet(),
        );

        assert!(result.is_err());
        assert!(!dir.join("out.pdf").exists(), "nothing may be written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_document_with_no_images_reports_none_rather_than_failing() {
        let dir = scratch("images-none");
        let args = ImagesArgs {
            inputs: Vec::new(),
            output: dir.clone(),
        };
        let report = images(
            &fixture("many-pages.pdf"),
            &args,
            None,
            false,
            false,
            &CancelToken::never(),
            &quiet(),
        )
        .expect("runs");

        assert_eq!(report.json["count"], 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
