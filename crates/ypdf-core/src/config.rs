//! Layered configuration (spec §32).
//!
//! Five sources, lowest precedence first:
//!
//! 1. system config   — `%PROGRAMDATA%\ypdf\config.toml` / `/etc/ypdf/config.toml`
//! 2. user config     — the platform config directory
//! 3. project config  — `config.toml` or `ypdf.toml` beside the workspace
//! 4. environment     — `YPDF_*`
//! 5. CLI arguments   — supplied by the caller
//!
//! Every source parses into a [`ConfigPatch`] of `Option` fields; patches are
//! applied in order onto [`Config::default`]. A source only overrides what it
//! actually sets, so a user config with one key does not wipe the rest.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Where a configuration layer came from. Kept for diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    /// Compiled-in defaults.
    Defaults,
    /// System-wide file.
    System(PathBuf),
    /// Per-user file.
    User(PathBuf),
    /// Per-project file.
    Project(PathBuf),
    /// `YPDF_*` environment variables.
    Environment,
    /// Command-line arguments.
    Cli,
}

/// How logs are rendered (spec §31).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    /// Human-readable, for terminals.
    #[default]
    Pretty,
    /// One JSON object per line, for machines.
    Json,
}

/// Engine limits and parallelism (spec §25).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Engine {
    /// Rayon worker count. Never affects the single render thread.
    pub workers: usize,
    /// Soft memory ceiling for a single operation.
    pub max_memory_mb: u64,
    /// Inputs larger than this are refused (spec §24).
    pub max_file_size_mb: u64,
    /// Wall-clock ceiling for a single operation (spec §24).
    pub max_processing_secs: u64,
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            workers: std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get),
            max_memory_mb: 4096,
            max_file_size_mb: 4096,
            max_processing_secs: 900,
        }
    }
}

/// OCR settings (spec §7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Ocr {
    /// Tesseract language spec, e.g. `"eng+msa"`.
    pub language: String,
    /// Where language packs live. `None` means the Tesseract default.
    pub data_dir: Option<PathBuf>,
}

impl Default for Ocr {
    fn default() -> Self {
        Self {
            language: "eng".into(),
            data_dir: None,
        }
    }
}

/// Security scanner behaviour (spec §26).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Security {
    /// Inspect embedded files, not just list them.
    pub scan_embedded_files: bool,
    /// Detect JavaScript and auto-actions.
    pub scan_javascript: bool,
    /// Collect and classify external URLs.
    pub scan_urls: bool,
}

impl Default for Security {
    fn default() -> Self {
        Self {
            scan_embedded_files: true,
            scan_javascript: true,
            scan_urls: true,
        }
    }
}

/// Logging settings (spec §31).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Logging {
    /// `error` | `warn` | `info` | `debug` | `trace`.
    pub level: String,
    /// Pretty or JSON.
    pub format: LogFormat,
}

impl Default for Logging {
    fn default() -> Self {
        Self {
            level: "info".into(),
            format: LogFormat::Pretty,
        }
    }
}

/// Cache settings (spec §34).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Cache {
    /// Cache root. `None` means the platform cache directory.
    pub dir: Option<PathBuf>,
    /// Ceiling for the on-disk cache.
    pub max_size_mb: u64,
    /// Ceiling for in-memory rendered page textures.
    pub texture_budget_mb: u64,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            dir: None,
            max_size_mb: 2048,
            texture_budget_mb: 512,
        }
    }
}

/// Privacy settings (spec §27). Defaults are the private ones.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Privacy {
    /// Off by default, and there is nothing to send it to.
    pub telemetry: bool,
    /// Overwrite temporary files before unlinking them.
    pub secure_delete_temp: bool,
    /// Include full paths in logs. Off by default (spec §35).
    pub log_full_paths: bool,
}

/// Fully resolved configuration. Every field has a value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Config {
    /// Engine limits and parallelism.
    pub engine: Engine,
    /// OCR settings.
    pub ocr: Ocr,
    /// Security scanner settings.
    pub security: Security,
    /// Logging settings.
    pub logging: Logging,
    /// Cache settings.
    pub cache: Cache,
    /// Privacy settings.
    pub privacy: Privacy,
}

impl Config {
    /// Resolve all five layers.
    ///
    /// `project_dir` is searched for `config.toml` then `ypdf.toml`.
    /// `cli` carries whatever the caller parsed from arguments.
    /// Missing files are not an error; malformed ones are.
    pub fn load(project_dir: Option<&Path>, cli: ConfigPatch) -> Result<Self> {
        let mut config = Self::default();

        for path in Self::system_paths() {
            Self::apply_file(&mut config, &path)?;
        }
        if let Some(path) = Self::user_path() {
            Self::apply_file(&mut config, &path)?;
        }
        if let Some(dir) = project_dir {
            for name in ["config.toml", "ypdf.toml"] {
                Self::apply_file(&mut config, &dir.join(name))?;
            }
        }
        ConfigPatch::from_env()?.apply(&mut config);
        cli.apply(&mut config);

        config.validate()?;
        Ok(config)
    }

