//! The command line itself (spec §22).
//!
//! Two conventions run through every command:
//!
//! * **Inputs are globs.** Windows shells do not expand `*.pdf`, so the tool
//!   does it itself rather than working on some platforms and not others.
//! * **Nothing overwrites without permission.** Every command that writes
//!   refuses an existing file unless `--overwrite` is given. A batch run over a
//!   thousand files is exactly where a silent overwrite is unrecoverable.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// yPDF — a privacy-first PDF toolkit. Nothing leaves the machine.
#[derive(Debug, Parser)]
#[command(name = "ypdf-cli", version, about, long_about = None)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,

    /// Options that apply to every command.
    #[command(flatten)]
    pub global: Global,
}

/// Options shared by every command.
#[derive(Clone, Debug, Args)]
pub struct Global {
    /// Machine-readable JSON on stdout instead of text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Only errors. Overrides `--verbose`.
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// More detail, including per-file progress.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Configuration file to load on top of the usual layers.
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Replace output files that already exist.
    #[arg(long, global = true)]
    pub overwrite: bool,

    /// How many files to work on at once. Defaults to the configured value.
    #[arg(long, global = true, value_name = "N")]
    pub workers: Option<usize>,

    /// Password for encrypted inputs. Either the user or the owner password.
    ///
    /// A password on a command line is visible to anything that can list
    /// processes, and lands in the shell history. `YPDF_PASSWORD` is read when
    /// this is not given, which is the better habit for a script.
    #[arg(long, global = true, value_name = "PASSWORD", env = "YPDF_PASSWORD")]
    pub password: Option<String>,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Summarize a document: pages, size, version, encryption.
    Info(InputsArgs),

    /// Full structural report, including problems found (spec §14).
    Diagnostics(InputsArgs),

    /// Look for JavaScript, launch actions, embedded files, and links (spec §26).
    ///
    /// Nothing found is executed, followed, or fetched.
    SecurityScan(SecurityScanArgs),

    /// Read and check digital signatures (spec §9).
    ///
    /// Reports what each signature covers and whether it verifies. It never
    /// says a signature is trusted: there is no trust store here, and
    /// revocation checking needs network access this tool does not have.
    Signatures(SignaturesArgs),

    /// Check a document against PDF/A (spec §15).
    ///
    /// Exits non-zero when the check fails, so it can gate a pipeline. What is
    /// checked, and what is not, is printed with every report.
    Pdfa(PdfaArgs),

    /// Read or edit document metadata (spec §13).
    Metadata(MetadataArgs),

    /// Join several documents into one (spec §3.1).
    Merge(MergeArgs),

    /// Cut a document into several (spec §3.2).
    Split(SplitArgs),

    /// Write out only the pages named (spec §3.3).
    Extract(ExtractArgs),

    /// Make files smaller (spec §4).
    Compress(CompressArgs),

    /// Write every embedded image out as a file (spec §6).
    Images(ImagesArgs),

    /// Protect a document with a password and permission flags (spec §8).
    Encrypt(EncryptArgs),

    /// Write a protected document out without its protection (spec §8).
    ///
    /// Needs the password: this removes protection you can already lift.
    Decrypt(DecryptArgs),

    /// List, add, or remove annotations (spec §10).
    Annotate(AnnotateArgs),

    /// List, fill, clear, or flatten form fields (spec §16).
    Forms(FormsArgs),

    /// List or edit the outline (spec §17).
    Bookmarks(BookmarksArgs),

    /// List or edit the links on a page (spec §18).
    Links(LinksArgs),

    /// Remove content permanently (spec §11).
    ///
    /// Not a black rectangle: the glyphs come out of the content stream and
    /// the pixels out of the images.
    Redact(RedactArgs),

    /// Stamp pages with text or an image (spec §12).
    Watermark(WatermarkArgs),

    /// Add a searchable text layer to a scan (spec §7).
    ///
    /// The scan itself is not changed: the recognized words go into an
    /// invisible layer over the page.
    Ocr(OcrArgs),
}

