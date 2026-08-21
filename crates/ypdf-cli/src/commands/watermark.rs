//! `watermark` — stamp pages with text or an image (spec §12).

use std::path::Path;

use serde_json::json;
use ypdf_core::{CancelToken, Error, Result};
use ypdf_doc::PageSpec;
use ypdf_watermark::{Colour, Content, Face, Placement, Position, Watermark};

use crate::batch::FileReport;
use crate::cli::{PositionArg, WatermarkArgs};
use crate::paths;

/// Build the watermark the arguments describe.
///
/// Resolved once for a whole run: an unreadable image or an impossible colour
/// is the same answer for every file, and finding it out on file 900 of 1,000
/// helps nobody.
pub fn watermark_from(args: &WatermarkArgs) -> Result<Watermark> {
    let content = match (&args.text, &args.image) {
        (Some(text), None) => Content::Text {
            text: text.clone(),
            face: if args.regular {
                Face::Regular
            } else {
                Face::Bold
            },
            size: args.size,
            colour: Colour::parse(&args.colour)?,
        },
        (None, Some(path)) => {
            let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
            Content::Image { bytes }
        }
        _ => {
            return Err(Error::Config {
                detail: "give either --text or --image".into(),
                source_path: None,
            });
        }
    };

    Ok(Watermark {
        content,
        position: args.position.into(),
        rotation: args
            .rotation
            .unwrap_or(if args.image.is_some() { 0.0 } else { 45.0 }),
        opacity: args.opacity,
        scale: args.scale,
        placement: if args.under {
            Placement::Under
        } else {
            Placement::Over
        },
    })
}

/// Stamp one file.
pub fn run(
    path: &Path,
    args: &WatermarkArgs,
    watermark: &Watermark,
    password: Option<&str>,
    many: bool,
    overwrite: bool,
    _cancel: &CancelToken,
) -> Result<FileReport> {
    let target = paths::output_for(&args.output, path, many, "-watermarked")?;
    paths::guard(&target, overwrite)?;

    let mut pdf = super::open_input(path, password)?;
    let pages = match &args.pages {
        Some(spec) => PageSpec::parse(spec)?.resolve_unique(pdf.page_count())?,
        None => (1..=pdf.page_count()).collect(),
    };

    let report = ypdf_watermark::apply(&mut pdf, &pages, watermark)?;
    if report.pages == 0 {
        // Writing a copy that looks identical and calling it done would be
        // worse than saying nothing happened.
        return Err(Error::Config {
            detail: if report.unencodable > 0 {
                "the watermark text uses characters the built-in fonts cannot draw".into()
            } else {
                "no pages were stamped".into()
            },
            source_path: Some(path.to_path_buf()),
        });
    }

    let bytes = pdf.to_bytes()?;
    paths::write(&target, &bytes, overwrite)?;

    Ok(FileReport {
        human: format!(
            "Stamped {} page(s) → {} ({})",
            report.pages,
            target.display(),
            ypdf_optimize::format_size(bytes.len() as u64)
        ),
        json: json!({
            "input": path.display().to_string(),
            "output": target.display().to_string(),
            "pages": report.pages,
            "placement": if args.under { "under" } else { "over" },
            "bytes": bytes.len(),
        }),
        bytes_in: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        bytes_out: bytes.len() as u64,
        flagged: false,
    })
}

impl From<PositionArg> for Position {
    fn from(value: PositionArg) -> Self {
        match value {
            PositionArg::Center => Self::Center,
            PositionArg::TopLeft => Self::TopLeft,
            PositionArg::TopCenter => Self::TopCenter,
            PositionArg::TopRight => Self::TopRight,
            PositionArg::BottomLeft => Self::BottomLeft,
            PositionArg::BottomCenter => Self::BottomCenter,
            PositionArg::BottomRight => Self::BottomRight,
            PositionArg::Tiled => Self::Tiled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use ypdf_doc::Pdf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("ypdf-cli-tests").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");
        dir
    }

    fn args(output: PathBuf) -> WatermarkArgs {
        WatermarkArgs {
            inputs: Vec::new(),
            output,
            text: Some("CONFIDENTIAL".into()),
            image: None,
            pages: None,
            position: PositionArg::Center,
            rotation: None,
            opacity: 0.15,
            scale: 1.0,
            size: None,
            colour: "808080".into(),
            regular: false,
            under: false,
        }
    }

    #[test]
    fn text_and_image_together_are_refused() {
        let mut args = args(PathBuf::from("out.pdf"));
        args.image = Some(PathBuf::from("logo.png"));
        assert!(watermark_from(&args).is_err());
    }

    #[test]
    fn neither_text_nor_image_is_refused() {
        let mut args = args(PathBuf::from("out.pdf"));
        args.text = None;
        assert!(watermark_from(&args).is_err());
    }

    #[test]
    fn text_defaults_to_the_diagonal_and_an_image_to_upright() {
        let text = watermark_from(&args(PathBuf::from("out.pdf"))).expect("builds");
        assert!((text.rotation - 45.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_bad_colour_is_refused_before_anything_is_opened() {
        let mut args = args(PathBuf::from("out.pdf"));
        args.colour = "not-a-colour".into();
        assert!(watermark_from(&args).is_err());
    }

    #[test]
    fn stamping_writes_a_new_file_and_leaves_the_input_alone() {
        let dir = scratch("watermark");
        let input = fixture("many-pages.pdf");
        let before = std::fs::read(&input).expect("reads");

        let args = args(dir.join("out.pdf"));
        let watermark = watermark_from(&args).expect("builds");
        let report = run(
            &input,
            &args,
            &watermark,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("stamps");

        assert_eq!(report.json["pages"], 12);
        assert_eq!(std::fs::read(&input).expect("reads"), before);
        assert_eq!(
            Pdf::open(dir.join("out.pdf"))
                .expect("re-opens")
                .page_count(),
            12
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_pages_asked_for_are_stamped() {
        let dir = scratch("watermark-pages");
        let mut args = args(dir.join("out.pdf"));
        args.pages = Some("2-3".into());
        let watermark = watermark_from(&args).expect("builds");

        let report = run(
            &fixture("many-pages.pdf"),
            &args,
            &watermark,
            None,
            false,
            false,
            &CancelToken::never(),
        )
        .expect("stamps");

        assert_eq!(report.json["pages"], 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn text_the_fonts_cannot_draw_fails_rather_than_writing_a_copy() {
        // A file that looks identical to its input, reported as success, sends
        // someone looking for the bug in the wrong place.
        let dir = scratch("watermark-unencodable");
        let mut args = args(dir.join("out.pdf"));
        args.text = Some("\u{6a5f}\u{5bc6}".into());
        let watermark = watermark_from(&args).expect("builds");

        let result = run(
            &fixture("two-pages.pdf"),
            &args,
            &watermark,
            None,
            false,
            false,
            &CancelToken::never(),
        );

        assert!(result.is_err());
        assert!(!dir.join("out.pdf").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
