//! `ypdf-cli` — the scriptable face of the engine (spec §21, §22).
//!
//! Everything here is the same engine the desktop application uses; this crate
//! only turns arguments into calls and results into text or JSON.
//!
//! Three promises the CLI keeps:
//!
//! * **Exit codes are stable.** They come from [`ypdf_core::Error::exit_code`],
//!   so `E_PARSE_XREF` is 3 whether it came from the GUI, a batch of one, or a
//!   batch of a thousand.
//! * **Ctrl-C stops the work.** It sets the same cancellation token every engine
//!   operation already checks, and the run exits 130 rather than pretending to
//!   have finished.
//! * **Nothing is uploaded.** There is no network code in this binary or in
//!   anything below it.

mod batch;
mod cli;
mod commands;
mod output;
mod paths;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ypdf_core::{CancelToken, Config, ConfigPatch, Error, Result};

use crate::batch::FileReport;
use crate::cli::{Cli, Command};
use crate::output::Out;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let out = Out::new(&cli.global);

    let code = match run(&cli, &out) {
        Ok(code) => code,
        Err(error) => {
            if out.is_json() {
                out.json(&serde_json::json!({
                    "command": cli.command.name(),
                    "ok": false,
                    "error": output::error_json(&error),
                }));
            } else {
                out.error(&error);
            }
            error.exit_code()
        }
    };

    // ExitCode takes a u8; every code the engine produces fits, and clamping is
    // better than wrapping 130 round to something that reads as success.
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn run(cli: &Cli, out: &Out) -> Result<i32> {
    let config = load_config(cli)?;
    ypdf_core::logging::init(&config.logging)?;

    let cancel = CancelToken::new();
    install_interrupt_handler(&cancel, out);

    let workers = cli.global.workers.unwrap_or(config.engine.workers).max(1);
    let overwrite = cli.global.overwrite;
    let password = cli.global.password.as_deref();

    // The commands that take one input and produce one output are not batches,
    // and printing a batch summary over a single file would be noise.
    match &cli.command {
        Command::Merge(args) => {
            let files = batch::expand(&args.inputs)?;
            let report = commands::pages::merge(&files, args, password, overwrite, &cancel, out)?;
            emit_single(out, cli.command.name(), &report);
            Ok(0)
        }
        Command::Split(args) => {
            let report = commands::pages::split(args, password, overwrite, &cancel, out)?;
            emit_single(out, cli.command.name(), &report);
            Ok(0)
        }
        Command::Extract(args) => {
            let report = commands::pages::extract(args, password, overwrite, &cancel)?;
            emit_single(out, cli.command.name(), &report);
            Ok(0)
        }
        Command::Redact(args) => {
            // Finding text means asking PDFium where the characters are, and
            // PDFium lives on one thread, so this runs its own loop.
            let files = batch::expand(&args.inputs)?;
            let results = commands::redact::run(&files, args, password, overwrite, &cancel, out)?;
            let outcome = batch::BatchOutcome::from_results(results);
            outcome.emit(out, cli.command.name());
            Ok(outcome.exit_code())
        }
        Command::Ocr(args) => {
            // OCR rasterizes, and PDFium lives on one thread by design, so it
            // runs its own sequential loop rather than going through the
            // parallel batch runner.
            let files = batch::expand(&args.inputs)?;
            let results = commands::ocr::run(&files, args, password, overwrite, &cancel, out)?;
            let outcome = batch::BatchOutcome::from_results(results);
            outcome.emit(out, cli.command.name());
            Ok(outcome.exit_code())
        }
        _ => run_batch(cli, out, workers, overwrite, &cancel),
    }
}

