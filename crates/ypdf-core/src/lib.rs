//! Shared foundation for every yPDF crate.
//!
//! Nothing here touches a PDF. This crate owns the vocabulary that the engine
//! crates, the GUI, and the CLI all speak: errors with stable codes,
//! layered configuration, presets, progress reporting, and cancellation.
//!
//! Two rules hold across the whole workspace and start here:
//!
//! * every long-running operation accepts a [`CancelToken`];
//! * every long-running operation reports [`Progress`].

pub mod cancel;
pub mod config;
pub mod error;
pub mod logging;
pub mod op;
pub mod preset;
pub mod progress;

pub use cancel::CancelToken;
pub use config::{Config, ConfigPatch, ConfigSource};
pub use error::{Error, ErrorReport, Limit, ParseFailure, Result};
pub use op::OperationId;
pub use preset::Preset;
pub use progress::{Progress, ProgressReporter};
