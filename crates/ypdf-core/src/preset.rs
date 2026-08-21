//! Presets (spec §29).
//!
//! A preset is a named bundle of operation settings. The TOML shape is the one
//! in the spec, so a user's `Company Standard` file loads unchanged.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Name and description of a preset.
///
/// Defaulted so a preset file may omit `[preset]` entirely.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PresetMeta {
    /// Display name, e.g. `"Company Standard"`.
    pub name: String,
    /// Optional one-line explanation shown in the picker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Default for PresetMeta {
    fn default() -> Self {
        Self {
            name: "Custom".into(),
            description: None,
        }
    }
}

/// Compression settings (spec §4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Compression {
    /// Target raster resolution for downsampled images.
    pub dpi: u32,
    /// JPEG quality, 1-100.
    pub quality: u8,
    /// Re-encode PNG images with oxipng.
    pub optimize_png: bool,
    /// Drop objects nothing references.
    pub remove_unused_objects: bool,
    /// Re-compress content streams.
    pub compress_streams: bool,
    /// Subset embedded fonts.
    pub optimize_fonts: bool,
    /// Produce a linearized ("fast web view") file.
    pub linearize: bool,
}

impl Default for Compression {
    fn default() -> Self {
        Self {
            dpi: 150,
            quality: 80,
            optimize_png: true,
            remove_unused_objects: true,
            compress_streams: true,
            optimize_fonts: true,
            linearize: false,
        }
    }
}

/// Metadata handling for a preset (spec §13).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metadata {
    /// Strip document information and XMP.
    pub remove: bool,
}

/// Security actions bundled into a preset (spec §8, §26).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityActions {
    /// Run the security scanner as part of the operation.
    pub scan: bool,
}

impl Default for SecurityActions {
    fn default() -> Self {
        Self { scan: true }
    }
}

/// OCR settings bundled into a preset (spec §7).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OcrActions {
    /// Run OCR and add a searchable text layer.
    pub enabled: bool,
    /// Tesseract language spec; falls back to `[ocr] language` when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// A named bundle of operation settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    /// `[preset]`
    #[serde(default)]
    pub preset: PresetMeta,
    /// `[compression]`
    #[serde(default)]
    pub compression: Compression,
    /// `[metadata]`
    #[serde(default)]
    pub metadata: Metadata,
    /// `[security]`
    #[serde(default)]
    pub security: SecurityActions,
    /// `[ocr]`
    #[serde(default)]
    pub ocr: OcrActions,
}

impl Preset {
    /// Parse a preset from TOML. `path` is only used for the error message.
    pub fn from_toml(text: &str, path: impl Into<PathBuf>) -> Result<Self> {
        toml::from_str(text).map_err(|e| Error::Config {
            detail: e.to_string(),
            source_path: Some(path.into()),
        })
    }

    /// Read a preset file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        Self::from_toml(&text, path)
    }

    /// Serialize back to TOML, for saving a user preset.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| Error::Config {
            detail: e.to_string(),
            source_path: None,
        })
    }

    /// The preset's display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.preset.name
    }

    fn named(name: &str, description: &str) -> Self {
        Self {
            preset: PresetMeta {
                name: name.into(),
                description: Some(description.into()),
            },
            ..Self::default()
        }
    }

    /// The built-in presets from spec §29.
    #[must_use]
    pub fn builtin() -> Vec<Self> {
        vec![
            Self {
                compression: Compression {
                    dpi: 150,
                    quality: 75,
                    linearize: true,
                    ..Compression::default()
                },
                metadata: Metadata { remove: true },
                ..Self::named("Web PDF", "Small and linearized for fast web viewing.")
            },
            Self {
                compression: Compression {
                    dpi: 300,
                    quality: 95,
                    optimize_fonts: false,
                    ..Compression::default()
                },
                ..Self::named(
                    "Archive PDF",
                    "High fidelity, fonts kept whole, aimed at PDF/A.",
                )
            },
            Self {
                compression: Compression {
                    dpi: 110,
                    quality: 65,
                    ..Compression::default()
                },
                metadata: Metadata { remove: true },
                ..Self::named("Email PDF", "Aggressive size reduction for attachments.")
            },
            Self {
                compression: Compression {
                    dpi: 300,
                    quality: 92,
                    ..Compression::default()
                },
                ..Self::named(
                    "Print PDF",
                    "Print-resolution images, no visible artefacts.",
                )
            },
            Self {
                ocr: OcrActions {
                    enabled: true,
                    language: None,
                },
                compression: Compression {
                    dpi: 200,
                    quality: 85,
                    ..Compression::default()
                },
                ..Self::named("OCR Document", "Adds a searchable text layer to scans.")
            },
            Self {
                compression: Compression {
                    dpi: 72,
                    quality: 45,
                    ..Compression::default()
                },
                metadata: Metadata { remove: true },
                ..Self::named(
                    "Maximum Compression",
                    "Smallest possible file; visible quality loss.",
                )
            },
            Self::named(
                "Company Standard",
                "Starting point for an organisation-wide preset.",
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_example_preset_parses() {
        // Verbatim from spec §29.
        let text = "\
[preset]
name = \"Company Standard\"

[compression]
dpi = 150
quality = 80

[metadata]
remove = false

[security]
scan = true
";
        let preset = Preset::from_toml(text, "preset.toml").expect("parses");
        assert_eq!(preset.name(), "Company Standard");
        assert_eq!(preset.compression.dpi, 150);
        assert_eq!(preset.compression.quality, 80);
        assert!(!preset.metadata.remove);
        assert!(preset.security.scan);
    }

    #[test]
    fn builtins_have_unique_names_and_round_trip() {
        let builtin = Preset::builtin();
        assert_eq!(builtin.len(), 7);

        let mut names: Vec<_> = builtin.iter().map(Preset::name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), builtin.len());

        for preset in &builtin {
            let toml = preset.to_toml().expect("serializes");
            let parsed = Preset::from_toml(&toml, "roundtrip").expect("parses");
            assert_eq!(&parsed, preset);
        }
    }

    #[test]
    fn missing_sections_fall_back_to_defaults() {
        let preset = Preset::from_toml("[preset]\nname = \"Bare\"\n", "x").expect("parses");
        assert_eq!(preset.compression, Compression::default());
        assert!(preset.security.scan);
    }
}