impl Command {
    /// The name used in logs and JSON output.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Info(_) => "info",
            Self::Diagnostics(_) => "diagnostics",
            Self::SecurityScan(_) => "security-scan",
            Self::Signatures(_) => "signatures",
            Self::Pdfa(_) => "pdfa",
            Self::Metadata(_) => "metadata",
            Self::Merge(_) => "merge",
            Self::Split(_) => "split",
            Self::Extract(_) => "extract",
            Self::Compress(_) => "compress",
            Self::Images(_) => "images",
            Self::Encrypt(_) => "encrypt",
            Self::Decrypt(_) => "decrypt",
            Self::Ocr(_) => "ocr",
            Self::Watermark(_) => "watermark",
            Self::Redact(_) => "redact",
            Self::Annotate(_) => "annotate",
            Self::Forms(_) => "forms",
            Self::Bookmarks(_) => "bookmarks",
            Self::Links(_) => "links",
        }
    }
}

/// Commands that only read take any number of inputs.
#[derive(Clone, Debug, Args)]
pub struct InputsArgs {
    /// Files or globs, e.g. `./invoices/*.pdf`.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,
}

/// `security-scan`.
#[derive(Clone, Debug, Args)]
pub struct SecurityScanArgs {
    /// Files or globs.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// Exit non-zero when a finding reaches this severity.
    ///
    /// For a gate in a pipeline: the scan itself always reports everything.
    #[arg(long, value_name = "SEVERITY")]
    pub fail_on: Option<SeverityArg>,
}

/// `signatures`.
#[derive(Clone, Debug, Args)]
pub struct SignaturesArgs {
    /// Files or globs.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// Exit non-zero when a file carries no signature at all.
    ///
    /// Without this, only a signature that fails counts as a finding: plenty
    /// of documents are legitimately unsigned.
    #[arg(long)]
    pub require_signed: bool,
}

/// `pdfa`.
#[derive(Clone, Debug, Args)]
pub struct PdfaArgs {
    /// Files or globs.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// The level to check against, e.g. `2b`, `3u`, `1a`.
    ///
    /// Defaults to the level the file claims in its XMP. A file that claims
    /// nothing is checked against PDF/A-2b and told that it claims nothing.
    #[arg(long, value_name = "LEVEL")]
    pub level: Option<String>,
}

/// Severity as a command-line word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SeverityArg {
    /// Worth knowing.
    Info,
    /// Worth a look.
    Warning,
    /// Worth stopping for.
    High,
    /// Worth refusing.
    Critical,
}

impl From<SeverityArg> for ypdf_doc::Severity {
    fn from(value: SeverityArg) -> Self {
        match value {
            SeverityArg::Info => Self::Info,
            SeverityArg::Warning => Self::Warning,
            SeverityArg::High => Self::High,
            SeverityArg::Critical => Self::Critical,
        }
    }
}

/// `metadata`.
#[derive(Clone, Debug, Args)]
pub struct MetadataArgs {
    /// Files or globs.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// New title. An empty string removes the field.
    #[arg(long, value_name = "TEXT")]
    pub set_title: Option<String>,

    /// New author. An empty string removes the field.
    #[arg(long, value_name = "TEXT")]
    pub set_author: Option<String>,

    /// New subject. An empty string removes the field.
    #[arg(long, value_name = "TEXT")]
    pub set_subject: Option<String>,

    /// New keywords. An empty string removes the field.
    #[arg(long, value_name = "TEXT")]
    pub set_keywords: Option<String>,

    /// Remove `/Info` and the XMP packet entirely.
    #[arg(long, conflicts_with_all = ["set_title", "set_author", "set_subject", "set_keywords"])]
    pub strip: bool,

    /// Where to write. A directory for several inputs; defaults to editing in
    /// place, which needs `--overwrite`.
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,
}

impl MetadataArgs {
    /// Is this a read or a write?
    #[must_use]
    pub const fn is_edit(&self) -> bool {
        self.strip
            || self.set_title.is_some()
            || self.set_author.is_some()
            || self.set_subject.is_some()
            || self.set_keywords.is_some()
    }
}

/// `merge`.
#[derive(Clone, Debug, Args)]
pub struct MergeArgs {
    /// Files or globs, joined in the order given. A glob contributes its
    /// matches sorted by name, so `chapter-*.pdf` merges predictably.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// Where to write the joined document.
    #[arg(short, long, value_name = "FILE")]
    pub output: PathBuf,
}

/// `split`.
#[derive(Clone, Debug, Args)]
pub struct SplitArgs {
    /// The document to cut up.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Directory for the pieces. Created if it does not exist.
    #[arg(short, long, value_name = "DIR")]
    pub output: PathBuf,

