//! Human text on stdout, JSON on stdout, commentary on stderr.
//!
//! The split matters for scripting: `ypdf-cli info x.pdf --json | jq` must see
//! nothing but the JSON document, so progress, warnings, and per-file lines all
//! go to stderr. Redirecting stdout is then enough to capture the result.

use std::io::Write as _;

use serde_json::Value;
use ypdf_core::Error;

use crate::cli::Global;

/// Where output goes and how much of it there is.
#[derive(Clone, Debug)]
pub struct Out {
    json: bool,
    quiet: bool,
    verbose: bool,
}

impl Out {
    /// Take the settings from the parsed command line.
    #[must_use]
    pub const fn new(global: &Global) -> Self {
        Self {
            json: global.json,
            quiet: global.quiet,
            // Quiet wins: someone who asked for silence and left a --verbose in
            // a script meant silence.
            verbose: global.verbose && !global.quiet,
        }
    }

    /// Is JSON being produced?
    #[must_use]
    pub const fn is_json(&self) -> bool {
        self.json
    }

    /// Is extra detail wanted?
    #[must_use]
    pub const fn is_verbose(&self) -> bool {
        self.verbose
    }

    /// A line of human output. Suppressed entirely in JSON mode.
    pub fn human(&self, text: impl AsRef<str>) {
        if self.json || self.quiet {
            return;
        }
        println!("{}", text.as_ref());
    }

    /// The JSON document. Written once, at the end.
    pub fn json(&self, value: &Value) {
        if !self.json {
            return;
        }
        let mut stdout = std::io::stdout().lock();
        let _ = serde_json::to_writer_pretty(&mut stdout, value);
        let _ = stdout.write_all(b"\n");
    }

    /// Running commentary: progress, per-file lines. Always stderr.
    pub fn status(&self, text: impl AsRef<str>) {
        if self.quiet {
            return;
        }
        let _ = writeln!(std::io::stderr(), "{}", text.as_ref());
    }

    /// Detail only wanted with `--verbose`.
    pub fn note(&self, text: impl AsRef<str>) {
        if self.verbose {
            let _ = writeln!(std::io::stderr(), "{}", text.as_ref());
        }
    }

    /// An error, in the shape spec §14 asks for: what happened, why, and what
    /// to do about it. Errors are never suppressed by `--quiet`.
    pub fn error(&self, error: &Error) {
        let _ = writeln!(std::io::stderr(), "{}", error.report().to_human());
    }
}

/// An error as JSON, with its stable code so a script can branch on it.
///
/// [`ErrorReport`](ypdf_core::ErrorReport) already serializes; the exit code is
/// added because that is the other thing a script wants and it lives on the
/// error rather than the report.
#[must_use]
pub fn error_json(error: &Error) -> Value {
    let mut value = serde_json::to_value(error.report()).unwrap_or(Value::Null);
    if let Value::Object(object) = &mut value {
        object.insert("exit_code".into(), Value::from(error.exit_code()));
    }
    value
}

/// `1,248` — a count someone is going to read out loud.
#[must_use]
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_get_separators_where_a_reader_expects_them() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_248), "1,248");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn an_error_carries_its_stable_code_into_json() {
        let error = Error::Unsupported {
            feature: "linearization".into(),
        };
        let json = error_json(&error);
        assert_eq!(json["code"], Value::String(error.code().to_string()));
        assert_eq!(json["exit_code"], Value::from(7));
    }

    #[test]
    fn quiet_beats_verbose() {
        let global = Global {
            json: false,
            quiet: true,
            verbose: true,
            config: None,
            overwrite: false,
            workers: None,
            password: None,
        };
        assert!(!Out::new(&global).is_verbose());
    }
}
