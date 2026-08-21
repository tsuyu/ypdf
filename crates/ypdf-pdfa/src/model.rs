//! What a PDF/A level is, and what a failure to meet one looks like.

use std::fmt;

use ypdf_core::{Error, Result};

/// The conformance level within a part.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Conformance {
    /// Accessible: everything in `B`, plus tagging and language.
    A,
    /// Basic: the page looks the same forever.
    B,
    /// Unicode: everything in `B`, plus text that can be extracted reliably.
    U,
}

impl Conformance {
    /// The single letter used in `pdfaid:conformance` and in names like `2b`.
    #[must_use]
    pub const fn letter(self) -> char {
        match self {
            Self::A => 'a',
            Self::B => 'b',
            Self::U => 'u',
        }
    }

    /// Does this level require every font to map back to Unicode?
    ///
    /// `U` says so directly; `A` needs it because a screen reader that cannot
    /// recover the characters cannot read the document out.
    #[must_use]
    pub const fn needs_unicode(self) -> bool {
        matches!(self, Self::A | Self::U)
    }
}

/// A PDF/A part and conformance level, e.g. PDF/A-2b.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    /// 1, 2, or 3.
    pub part: u8,
    /// The letter after the part.
    pub conformance: Conformance,
}

impl Level {
    /// Build a level, refusing combinations that do not exist.
    ///
    /// `PDF/A-1u` is the one people ask for and it has never existed: part 1
    /// has only `a` and `b`. Accepting it would mean validating against a
    /// standard nobody can conform to.
    pub fn new(part: u8, conformance: Conformance) -> Result<Self> {
        if !(1..=3).contains(&part) {
            return Err(Error::Config {
                detail: format!(
                    "PDF/A part {part} is not one this checks: the parts are 1, 2, and 3"
                ),
                source_path: None,
            });
        }
        if part == 1 && conformance == Conformance::U {
            return Err(Error::Config {
                detail: "PDF/A-1u does not exist: part 1 has conformance levels a and b"
                    .to_string(),
                source_path: None,
            });
        }
        Ok(Self { part, conformance })
    }

    /// Parse `2b`, `PDF/A-2B`, `3u`, and the like.
    pub fn parse(text: &str) -> Result<Self> {
        let cleaned: String = text
            .trim()
            .to_ascii_lowercase()
            .replace("pdf/a-", "")
            .replace("pdfa-", "")
            .replace("pdfa", "");
        let mut chars = cleaned.chars();
        let part = chars
            .next()
            .and_then(|c| c.to_digit(10))
            .and_then(|d| u8::try_from(d).ok());
        let letter = chars.next();

        let (Some(part), Some(letter)) = (part, letter) else {
            return Err(Error::Config {
                detail: format!("`{text}` is not a PDF/A level: try 1b, 2b, 2u, 2a, 3b, 3u, 3a"),
                source_path: None,
            });
        };
        let conformance = match letter {
            'a' => Conformance::A,
            'b' => Conformance::B,
            'u' => Conformance::U,
            other => {
                return Err(Error::Config {
                    detail: format!(
                        "`{other}` is not a PDF/A conformance level: they are a, b, and u"
                    ),
                    source_path: None,
                });
            }
        };
        Self::new(part, conformance)
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PDF/A-{}{}", self.part, self.conformance.letter())
    }
}

/// One requirement the document does not meet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// Stable code, for scripting against — never renumbered.
    pub code: &'static str,
    /// The requirement that was not met, named as the standard names it.
    ///
    /// Deliberately a phrase rather than a clause number: citing a clause is
    /// only useful if the number is right, and getting one wrong sends a
    /// reader to the wrong page of a standard they had to pay for.
    pub requirement: &'static str,
    /// What is wrong, in a sentence.
    pub message: String,
    /// How many objects share this problem, when that is meaningful.
    pub count: usize,
}

impl Violation {
    /// One violation, seen once.
    pub(crate) fn new(
        code: &'static str,
        requirement: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            requirement,
            message: message.into(),
            count: 1,
        }
    }

    /// One violation standing for `count` objects.
    pub(crate) fn counted(
        code: &'static str,
        requirement: &'static str,
        message: impl Into<String>,
        count: usize,
    ) -> Self {
        Self {
            count,
            ..Self::new(code, requirement, message)
        }
    }
}