    /// Ranges, e.g. `1-20,21-40`. One file per range.
    #[arg(long, value_name = "RANGES", group = "how")]
    pub pages: Option<String>,

    /// One file per fixed-size run of pages.
    #[arg(long, value_name = "N", group = "how")]
    pub every: Option<u32>,

    /// One file per page.
    #[arg(long, group = "how")]
    pub each_page: bool,

    /// One file per top-level bookmark, cut where each one points.
    #[arg(long, group = "how")]
    pub bookmarks: bool,
}

/// `extract`.
#[derive(Clone, Debug, Args)]
pub struct ExtractArgs {
    /// The document to take pages from.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Which pages, e.g. `1-10,20,last`.
    #[arg(long, value_name = "RANGES")]
    pub pages: String,

    /// Where to write.
    #[arg(short, long, value_name = "FILE")]
    pub output: PathBuf,
}

/// `compress`.
#[derive(Clone, Debug, Args)]
pub struct CompressArgs {
    /// Files or globs.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// Where to write: a file for one input, a directory for several.
    #[arg(short, long, value_name = "PATH")]
    pub output: PathBuf,

    /// A named preset from spec §29, e.g. "Maximum Compression".
    #[arg(long, value_name = "NAME")]
    pub preset: Option<String>,

    /// Target resolution for images that are drawn larger than it.
    #[arg(long, value_name = "DPI")]
    pub dpi: Option<u32>,

    /// JPEG quality 1-100. 100 resamples but never re-encodes.
    #[arg(long, value_name = "Q")]
    pub quality: Option<u8>,

    /// Also remove `/Info` and the XMP packet.
    #[arg(long)]
    pub strip_metadata: bool,
}

/// `images`.
#[derive(Clone, Debug, Args)]
pub struct ImagesArgs {
    /// Files or globs.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// Directory for the images. Created if it does not exist; each input gets
    /// a subdirectory named after it when there is more than one.
    #[arg(short, long, value_name = "DIR")]
    pub output: PathBuf,
}

/// `encrypt`.
#[derive(Clone, Debug, Args)]
pub struct EncryptArgs {
    /// Files or globs.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// Where to write: a file for one input, a directory for several.
    #[arg(short, long, value_name = "PATH")]
    pub output: PathBuf,

    /// Password needed to open the document at all.
    ///
    /// Without one, anyone can read the file and only the permission flags
    /// apply — which is a real choice, but rarely the one people mean.
    #[arg(long, value_name = "PASSWORD", env = "YPDF_USER_PASSWORD")]
    pub user_password: Option<String>,

    /// Password needed to change the protection or ignore the flags.
    /// Defaults to the user password.
    #[arg(long, value_name = "PASSWORD", env = "YPDF_OWNER_PASSWORD")]
    pub owner_password: Option<String>,

    /// Which cipher to use.
    #[arg(long, value_enum, default_value_t = crate::commands::protect::AlgorithmArg::Aes256)]
    pub algorithm: crate::commands::protect::AlgorithmArg,

    /// Disallow printing.
    #[arg(long)]
    pub no_print: bool,

    /// Allow only degraded-quality printing.
    #[arg(long)]
    pub no_high_quality_print: bool,

    /// Disallow editing the contents.
    #[arg(long)]
    pub no_modify: bool,

    /// Disallow copying text and graphics out.
    #[arg(long)]
    pub no_copy: bool,

    /// Disallow adding or changing annotations.
    #[arg(long)]
    pub no_annotate: bool,

    /// Disallow filling in form fields.
    #[arg(long)]
    pub no_forms: bool,

    /// Disallow inserting, rotating, or deleting pages.
    #[arg(long)]
    pub no_assemble: bool,
}

/// `decrypt`.
#[derive(Clone, Debug, Args)]
pub struct DecryptArgs {
    /// Files or globs.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// Where to write: a file for one input, a directory for several.
    #[arg(short, long, value_name = "PATH")]
    pub output: PathBuf,
}

/// `annotate`.
///
/// Areas are `page:x0,y0,x1,y1` in points from the bottom left, which is how a
/// PDF measures itself. Every one of these may be given more than once.
#[derive(Clone, Debug, Args)]
pub struct AnnotateArgs {
    /// Files or globs.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// Where to write, when editing. Defaults to editing in place, which needs
    /// `--overwrite`.
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Mark text with a translucent wash.
    #[arg(long, value_name = "AREA")]
    pub highlight: Vec<String>,

