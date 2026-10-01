//! yPDF desktop shell.
//!
//! Startup order matters: configuration, then logging, then the render thread —
//! so a missing PDFium library is reported as a diagnostic rather than an
//! empty window.

// The GUI is the one place where a top-level window is the product; a console
// window alongside it is not. Debug builds keep the console for `tracing`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod annotate;
mod app;
mod compress;
mod document;
mod edit;
mod export;
mod forms;
mod images;
mod inspect;
mod merge;
mod ocr;
mod outline;
mod protect;
mod recent;
mod redact;
mod search;
mod split;
mod textures;
mod thumbnails;
mod viewer;
mod watermark;

use std::path::PathBuf;

use ypdf_core::{Config, ConfigPatch, Error, logging};
use ypdf_render::RenderHandle;

fn main() -> eframe::Result {
    // Configuration must resolve before logging can be installed, so failures
    // here have nowhere to go but stderr.
    let config = fatal_on_error(Config::load(
        project_dir().as_deref(),
        ConfigPatch::default(),
    ));
    fatal_on_error(logging::init(&config.logging));

    tracing::info!(
        workers = config.engine.workers,
        texture_budget_mb = config.cache.texture_budget_mb,
        "yPDF {} starting",
        env!("CARGO_PKG_VERSION")
    );

    // PDFium lives on exactly one thread for the life of the process.
    let render = fatal_on_error(RenderHandle::spawn());

    let open: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("yPDF")
            .with_drag_and_drop(true),
        ..Default::default()
    };

    let result = eframe::run_native(
        "yPDF",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::YpdfApp::new(cc, config, render, open)))),
    );

    if let Err(error) = &result {
        report_window_failure(&error.to_string());
    }
    result
}

/// Say out loud that the window never opened.
///
/// A release build has no console (`windows_subsystem = "windows"`), so an
/// `Err` out of `run_native` is invisible: the process exits and nothing
/// appears on screen at all. That is exactly the failure on a Windows Server
/// with no graphics driver, where the generic GDI OpenGL is 1.1 and the
/// renderer wants 3.3, and often over Remote Desktop as well.
///
/// The message goes to a plain Win32 dialog, which needs no OpenGL of its own,
/// and to the log for whoever reads it afterwards.
fn report_window_failure(detail: &str) {
    let message = window_failure_message(detail);
    tracing::error!("{message}");
    let _ = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("yPDF")
        .set_description(&message)
        .show();
}

/// What to tell someone whose window did not open.
fn window_failure_message(detail: &str) -> String {
    format!(
        "yPDF could not open its window.\n\n\
         {detail}\n\n\
         The viewer needs a desktop session with OpenGL 3.3. A Windows Server \
         with no graphics driver, and a Remote Desktop session without \
         acceleration, usually provide neither.\n\n\
         Everything the viewer does to a file - convert, merge, split, rotate, \
         compress, redact - is also in ypdf-cli.exe, which needs no display."
    )
}

/// Report an engine error the way the CLI would, then exit with its code.
fn fatal_on_error<T>(result: Result<T, Error>) -> T {
    match result {
        Ok(value) => value,
        Err(e) => {
            eprintln!("{}", e.report().to_human());
            std::process::exit(e.exit_code());
        }
    }
}

/// The directory a project-level `config.toml` would live in.
///
/// The current working directory, which is what a user running `ypdf` inside a
/// workspace expects (spec §30).
fn project_dir() -> Option<PathBuf> {
    std::env::current_dir().ok()
}

#[cfg(test)]
mod tests {
    use super::window_failure_message;

    #[test]
    fn the_failure_message_keeps_the_error_and_offers_the_cli() {
        let message = window_failure_message("NoGlutinConfigs(...)");
        assert!(
            message.contains("NoGlutinConfigs(...)"),
            "the real error has to survive: {message}"
        );
        assert!(
            message.contains("ypdf-cli.exe"),
            "someone stuck on a server needs to be told what still works"
        );
        assert!(message.contains("OpenGL 3.3"), "{message}");
    }
}
