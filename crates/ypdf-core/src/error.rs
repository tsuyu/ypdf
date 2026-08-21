//! Errors with stable codes, human text, and a JSON form (spec §35).
//!
//! Three consumers, one type:
//!
//! * the GUI shows [`Error::message`] plus [`Error::suggestion`];
//! * the CLI prints [`ErrorReport::to_human`] and exits with [`Error::exit_code`];
//! * the API and `--json` emit [`ErrorReport`] as JSON.
//!
//! Codes are part of the public contract. Never renumber one; add a new variant
//! instead.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Result alias used throughout the workspace.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why a document could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ParseFailure {
    /// Header is missing or is not a `%PDF-` marker.
    BadHeader,
    /// Cross-reference table or stream is unusable.
    InvalidXref,
    /// Trailer dictionary is missing or has no root.
    MissingTrailer,
    /// A stream could not be decoded with its declared filters.
    CorruptStream { object: u32 },
    /// An object referenced by the document does not exist.
    MissingObject { object: u32, generation: u16 },
    /// Structurally valid but semantically wrong.
    Other { detail: String },
}

impl fmt::Display for ParseFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadHeader => f.write_str("Missing or invalid PDF header."),
            Self::InvalidXref => f.write_str("Invalid cross-reference table."),
            Self::MissingTrailer => f.write_str("Missing trailer dictionary."),
            Self::CorruptStream { object } => {
                write!(f, "Corrupted stream in object {object}.")
            }
            Self::MissingObject { object, generation } => {
                write!(
                    f,
                    "Referenced object {object} {generation} R does not exist."
                )
            }
            Self::Other { detail } => f.write_str(detail),
        }
    }
}

/// A resource ceiling that an operation ran into (spec §24, §25).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Limit {
    /// Input exceeded the configured maximum file size.
    FileSize { limit_mb: u64, actual_mb: u64 },
    /// Operation would exceed the configured memory budget.
    Memory { limit_mb: u64 },
    /// Operation exceeded its wall-clock budget.
    Time { limit_secs: u64 },
    /// Page count exceeded what this operation accepts.
    PageCount { limit: u32 },
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileSize {
                limit_mb,
                actual_mb,
            } => {
                write!(f, "File is {actual_mb} MB, limit is {limit_mb} MB.")
            }
            Self::Memory { limit_mb } => write!(f, "Memory budget of {limit_mb} MB exhausted."),
            Self::Time { limit_secs } => write!(f, "Exceeded the {limit_secs}s time budget."),
            Self::PageCount { limit } => write!(f, "Exceeds the {limit} page limit."),
        }
    }
}

/// Everything that can go wrong in the engine.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Filesystem failure, with the path that caused it.
    Io {
        path: Option<PathBuf>,
        source: std::io::Error,
    },
    /// The document could not be parsed.
    Parse {
        reason: ParseFailure,
        path: Option<PathBuf>,
    },
    /// The document is encrypted and no password was supplied.
    PasswordRequired,
    /// A password was supplied but it was wrong.
    WrongPassword,
    /// Permitted by the file's permission flags, refused by us.
    PermissionDenied { action: &'static str },
    /// A page index outside the document was requested.
    PageOutOfRange { requested: u32, pages: u32 },
    /// A page range expression could not be understood.
    InvalidPageRange { spec: String },
    /// A valid PDF feature that yPDF does not implement yet.
    Unsupported { feature: String },
    /// An output file exists and replacing it was not asked for.
    ///
    /// Separate from [`Self::Config`] because it is not a mistake in what the
    /// user typed: the command was valid, and refusing it protected a file.
    OutputExists { path: PathBuf },
    /// Configuration or preset is invalid.
    Config {
        detail: String,
        source_path: Option<PathBuf>,
    },
    /// The operation was cancelled by the user.
    Cancelled,
    /// A configured resource ceiling was hit.
    LimitExceeded { limit: Limit },
    /// A native backend (pdfium, qpdf, tesseract) failed.
    Backend {
        backend: &'static str,
        detail: String,
    },
}