    /// Underline text.
    #[arg(long, value_name = "AREA")]
    pub underline: Vec<String>,

    /// Strike through text.
    #[arg(long, value_name = "AREA")]
    pub strike: Vec<String>,

    /// A sticky note, as `page:x,y=text`.
    #[arg(long, value_name = "NOTE")]
    pub note: Vec<String>,

    /// A box of text on the page, as `page:x0,y0,x1,y1=text`.
    #[arg(long, value_name = "BOX")]
    pub text_box: Vec<String>,

    /// A rectangle.
    #[arg(long, value_name = "AREA")]
    pub rect: Vec<String>,

    /// An ellipse inscribed in an area.
    #[arg(long, value_name = "AREA")]
    pub ellipse: Vec<String>,

    /// A line from the first corner of the area to the second.
    #[arg(long, value_name = "AREA")]
    pub line: Vec<String>,

    /// A stamp, as `page:x0,y0,x1,y1=WORD`.
    #[arg(long, value_name = "STAMP")]
    pub stamp: Vec<String>,

    /// Put an arrowhead on the end of each line.
    #[arg(long)]
    pub arrow: bool,

    /// The comment attached to what is added.
    #[arg(long, value_name = "TEXT")]
    pub comment: Option<String>,

    /// Who is making the marks.
    #[arg(long, value_name = "NAME")]
    pub author: Option<String>,

    /// Colour, as `#rrggbb`. Defaults to highlighter yellow.
    #[arg(long, value_name = "HEX")]
    pub colour: Option<String>,

    /// 0.0 to 1.0.
    #[arg(long, default_value_t = 1.0, value_name = "N")]
    pub opacity: f32,

    /// Stroke width in points.
    #[arg(long, default_value_t = 1.5, value_name = "PT")]
    pub width: f32,

    /// Remove every annotation.
    ///
    /// Form fields and links are not annotations for this purpose and are left
    /// alone.
    #[arg(long)]
    pub remove_all: bool,

    /// Remove the annotations one person made.
    #[arg(long, value_name = "NAME")]
    pub remove_author: Option<String>,
}

impl AnnotateArgs {
    /// Is this a read or a write?
    #[must_use]
    pub fn is_edit(&self) -> bool {
        self.remove_all
            || self.remove_author.is_some()
            || !self.highlight.is_empty()
            || !self.underline.is_empty()
            || !self.strike.is_empty()
            || !self.note.is_empty()
            || !self.text_box.is_empty()
            || !self.rect.is_empty()
            || !self.ellipse.is_empty()
            || !self.line.is_empty()
            || !self.stamp.is_empty()
    }
}

/// Which interchange format to use for form data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum FormatArg {
    /// JSON: an object of name to value.
    Json,
    /// FDF, the PDF-shaped one.
    Fdf,
    /// XFDF, the XML one.
    Xfdf,
}

impl From<FormatArg> for ypdf_forms::Format {
    fn from(value: FormatArg) -> Self {
        match value {
            FormatArg::Json => Self::Json,
            FormatArg::Fdf => Self::Fdf,
            FormatArg::Xfdf => Self::Xfdf,
        }
    }
}

/// `forms`.
#[derive(Clone, Debug, Args)]
pub struct FormsArgs {
    /// Files or globs.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// Where to write, when editing. Defaults to editing in place, which needs
    /// `--overwrite`.
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Set a field, as `name=value`. May be given more than once.
    #[arg(long, value_name = "NAME=VALUE")]
    pub fill: Vec<String>,

    /// Read values from a JSON, FDF, or XFDF file.
    #[arg(long, value_name = "FILE")]
    pub import: Option<PathBuf>,

    /// Write the current values out.
    #[arg(long, value_name = "FILE")]
    pub export: Option<PathBuf>,

    /// Force the interchange format, rather than guessing from the file name.
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub format: Option<FormatArg>,

    /// Empty every field before filling.
    #[arg(long)]
    pub clear: bool,

    /// Draw the fields into the page and remove the form.
    ///
    /// One-way: afterwards nobody can change the values by opening the file,
    /// which is the point, and there is no undoing it.
    #[arg(long)]
    pub flatten: bool,
}

impl FormsArgs {
    /// Is this a read or a write?
    ///
    /// Exporting is a read: it writes a data file, not the document.
    #[must_use]
    pub fn is_edit(&self) -> bool {
        self.clear || self.flatten || self.import.is_some() || !self.fill.is_empty()
    }
}