/// The result of checking a document (spec §15).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The level the file claims in its XMP, if it claims one.
    pub claimed: Option<Level>,
    /// The level it was checked against.
    pub checked: Level,
    /// Did every check that ran pass?
    ///
    /// Read it with [`Report::limits`] in view: this is "nothing failed", not
    /// "a certified validator would pass it".
    pub passed: bool,
    /// Everything that failed, worst first.
    pub violations: Vec<Violation>,
}

/// What this checker does not check.
///
/// Kept next to the verdict on purpose. PDF/A has several hundred
/// requirements, and a tool that answers YES without saying which ones it
/// looked at is worse than no tool: it produces confident archives that fail
/// at the archive's door.
pub const LIMITS: &[&str] = &[
    "ICC output profiles are checked for presence, not parsed or validated",
    "font programs are checked for presence, not for glyph coverage, CIDSet completeness, \
     or symbolic TrueType encoding rules",
    "content-stream operators are not checked against the permitted set",
    "the structure tree is checked for presence, not for correct semantics or reading order",
    "XMP is read for the PDF/A identification and title, not validated against its schemas",
    "embedded files are checked for their own PDF/A identification, not validated in full",
    "halftones, transfer functions, and rendering intents are not checked",
    "PDF/A-4 (ISO 19005-4) is not covered",
];

impl Report {
    /// What this checker did not look at.
    #[must_use]
    pub const fn limits() -> &'static [&'static str] {
        LIMITS
    }

    /// The report as text, in the shape of spec §15.
    #[must_use]
    pub fn to_human(&self) -> String {
        let mut out = String::from("PDF/A CHECK\n\n");
        out.push_str(&format!(
            "Claimed:   {}\n",
            self.claimed.map_or_else(
                || "nothing — the file makes no PDF/A claim".to_string(),
                |l| l.to_string()
            )
        ));
        out.push_str(&format!("Checked:   {}\n", self.checked));
        out.push_str(&format!(
            "Result:    {}\n",
            if self.passed {
                "every check performed passed"
            } else {
                "FAILED"
            }
        ));

        if !self.violations.is_empty() {
            out.push_str("\nViolations:\n");
            for violation in &self.violations {
                let times = if violation.count > 1 {
                    format!(" ({} objects)", violation.count)
                } else {
                    String::new()
                };
                out.push_str(&format!(
                    "  [{}] {}{}\n      {}\n",
                    violation.code, violation.requirement, times, violation.message
                ));
            }
        }

        out.push_str("\nNot checked here:\n");
        for limit in LIMITS {
            out.push_str(&format!("  - {limit}\n"));
        }
        if self.passed {
            out.push_str(
                "\nPassing every check above is not a certificate of conformance. For that, \
                 run a validator that implements the whole standard.\n",
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_parse_the_way_people_write_them() {
        for text in ["2b", "2B", "PDF/A-2b", "pdfa-2b", "  pdf/a-2B  "] {
            let level = Level::parse(text).expect("parses");
            assert_eq!(level.part, 2);
            assert_eq!(level.conformance, Conformance::B);
        }
    }

    #[test]
    fn a_level_that_does_not_exist_is_refused() {
        // Part 1 never had a Unicode level; silently checking it as 1b would
        // report conformance to a standard that was not asked for.
        assert!(Level::parse("1u").is_err());
        assert!(Level::parse("4b").is_err());
        assert!(Level::parse("2c").is_err());
        assert!(Level::parse("").is_err());
    }

    #[test]
    fn a_level_prints_the_way_the_standard_names_it() {
        assert_eq!(Level::parse("3u").expect("parses").to_string(), "PDF/A-3u");
    }

    #[test]
    fn the_report_always_says_what_it_did_not_check() {
        let report = Report {
            claimed: None,
            checked: Level::parse("2b").expect("parses"),
            passed: true,
            violations: Vec::new(),
        };
        let text = report.to_human();
        assert!(text.contains("Not checked here:"));
        assert!(text.contains("not a certificate of conformance"));
    }
}