    /// Resolve without touching the filesystem or environment. For tests.
    #[must_use]
    pub fn from_patch(patch: ConfigPatch) -> Self {
        let mut config = Self::default();
        patch.apply(&mut config);
        config
    }

    /// Parse one file and apply it, ignoring absence.
    fn apply_file(config: &mut Self, path: &Path) -> Result<()> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                ConfigPatch::from_toml(&text, path)?.apply(config);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(Error::io(path, source)),
        }
    }

    fn system_paths() -> Vec<PathBuf> {
        if cfg!(windows) {
            std::env::var_os("PROGRAMDATA")
                .map(|d| vec![PathBuf::from(d).join("ypdf").join("config.toml")])
                .unwrap_or_default()
        } else {
            vec![PathBuf::from("/etc/ypdf/config.toml")]
        }
    }

    fn user_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("dev", "ypdf", "ypdf")
            .map(|d| d.config_dir().join("config.toml"))
    }

    /// Reject values that would break the engine.
    pub fn validate(&self) -> Result<()> {
        let bad = |detail: &str| Error::Config {
            detail: detail.into(),
            source_path: None,
        };

        if self.engine.workers == 0 {
            return Err(bad("engine.workers must be at least 1"));
        }
        if self.engine.max_memory_mb < 128 {
            return Err(bad("engine.max_memory_mb must be at least 128"));
        }
        const LEVELS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];
        if !LEVELS.contains(&self.logging.level.as_str()) {
            return Err(bad(
                "logging.level must be one of: error, warn, info, debug, trace",
            ));
        }
        if self.ocr.language.trim().is_empty() {
            return Err(bad("ocr.language must not be empty"));
        }
        Ok(())
    }
}

macro_rules! patch_section {
    ($(#[$meta:meta])* $name:ident => $target:ty { $($field:ident : $ty:ty),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            $(
                /// Overrides the corresponding field when set.
                pub $field: Option<$ty>,
            )*
        }

        impl $name {
            fn apply(self, target: &mut $target) {
                $(
                    if let Some(v) = self.$field {
                        target.$field = v;
                    }
                )*
            }
        }
    };
}

patch_section!(
    /// Partial [`Engine`] settings.
    EnginePatch => Engine {
        workers: usize,
        max_memory_mb: u64,
        max_file_size_mb: u64,
        max_processing_secs: u64,
    }
);
patch_section!(
    /// Partial [`Security`] settings.
    SecurityPatch => Security {
        scan_embedded_files: bool,
        scan_javascript: bool,
        scan_urls: bool,
    }
);
patch_section!(
    /// Partial [`Logging`] settings.
    LoggingPatch => Logging { level: String, format: LogFormat }
);
patch_section!(
    /// Partial [`Privacy`] settings.
    PrivacyPatch => Privacy {
        telemetry: bool,
        secure_delete_temp: bool,
        log_full_paths: bool,
    }
);

// The two sections holding `Option<PathBuf>` targets are written by hand: the
// macro assigns the unwrapped value, which is wrong for an already-optional
// field. "Set" here means "set to Some", never "clear".

/// Partial [`Ocr`] settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrPatch {
    /// Overrides `ocr.language` when set.
    pub language: Option<String>,
    /// Overrides `ocr.data_dir` when set.
    pub data_dir: Option<PathBuf>,
}

/// Partial [`Cache`] settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachePatch {
    /// Overrides `cache.dir` when set.
    pub dir: Option<PathBuf>,
    /// Overrides `cache.max_size_mb` when set.
    pub max_size_mb: Option<u64>,
    /// Overrides `cache.texture_budget_mb` when set.
    pub texture_budget_mb: Option<u64>,
}

impl OcrPatch {
    fn apply_to(self, target: &mut Ocr) {
        if let Some(v) = self.language {
            target.language = v;
        }
        if let Some(v) = self.data_dir {
            target.data_dir = Some(v);
        }
    }
}

impl CachePatch {
    fn apply_to(self, target: &mut Cache) {
        if let Some(v) = self.dir {
            target.dir = Some(v);
        }
        if let Some(v) = self.max_size_mb {
            target.max_size_mb = v;
        }
        if let Some(v) = self.texture_budget_mb {
            target.texture_budget_mb = v;
        }
    }
}

/// One configuration layer: everything optional.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigPatch {
    /// Partial `[engine]` section.
    #[serde(default)]
    pub engine: EnginePatch,
    /// Partial `[ocr]` section.
    #[serde(default)]
    pub ocr: OcrPatch,
    /// Partial `[security]` section.
    #[serde(default)]
    pub security: SecurityPatch,
    /// Partial `[logging]` section.
    #[serde(default)]
    pub logging: LoggingPatch,
    /// Partial `[cache]` section.
    #[serde(default)]
    pub cache: CachePatch,
    /// Partial `[privacy]` section.
    #[serde(default)]
    pub privacy: PrivacyPatch,
}