/// `bookmarks`.
#[derive(Clone, Debug, Args)]
pub struct BookmarksArgs {
    /// Files or globs.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// Where to write, when editing. Defaults to editing in place, which needs
    /// `--overwrite`.
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Add a bookmark, as `Title=page`. May be given more than once.
    #[arg(long, value_name = "TITLE=PAGE")]
    pub add: Vec<String>,

    /// Replace the whole outline with a JSON tree.
    ///
    /// The same shape this command prints with `--json`, so an outline can be
    /// read out, edited, and put back.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["add", "clear"])]
    pub set: Option<PathBuf>,

    /// Remove the outline entirely.
    #[arg(long, conflicts_with = "add")]
    pub clear: bool,
}

impl BookmarksArgs {
    /// Is this a read or a write?
    #[must_use]
    pub fn is_edit(&self) -> bool {
        self.clear || self.set.is_some() || !self.add.is_empty()
    }
}

/// `links`.
#[derive(Clone, Debug, Args)]
pub struct LinksArgs {
    /// Files or globs.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// Where to write, when editing. Defaults to editing in place, which needs
    /// `--overwrite`.
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Add a link, as `page:x0,y0,x1,y1=target`, where the target is a URL,
    /// `mailto:someone@example.org`, or `page:4`.
    #[arg(long, value_name = "LINK")]
    pub add: Vec<String>,

    /// Remove every link.
    #[arg(long)]
    pub remove_all: bool,

    /// Remove links that lead outside the document.
    ///
    /// Web addresses, email addresses, and links into other files — what you
    /// want gone before a document leaves the building.
    #[arg(long)]
    pub remove_external: bool,

    /// Remove links whose target contains this text.
    #[arg(long, value_name = "TEXT")]
    pub remove_matching: Option<String>,
}

impl LinksArgs {
    /// Is this a read or a write?
    #[must_use]
    pub fn is_edit(&self) -> bool {
        self.remove_all
            || self.remove_external
            || self.remove_matching.is_some()
            || !self.add.is_empty()
    }
}

/// `redact`.
#[derive(Clone, Debug, Args)]
pub struct RedactArgs {
    /// Files or globs.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// Where to write: a file for one input, a directory for several.
    #[arg(short, long, value_name = "PATH")]
    pub output: PathBuf,

    /// Text to find and remove. May be given more than once.
    #[arg(long, value_name = "TEXT")]
    pub find: Vec<String>,

    /// An area to clear, as `page:x0,y0,x1,y1` in points from the bottom left.
    #[arg(long, value_name = "AREA")]
    pub rect: Vec<String>,

    /// Which pages to search, e.g. `1-10`. Defaults to all of them.
    #[arg(long, value_name = "RANGES")]
    pub pages: Option<String>,

    /// Match `--find` exactly, including case.
    #[arg(long)]
    pub case_sensitive: bool,

    /// Match `--find` only as whole words.
    #[arg(long)]
    pub whole_words: bool,

    /// Do not draw a box over the cleared areas.
    ///
    /// The box is not the redaction — the content is gone either way — but
    /// without it a page has quietly lost a sentence, which is worse to read.
    #[arg(long)]
    pub no_cover: bool,

    /// Carry on when a `--find` term matches nothing.
    ///
    /// Off by default: being told "done" when the pattern was wrong is the
    /// most dangerous answer this command can give.
    #[arg(long)]
    pub allow_no_matches: bool,
}

/// Where a watermark sits on the page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum PositionArg {
    /// The middle of the page.
    #[default]
    Center,
    /// Top left.
    TopLeft,
    /// Top centre.
    TopCenter,
    /// Top right.
    TopRight,
    /// Bottom left.
    BottomLeft,
    /// Bottom centre.
    BottomCenter,
    /// Bottom right.
    BottomRight,
    /// Repeated across the page, so it cannot be cropped off.
    Tiled,
}

/// `watermark`.
#[derive(Clone, Debug, Args)]
pub struct WatermarkArgs {
    /// Files or globs.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// Where to write: a file for one input, a directory for several.
    #[arg(short, long, value_name = "PATH")]
    pub output: PathBuf,

    /// The words to stamp, e.g. CONFIDENTIAL.
    #[arg(long, value_name = "TEXT", conflicts_with = "image")]
    pub text: Option<String>,

