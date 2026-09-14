//! Blocks into Markdown.
//!
//! Everything interesting has already been decided by the time text reaches
//! here; this only writes it down. The one judgement it does make is escaping:
//! body text that happens to begin with `-` or `1.` would otherwise come back
//! as a list when the Markdown is read again.

use crate::blocks::Block;
use crate::lines::Run;

/// Write blocks out as Markdown.
#[must_use]
pub fn render(blocks: &[Block]) -> String {
    let mut out = String::new();
    let mut ordinal = 0_u32;
    let mut previous_was_item = false;

    for block in blocks {
        // Items of one list are kept together. A blank line between them is
        // still a list, but a loose one, which readers set with far more air
        // than the page it came from had.
        let tight = previous_was_item && matches!(block, Block::ListItem { .. });
        previous_was_item = matches!(block, Block::ListItem { .. });

        match block {
            Block::Heading { level, runs } => {
                ordinal = 0;
                let hashes = "#".repeat(usize::from(*level).clamp(1, 6));
                let text = plain(runs);
                push(&mut out, &format!("{hashes} {}", text.trim()), tight);
            }
            Block::Paragraph { runs } => {
                ordinal = 0;
                let text = emphasised(runs);
                push(&mut out, escape_leading(text.trim()).trim(), tight);
            }
            Block::ListItem { runs, ordered } => {
                let text = emphasised(runs);
                let marker = if *ordered {
                    ordinal += 1;
                    format!("{ordinal}.")
                } else {
                    ordinal = 0;
                    "-".to_string()
                };
                push(&mut out, &format!("{marker} {}", text.trim()), tight);
            }
        }
    }

    out
}

/// Add a block, parted by a blank line unless it belongs with the one above.
fn push(out: &mut String, text: &str, tight: bool) {
    if text.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push_str(if tight { "\n" } else { "\n\n" });
    }
    out.push_str(text);
}

/// The runs' text with nothing added.
fn plain(runs: &[Run]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}

/// The runs' text with bold and italic marked.
fn emphasised(runs: &[Run]) -> String {
    let mut out = String::new();
    for run in runs {
        let marker = match (run.bold, run.italic) {
            (true, true) => "***",
            (true, false) => "**",
            (false, true) => "*",
            (false, false) => "",
        };

        if marker.is_empty() || run.text.trim().is_empty() {
            out.push_str(&run.text);
            continue;
        }

        // Markers have to sit against the words: `** bold **` is not emphasis
        // in any Markdown reader, it is two literal asterisks.
        let leading: String = run.text.chars().take_while(|c| c.is_whitespace()).collect();
        let trailing: String = run
            .text
            .chars()
            .rev()
            .take_while(|c| c.is_whitespace())
            .collect();
        let core = run.text.trim();

        out.push_str(&leading);
        out.push_str(marker);
        out.push_str(core);
        out.push_str(marker);
        out.push_str(&trailing);
    }
    out
}

/// Escape a leading character that would turn body text into something else.
fn escape_leading(text: &str) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return text.to_string();
    };

    if matches!(first, '#' | '-' | '+' | '>' | '*' | '=' | '|') {
        return format!("\\{text}");
    }

    // `1.` at the start of a paragraph reads as an ordered list.
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty()
        && text[digits.len()..].starts_with(['.', ')'])
        && text[digits.len() + 1..].starts_with(char::is_whitespace)
    {
        return format!("{digits}\\{}", &text[digits.len()..]);
    }

    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> Run {
        Run {
            text: text.to_string(),
            bold: false,
            italic: false,
        }
    }

    #[test]
    fn headings_carry_one_hash_per_level() {
        let out = render(&[
            Block::Heading {
                level: 1,
                runs: vec![run("Title")],
            },
            Block::Heading {
                level: 3,
                runs: vec![run("Deeper")],
            },
        ]);
        assert_eq!(out, "# Title\n\n### Deeper");
    }

    #[test]
    fn blocks_are_separated_by_a_blank_line() {
        let out = render(&[
            Block::Paragraph {
                runs: vec![run("One.")],
            },
            Block::Paragraph {
                runs: vec![run("Two.")],
            },
        ]);
        assert_eq!(out, "One.\n\nTwo.");
    }

    #[test]
    fn numbered_items_count_up_and_reset_after_other_blocks() {
        let out = render(&[
            Block::ListItem {
                runs: vec![run("first")],
                ordered: true,
            },
            Block::ListItem {
                runs: vec![run("second")],
                ordered: true,
            },
            Block::Paragraph {
                runs: vec![run("Interrupting.")],
            },
            Block::ListItem {
                runs: vec![run("first again")],
                ordered: true,
            },
        ]);
        assert_eq!(
            out,
            "1. first\n2. second\n\nInterrupting.\n\n1. first again"
        );
    }

    #[test]
    fn a_list_stays_tight_but_is_still_parted_from_what_follows() {
        let out = render(&[
            Block::ListItem {
                runs: vec![run("one")],
                ordered: false,
            },
            Block::ListItem {
                runs: vec![run("two")],
                ordered: false,
            },
            Block::Paragraph {
                runs: vec![run("After.")],
            },
        ]);
        assert_eq!(out, "- one\n- two\n\nAfter.");
    }

    #[test]
    fn emphasis_markers_sit_against_the_words_not_the_spaces() {
        let out = render(&[Block::Paragraph {
            runs: vec![
                run("a "),
                Run {
                    text: "loud ".to_string(),
                    bold: true,
                    italic: false,
                },
                run("word"),
            ],
        }]);
        assert_eq!(out, "a **loud** word");
    }

    #[test]
    fn body_text_that_looks_like_a_list_is_escaped() {
        let out = render(&[Block::Paragraph {
            runs: vec![run("- not a list")],
        }]);
        assert_eq!(out, "\\- not a list");

        let numbered = render(&[Block::Paragraph {
            runs: vec![run("1. also not a list")],
        }]);
        assert_eq!(numbered, "1\\. also not a list");
    }

    #[test]
    fn a_heading_is_not_further_emphasised() {
        let out = render(&[Block::Heading {
            level: 2,
            runs: vec![Run {
                text: "Bold Heading".to_string(),
                bold: true,
                italic: false,
            }],
        }]);
        assert_eq!(out, "## Bold Heading", "a heading is already strong");
    }
}