impl ConfigPatch {
    /// Parse a TOML layer. `path` is only used for the error message.
    pub fn from_toml(text: &str, path: impl Into<PathBuf>) -> Result<Self> {
        toml::from_str(text).map_err(|e| Error::Config {
            detail: e.to_string(),
            source_path: Some(path.into()),
        })
    }

    /// Read the `YPDF_*` environment variables.
    pub fn from_env() -> Result<Self> {
        let mut patch = Self::default();

        patch.engine.workers = env_parse("YPDF_WORKERS")?;
        patch.engine.max_memory_mb = env_parse("YPDF_MAX_MEMORY_MB")?;
        patch.engine.max_file_size_mb = env_parse("YPDF_MAX_FILE_SIZE_MB")?;
        patch.ocr.language = std::env::var("YPDF_OCR_LANG").ok();
        patch.ocr.data_dir = std::env::var_os("YPDF_OCR_DATA_DIR").map(PathBuf::from);
        patch.cache.dir = std::env::var_os("YPDF_CACHE_DIR").map(PathBuf::from);
        patch.logging.level = std::env::var("YPDF_LOG_LEVEL")
            .ok()
            .map(|v| v.to_lowercase());
        patch.privacy.telemetry = env_bool("YPDF_TELEMETRY")?;

        patch.logging.format = match std::env::var("YPDF_LOG_FORMAT").ok().as_deref() {
            None => None,
            Some("json") => Some(LogFormat::Json),
            Some("pretty") => Some(LogFormat::Pretty),
            Some(other) => {
                return Err(Error::Config {
                    detail: format!(
                        "YPDF_LOG_FORMAT must be \"pretty\" or \"json\", got {other:?}"
                    ),
                    source_path: None,
                });
            }
        };

        Ok(patch)
    }

    /// Apply this layer over `config`.
    pub fn apply(self, config: &mut Config) {
        self.engine.apply(&mut config.engine);
        self.ocr.apply_to(&mut config.ocr);
        self.security.apply(&mut config.security);
        self.logging.apply(&mut config.logging);
        self.cache.apply_to(&mut config.cache);
        self.privacy.apply(&mut config.privacy);
    }
}

fn env_parse<T>(key: &str) -> Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Err(_) => Ok(None),
        Ok(raw) => raw
            .trim()
            .parse::<T>()
            .map(Some)
            .map_err(|e| Error::Config {
                detail: format!("{key}: {e}"),
                source_path: None,
            }),
    }
}

fn env_bool(key: &str) -> Result<Option<bool>> {
    match std::env::var(key) {
        Err(_) => Ok(None),
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(Some(true)),
            "0" | "false" | "no" | "off" => Ok(Some(false)),
            other => Err(Error::Config {
                detail: format!("{key} must be a boolean, got {other:?}"),
                source_path: None,
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_only_overrides_what_it_sets() {
        let patch = ConfigPatch::from_toml("[engine]\nworkers = 2\n", "test.toml").expect("parses");
        let config = Config::from_patch(patch);
        assert_eq!(config.engine.workers, 2);
        assert_eq!(config.engine.max_memory_mb, Engine::default().max_memory_mb);
        assert_eq!(config.ocr.language, "eng");
    }

    #[test]
    fn later_layers_win() {
        let mut config = Config::default();
        ConfigPatch::from_toml("[ocr]\nlanguage = \"eng\"\n", "a")
            .expect("a")
            .apply(&mut config);
        ConfigPatch::from_toml("[ocr]\nlanguage = \"eng+msa\"\n", "b")
            .expect("b")
            .apply(&mut config);
        assert_eq!(config.ocr.language, "eng+msa");
    }

    #[test]
    fn spec_example_config_parses() {
        // Verbatim from spec §32.
        let text = "\
[engine]
workers = 8
max_memory_mb = 4096

[ocr]
language = \"eng+msa\"

[security]
scan_embedded_files = true

[logging]
level = \"info\"
format = \"json\"
";
        let config = Config::from_patch(ConfigPatch::from_toml(text, "spec").expect("parses"));
        assert_eq!(config.engine.workers, 8);
        assert_eq!(config.ocr.language, "eng+msa");
        assert_eq!(config.logging.format, LogFormat::Json);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        // A typo in a config file should not be silently ignored.
        let err = ConfigPatch::from_toml("[engine]\nworkerz = 8\n", "typo.toml");
        assert!(err.is_err());
    }

    #[test]
    fn validation_catches_bad_values() {
        let config = Config::from_patch(
            ConfigPatch::from_toml("[engine]\nworkers = 0\n", "x").expect("parses"),
        );
        assert!(config.validate().is_err());

        let config = Config::from_patch(
            ConfigPatch::from_toml("[logging]\nlevel = \"loud\"\n", "x").expect("parses"),
        );
        assert!(config.validate().is_err());
    }

    #[test]
    fn privacy_defaults_are_private() {
        let config = Config::default();
        assert!(!config.privacy.telemetry);
        assert!(!config.privacy.log_full_paths);
    }
}