    /// An image to stamp: a logo, a signature, a scanned stamp.
    #[arg(long, value_name = "FILE")]
    pub image: Option<PathBuf>,

    /// Which pages, e.g. `1-10`. Defaults to all of them.
    #[arg(long, value_name = "RANGES")]
    pub pages: Option<String>,

    /// Where on the page it sits.
    #[arg(long, value_enum, default_value_t = PositionArg::Center)]
    pub position: PositionArg,

    /// Degrees anticlockwise. Text defaults to 45, an image to 0.
    #[arg(long, value_name = "DEGREES")]
    pub rotation: Option<f32>,

    /// 0.0 to 1.0. Faint enough to read the page through.
    #[arg(long, default_value_t = 0.15, value_name = "N")]
    pub opacity: f32,

    /// Size multiplier, applied after fitting to the page.
    #[arg(long, default_value_t = 1.0, value_name = "N")]
    pub scale: f32,

    /// Point size for text. Defaults to fitting the page.
    #[arg(long, value_name = "PT")]
    pub size: Option<f32>,

    /// Colour as `#rrggbb`.
    #[arg(long, default_value = "808080", value_name = "HEX")]
    pub colour: String,

    /// Use Helvetica rather than Helvetica-Bold.
    #[arg(long)]
    pub regular: bool,

    /// Draw behind the page instead of on top of it.
    ///
    /// What a letterhead or a background logo needs. Nothing on the page is
    /// obscured, but anything opaque already there hides the watermark.
    #[arg(long)]
    pub under: bool,
}

/// `ocr`.
#[derive(Clone, Debug, Args)]
pub struct OcrArgs {
    /// Files or globs.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// Where to write: a file for one input, a directory for several.
    #[arg(short, long, value_name = "PATH")]
    pub output: PathBuf,

    /// Tesseract language specification, e.g. `eng` or `eng+msa`.
    #[arg(long, default_value = "eng", value_name = "LANG")]
    pub lang: String,

    /// Resolution to rasterize pages at before recognition.
    ///
    /// 300 is the usual sweet spot: below about 200 accuracy falls off, and
    /// above 400 the extra pixels mostly cost time.
    #[arg(long, default_value_t = 300, value_name = "DPI")]
    pub dpi: u32,

    /// Which pages, e.g. `1-10`. Defaults to all of them.
    #[arg(long, value_name = "RANGES")]
    pub pages: Option<String>,

    /// Discard recognized words below this confidence, 0-100.
    #[arg(long, default_value_t = 40.0, value_name = "N")]
    pub min_confidence: f32,

    /// Recognize pages that already have text.
    ///
    /// Off by default: a page with real text does not need a guess laid over
    /// it, and adding one gives search two answers for the same words.
    #[arg(long)]
    pub force: bool,

    /// Path to the Tesseract binary, if it is somewhere unusual.
    #[arg(long, value_name = "PATH", env = "YPDF_TESSERACT_PATH")]
    pub tesseract: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_line_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn split_modes_are_mutually_exclusive() {
        let result = Cli::try_parse_from([
            "ypdf-cli",
            "split",
            "in.pdf",
            "-o",
            "out",
            "--each-page",
            "--bookmarks",
        ]);
        assert!(result.is_err(), "two ways to split at once must not parse");
    }

    #[test]
    fn stripping_metadata_conflicts_with_setting_it() {
        let result = Cli::try_parse_from([
            "ypdf-cli",
            "metadata",
            "in.pdf",
            "--strip",
            "--set-title",
            "x",
        ]);
        assert!(result.is_err(), "strip and set together are contradictory");
    }

    #[test]
    fn global_flags_are_accepted_after_the_subcommand() {
        // `ypdf-cli compress in.pdf -o out.pdf --json` is how people actually
        // type it, so the flags have to work in that position too.
        let cli = Cli::try_parse_from([
            "ypdf-cli", "compress", "in.pdf", "-o", "out.pdf", "--json", "--quiet",
        ])
        .expect("parses");
        assert!(cli.global.json);
        assert!(cli.global.quiet);
        assert_eq!(cli.command.name(), "compress");
    }

    #[test]
    fn metadata_without_a_set_flag_is_a_read() {
        let cli = Cli::try_parse_from(["ypdf-cli", "metadata", "in.pdf"]).expect("parses");
        let Command::Metadata(args) = cli.command else {
            panic!("wrong command");
        };
        assert!(!args.is_edit());
    }
}
