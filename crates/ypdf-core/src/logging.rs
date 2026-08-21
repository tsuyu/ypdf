//! Structured logging (spec §31).
//!
//! One initialization point for the GUI, the CLI, and the future server. Format
//! and level come from [`Config`]; `YPDF_LOG` overrides the level with full
//! `tracing` filter syntax when a developer needs per-module control.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tracing_subscriber::EnvFilter;

use crate::config::{Config, LogFormat, Logging};
use crate::error::{Error, ErrorReport, Result};
use crate::op::OperationId;

static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Install the global subscriber.
///
/// Calling this twice is a no-op rather than an error, so tests and embedded
/// uses do not have to coordinate.
pub fn init(cfg: &Logging) -> Result<()> {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    // `YPDF_LOG` takes precedence: it is the developer escape hatch and accepts
    // full filter syntax such as `info,ypdf_render=trace`.
    let filter = match EnvFilter::try_from_env("YPDF_LOG") {
        Ok(filter) => filter,
        Err(_) => EnvFilter::try_new(&cfg.level).map_err(|e| Error::Config {
            detail: format!("logging.level: {e}"),
            source_path: None,
        })?,
    };

    // Logs go to stderr, never stdout. The CLI writes its result to stdout, so
    // a stray INFO line would end up inside the JSON a script is parsing.
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_writer(std::io::stderr);

    match cfg.format {
        LogFormat::Json => builder.json().flatten_event(true).init(),
        LogFormat::Pretty => builder.with_ansi(true).init(),
    }

    Ok(())
}

/// One completed operation, logged as a single structured record (spec §31).
#[derive(Clone, Debug, Serialize)]
pub struct OperationRecord {
    /// The operation identifier.
    pub op: OperationId,
    /// Operation name, e.g. `"compress"`.
    pub operation: &'static str,
    /// Input size in bytes, when meaningful.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_size: Option<u64>,
    /// Output size in bytes, when meaningful.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_size: Option<u64>,
    /// Wall-clock duration.
    pub duration_ms: u64,
    /// Page count touched, when meaningful.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
    /// Present when the operation failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorReport>,
}

impl OperationRecord {
    /// A record for a successful operation.
    #[must_use]
    pub fn success(op: OperationId, operation: &'static str, duration_ms: u64) -> Self {
        Self {
            op,
            operation,
            input_size: None,
            output_size: None,
            duration_ms,
            pages: None,
            error: None,
        }
    }

    /// Attach input and output sizes.
    #[must_use]
    pub fn with_sizes(mut self, input: u64, output: u64) -> Self {
        self.input_size = Some(input);
        self.output_size = Some(output);
        self
    }

    /// Attach the page count.
    #[must_use]
    pub fn with_pages(mut self, pages: u32) -> Self {
        self.pages = Some(pages);
        self
    }

    /// Attach a failure. Paths are redacted unless the config allows them.
    #[must_use]
    pub fn with_error(mut self, error: &Error, cfg: &Config) -> Self {
        let report = error.report();
        self.error = Some(if cfg.privacy.log_full_paths {
            report
        } else {
            report.redacted()
        });
        self
    }

    /// Emit the record. One line at INFO, or WARN when it carries an error.
    pub fn emit(&self) {
        match serde_json::to_string(self) {
            Ok(json) if self.error.is_some() => tracing::warn!(record = %json, "operation failed"),
            Ok(json) => tracing::info!(record = %json, "operation completed"),
            Err(e) => tracing::error!("could not serialize operation record: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_serializes_to_the_spec_shape() {
        let record = OperationRecord::success(OperationId::new(), "compress", 1820)
            .with_sizes(48_200_000, 12_700_000)
            .with_pages(124);
        let json = serde_json::to_value(&record).expect("serializes");
        assert_eq!(json["operation"], "compress");
        assert_eq!(json["input_size"], 48_200_000_u64);
        assert_eq!(json["output_size"], 12_700_000_u64);
        assert_eq!(json["duration_ms"], 1820);
        assert_eq!(json["pages"], 124);
        assert!(json.get("error").is_none());
    }

    #[test]
    fn errors_are_redacted_unless_allowed() {
        let err = Error::io("C:/secret/dir/file.pdf", std::io::Error::other("x"));
        let mut cfg = Config::default();

        let redacted =
            OperationRecord::success(OperationId::new(), "info", 1).with_error(&err, &cfg);
        assert_eq!(
            redacted.error.as_ref().and_then(|e| e.path.as_deref()),
            Some("file.pdf")
        );

        cfg.privacy.log_full_paths = true;
        let full = OperationRecord::success(OperationId::new(), "info", 1).with_error(&err, &cfg);
        assert!(
            full.error
                .expect("error")
                .path
                .expect("path")
                .contains("secret")
        );
    }
}