impl Error {
    /// Stable machine-readable code. Part of the public contract.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "E_IO",
            Self::Parse { reason, .. } => match reason {
                ParseFailure::BadHeader => "E_PARSE_HEADER",
                ParseFailure::InvalidXref => "E_PARSE_XREF",
                ParseFailure::MissingTrailer => "E_PARSE_TRAILER",
                ParseFailure::CorruptStream { .. } => "E_PARSE_STREAM",
                ParseFailure::MissingObject { .. } => "E_PARSE_OBJECT",
                ParseFailure::Other { .. } => "E_PARSE",
            },
            Self::PasswordRequired => "E_PASSWORD_REQUIRED",
            Self::WrongPassword => "E_PASSWORD_WRONG",
            Self::PermissionDenied { .. } => "E_PERMISSION",
            Self::PageOutOfRange { .. } => "E_PAGE_RANGE",
            Self::InvalidPageRange { .. } => "E_PAGE_SPEC",
            Self::Unsupported { .. } => "E_UNSUPPORTED",
            Self::OutputExists { .. } => "E_OUTPUT_EXISTS",
            Self::Config { .. } => "E_CONFIG",
            Self::Cancelled => "E_CANCELLED",
            Self::LimitExceeded { .. } => "E_LIMIT",
            Self::Backend { .. } => "E_BACKEND",
        }
    }

    /// Short headline, shown first in the GUI and the CLI.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Io { .. } => "File could not be read or written.".into(),
            Self::Parse { .. } => "PDF cannot be parsed.".into(),
            Self::PasswordRequired => "PDF is password protected.".into(),
            Self::WrongPassword => "Incorrect password.".into(),
            Self::PermissionDenied { action } => {
                format!("The document does not permit {action}.")
            }
            Self::PageOutOfRange { .. } => "Page is out of range.".into(),
            Self::InvalidPageRange { .. } => "Page range could not be parsed.".into(),
            Self::Unsupported { .. } => "Unsupported PDF feature.".into(),
            Self::OutputExists { .. } => "Output file already exists.".into(),
            Self::Config { .. } => "Invalid configuration.".into(),
            Self::Cancelled => "Operation cancelled.".into(),
            Self::LimitExceeded { .. } => "Resource limit exceeded.".into(),
            Self::Backend { backend, .. } => format!("The {backend} backend failed."),
        }
    }

    /// The detail line: what specifically went wrong.
    #[must_use]
    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Io { source, .. } => Some(source.to_string()),
            Self::Parse { reason, .. } => Some(reason.to_string()),
            Self::PasswordRequired | Self::WrongPassword | Self::Cancelled => None,
            Self::PermissionDenied { .. } => None,
            Self::PageOutOfRange { requested, pages } => Some(format!(
                "Requested page {requested}; the document has {pages}."
            )),
            Self::InvalidPageRange { spec } => Some(format!("Could not parse {spec:?}.")),
            Self::Unsupported { feature } => Some(feature.clone()),
            Self::OutputExists { path } => Some(format!("{} is already there.", path.display())),
            Self::Config { detail, .. } => Some(detail.clone()),
            Self::LimitExceeded { limit } => Some(limit.to_string()),
            Self::Backend { detail, .. } => Some(detail.clone()),
        }
    }

    /// What the user can actually do about it. Errors must be actionable.
    #[must_use]
    pub fn suggestion(&self) -> Option<String> {
        match self {
            Self::Io { .. } => {
                Some("Check that the path exists and that you have permission to access it.".into())
            }
            Self::Parse { .. } => Some(
                "Try \"Repair PDF\", or inspect the file using:\n\n    ypdf diagnostics <file>"
                    .into(),
            ),
            Self::PasswordRequired => Some("Supply the open password with --password.".into()),
            Self::WrongPassword => {
                Some("Check the password. Owner and open passwords are different.".into())
            }
            Self::PermissionDenied { .. } => {
                Some("Supply the owner password to override the permission flags.".into())
            }
            Self::PageOutOfRange { .. } | Self::InvalidPageRange { .. } => {
                Some("Page ranges look like 1-20, 3, or 5-. Pages are 1-based.".into())
            }
            Self::Unsupported { .. } => {
                Some("Run \"ypdf diagnostics <file>\" for the full feature report.".into())
            }
            Self::OutputExists { .. } => {
                Some("Pass --overwrite to replace it, or choose another path.".into())
            }
            Self::Config { source_path, .. } => Some(match source_path {
                Some(p) => format!("Check the configuration file: {}", p.display()),
                None => "Check the configuration values.".into(),
            }),
            Self::Cancelled => None,
            Self::LimitExceeded { .. } => {
                Some("Raise the limit in [engine] configuration, or process fewer files.".into())
            }
            Self::Backend { .. } => {
                Some("Re-run with --verbose for the full backend diagnostic.".into())
            }
        }
    }

    /// Process exit code for the CLI (spec §22).
    ///
    /// Distinct codes so shell scripts can branch without parsing text.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Cancelled => 130, // conventional SIGINT code
            Self::Io { .. } => 2,
            Self::Parse { .. } => 3,
            Self::PasswordRequired | Self::WrongPassword => 4,
            Self::PermissionDenied { .. } => 5,
            Self::PageOutOfRange { .. } | Self::InvalidPageRange { .. } => 6,
            Self::Unsupported { .. } => 7,
            Self::Config { .. } => 8,
            Self::OutputExists { .. } => 11,
            Self::LimitExceeded { .. } => 9,
            Self::Backend { .. } => 10,
        }
    }

    /// The path involved, if any.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Io { path, .. } | Self::Parse { path, .. } => path.as_deref(),
            Self::Config { source_path, .. } => source_path.as_deref(),
            Self::OutputExists { path } => Some(path),
            _ => None,
        }
    }

    /// Structured form for JSON output and logging.
    #[must_use]
    pub fn report(&self) -> ErrorReport {
        ErrorReport {
            code: self.code(),
            message: self.message(),
            reason: self.reason(),
            suggestion: self.suggestion(),
            path: self.path().map(|p| p.display().to_string()),
        }
    }

    /// Attach a path to an I/O error raised by a helper that did not know it.
    #[must_use]
    pub fn with_path(self, path: impl Into<PathBuf>) -> Self {
        match self {
            Self::Io { source, .. } => Self::Io {
                path: Some(path.into()),
                source,
            },
            Self::Parse { reason, .. } => Self::Parse {
                reason,
                path: Some(path.into()),
            },
            other => other,
        }
    }

    /// Convenience constructor for filesystem failures.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: Some(path.into()),
            source,
        }
    }

    /// Convenience constructor for parse failures.
    pub fn parse(reason: ParseFailure) -> Self {
        Self::Parse { reason, path: None }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.reason() {
            Some(reason) => write!(f, "{} {reason}", self.message()),
            None => f.write_str(&self.message()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::Io { path: None, source }
    }
}

/// Serializable projection of an [`Error`], for `--json`, the API, and logs.
#[derive(Clone, Debug, Serialize)]
pub struct ErrorReport {
    /// Stable code, e.g. `E_PARSE_XREF`.
    pub code: &'static str,
    /// Short headline.
    pub message: String,
    /// Specific cause, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Actionable next step, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    /// Path involved, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl ErrorReport {
    /// Reduce any path to its file name.
    ///
    /// Directory layout is user data. Logs get the redacted form by default
    /// (spec §35: no sensitive data in logs); terminal output does not.
    #[must_use]
    pub fn redacted(mut self) -> Self {
        self.path = self.path.map(|p| {
            Path::new(&p).file_name().map_or_else(
                || "<path>".to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        });
        self
    }

    /// The multi-line terminal rendering from spec §35.
    #[must_use]
    pub fn to_human(&self) -> String {
        let mut out = format!("ERROR [{}]: {}\n", self.code, self.message);
        if let Some(path) = &self.path {
            out.push_str(&format!("\nFile:\n{path}\n"));
        }
        if let Some(reason) = &self.reason {
            out.push_str(&format!("\nReason:\n{reason}\n"));
        }
        if let Some(suggestion) = &self.suggestion {
            out.push_str(&format!("\nSuggested action:\n{suggestion}\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_error_matches_the_spec_shape() {
        let err = Error::parse(ParseFailure::InvalidXref).with_path("document.pdf");
        let human = err.report().to_human();
        assert!(human.contains("PDF cannot be parsed."));
        assert!(human.contains("Invalid cross-reference table."));
        assert!(human.contains("ypdf diagnostics"));
        assert_eq!(err.code(), "E_PARSE_XREF");
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn redaction_strips_the_directory() {
        let err = Error::io(
            "C:/Users/someone/private/tax.pdf",
            std::io::Error::other("nope"),
        );
        let report = err.report().redacted();
        assert_eq!(report.path.as_deref(), Some("tax.pdf"));
    }

    #[test]
    fn every_variant_has_a_distinct_code() {
        let all = [
            Error::Io {
                path: None,
                source: std::io::Error::other("x"),
            },
            Error::parse(ParseFailure::BadHeader),
            Error::parse(ParseFailure::InvalidXref),
            Error::parse(ParseFailure::MissingTrailer),
            Error::parse(ParseFailure::CorruptStream { object: 1 }),
            Error::parse(ParseFailure::MissingObject {
                object: 1,
                generation: 0,
            }),
            Error::parse(ParseFailure::Other { detail: "x".into() }),
            Error::PasswordRequired,
            Error::WrongPassword,
            Error::PermissionDenied { action: "printing" },
            Error::PageOutOfRange {
                requested: 9,
                pages: 2,
            },
            Error::InvalidPageRange { spec: "x".into() },
            Error::Unsupported {
                feature: "x".into(),
            },
            Error::Config {
                detail: "x".into(),
                source_path: None,
            },
            Error::Cancelled,
            Error::LimitExceeded {
                limit: Limit::Memory { limit_mb: 1 },
            },
            Error::Backend {
                backend: "pdfium",
                detail: "x".into(),
            },
        ];
        let mut codes: Vec<_> = all.iter().map(Error::code).collect();
        let total = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), total, "duplicate error codes");
    }
}