/// Everything that works file by file (spec §21).
fn run_batch(
    cli: &Cli,
    out: &Out,
    workers: usize,
    overwrite: bool,
    cancel: &CancelToken,
) -> Result<i32> {
    let files = batch::expand(inputs_of(&cli.command))?;
    let many = files.len() > 1;
    let password = cli.global.password.as_deref();

    if many {
        out.status(format!(
            "{} files, {workers} at a time",
            output::thousands(files.len())
        ));
    }

    let outcome = match &cli.command {
        Command::Info(_) => batch::run(&files, workers, cancel, out, |path, cancel| {
            commands::inspect::info(path, password, cancel)
        }),
        Command::Diagnostics(_) => batch::run(&files, workers, cancel, out, |path, cancel| {
            commands::inspect::diagnostics(path, password, cancel)
        }),
        Command::SecurityScan(args) => {
            let threshold = args.fail_on.map(Into::into);
            batch::run(&files, workers, cancel, out, |path, cancel| {
                commands::inspect::security_scan(path, threshold, password, cancel)
            })
        }
        Command::Signatures(args) => {
            let require_signed = args.require_signed;
            batch::run(&files, workers, cancel, out, |path, cancel| {
                commands::signatures::run(path, require_signed, password, cancel)
            })
        }
        Command::Pdfa(args) => {
            // Parsed once, before the batch: a level nobody can conform to is a
            // mistake in the command, not a property of a thousand files.
            let level = args
                .level
                .as_deref()
                .map(ypdf_pdfa::Level::parse)
                .transpose()?;
            batch::run(&files, workers, cancel, out, |path, cancel| {
                commands::pdfa::run(path, level, password, cancel)
            })
        }
        Command::Metadata(args) => batch::run(&files, workers, cancel, out, |path, cancel| {
            commands::inspect::metadata(path, args, password, many, overwrite, cancel)
        }),
        Command::Compress(args) => {
            // Resolved once: an unusable --preset should fail before any file is
            // touched, not on each of a thousand of them.
            let settings = commands::optimize::settings_from(args)?;
            batch::run(&files, workers, cancel, out, |path, cancel| {
                commands::optimize::compress(
                    path, args, &settings, password, many, overwrite, cancel, out,
                )
            })
        }
        Command::Watermark(args) => {
            // Resolved once: an unreadable image or an impossible colour is the
            // same answer for every file in the batch.
            let watermark = commands::watermark::watermark_from(args)?;
            batch::run(&files, workers, cancel, out, |path, cancel| {
                commands::watermark::run(path, args, &watermark, password, many, overwrite, cancel)
            })
        }
        Command::Images(args) => batch::run(&files, workers, cancel, out, |path, cancel| {
            commands::optimize::images(path, args, password, many, overwrite, cancel, out)
        }),
        Command::Annotate(args) => batch::run(&files, workers, cancel, out, |path, cancel| {
            commands::annotate::run(path, args, password, many, overwrite, cancel)
        }),
        Command::Forms(args) => batch::run(&files, workers, cancel, out, |path, cancel| {
            commands::forms::run(path, args, password, many, overwrite, cancel)
        }),
        Command::Bookmarks(args) => batch::run(&files, workers, cancel, out, |path, cancel| {
            commands::navigate::bookmarks_command(path, args, password, many, overwrite, cancel)
        }),
        Command::Links(args) => batch::run(&files, workers, cancel, out, |path, cancel| {
            commands::navigate::links_command(path, args, password, many, overwrite, cancel)
        }),
        Command::Encrypt(args) => batch::run(&files, workers, cancel, out, |path, cancel| {
            commands::protect::encrypt(path, args, password, many, overwrite, cancel)
        }),
        Command::Decrypt(args) => batch::run(&files, workers, cancel, out, |path, cancel| {
            commands::protect::decrypt(path, args, password, many, overwrite, cancel)
        }),
        Command::Merge(_)
        | Command::Split(_)
        | Command::Extract(_)
        | Command::Ocr(_)
        | Command::Redact(_) => {
            return Err(Error::Config {
                detail: "this command is not a batch command".into(),
                source_path: None,
            });
        }
    };

    outcome.emit(out, cli.command.name());
    Ok(outcome.exit_code())
}

/// The inputs of whichever command was given.
fn inputs_of(command: &Command) -> &[String] {
    match command {
        Command::Info(args) | Command::Diagnostics(args) => &args.inputs,
        Command::SecurityScan(args) => &args.inputs,
        Command::Signatures(args) => &args.inputs,
        Command::Pdfa(args) => &args.inputs,
        Command::Metadata(args) => &args.inputs,
        Command::Compress(args) => &args.inputs,
        Command::Images(args) => &args.inputs,
        Command::Watermark(args) => &args.inputs,
        Command::Encrypt(args) => &args.inputs,
        Command::Decrypt(args) => &args.inputs,
        Command::Merge(args) => &args.inputs,
        Command::Ocr(args) => &args.inputs,
        Command::Redact(args) => &args.inputs,
        Command::Annotate(args) => &args.inputs,
        Command::Forms(args) => &args.inputs,
        Command::Bookmarks(args) => &args.inputs,
        Command::Links(args) => &args.inputs,
        Command::Split(_) | Command::Extract(_) => &[],
    }
}

fn emit_single(out: &Out, command: &str, report: &FileReport) {
    if out.is_json() {
        out.json(&serde_json::json!({
            "command": command,
            "ok": true,
            "result": report.json,
        }));
    } else {
        out.human(&report.human);
    }
}

/// The five configuration layers, plus `--config` if one was named.
fn load_config(cli: &Cli) -> Result<Config> {
    let mut patch = ConfigPatch::default();
    if let Some(workers) = cli.global.workers {
        patch.engine.workers = Some(workers);
    }
    if cli.global.verbose && !cli.global.quiet {
        patch.logging.level = Some("debug".into());
    }
    if cli.global.quiet {
        patch.logging.level = Some("error".into());
    }

    let project_dir = cli
        .global
        .config
        .as_ref()
        .map(|path| directory_of(path))
        .transpose()?;

    Config::load(project_dir.as_deref(), patch)
}

/// `--config path/to/config.toml` is loaded as that directory's project layer.
fn directory_of(path: &std::path::Path) -> Result<PathBuf> {
    if !path.is_file() {
        return Err(Error::Config {
            detail: format!("no configuration file at {}", path.display()),
            source_path: Some(path.to_path_buf()),
        });
    }
    Ok(path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf))
}

/// Ctrl-C cancels the work rather than killing the process mid-write.
///
/// A half-written PDF is worse than no PDF, and every engine operation already
/// checks the token between steps.
fn install_interrupt_handler(cancel: &CancelToken, out: &Out) {
    let cancel = cancel.clone();
    let handler_out = out.clone();
    let result = ctrlc::set_handler(move || {
        if cancel.is_cancelled() {
            // Asked twice: they mean it.
            std::process::exit(130);
        }
        handler_out
            .status("\nStopping — finishing the file in progress. Ctrl-C again to quit now.");
        cancel.cancel();
    });
    if result.is_err() {
        out.note("could not install a Ctrl-C handler; interrupts will kill the process");
    }
}
