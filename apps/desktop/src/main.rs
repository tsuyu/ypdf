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
mod forms;
mod inspect;
mod ocr;
mod outline;
mod protect;
mod recent;
mod redact;
mod search;
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

    eframe::run_native(
        "yPDF",
        native_options,
        Box::new(move |cc| Ok(Box::new(app::YpdfApp::new(cc, config, render, open)))),
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
