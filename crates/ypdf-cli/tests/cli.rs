//! The command line as a contract (spec §22).
//!
//! These run the built binary rather than calling the functions, because the
//! things being checked — exit codes, what lands on stdout versus stderr — only
//! exist at that boundary. A script depends on them, so they are tested the way
//! a script sees them.

// Integration tests compile as their own crate, so `cfg(test)` is not set and
// the clippy.toml allowance for tests does not reach here.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_ypdf-cli")
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("ypdf-cli-e2e").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn run(args: &[&str]) -> Output {
    Command::new(binary())
        .args(args)
        .output()
        .expect("the binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn json_of(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("stdout was not valid JSON ({e}):\n{}", stdout(output)))
}

#[test]
fn json_mode_puts_nothing_but_json_on_stdout() {
    // The whole point of --json is `... | jq`. A stray log line breaks it.
    let output = run(&[
        "info",
        &fixture("many-pages.pdf").display().to_string(),
        "--json",
    ]);
    let json = json_of(&output);

    assert_eq!(json["command"], "info");
    assert_eq!(json["files"][0]["result"]["pages"], 12);
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn a_missing_file_exits_two_and_says_which_code_it_was() {
    let output = run(&["info", "definitely-not-here.pdf", "--json"]);
    let json = json_of(&output);

    assert_eq!(output.status.code(), Some(2), "E_IO is exit 2");
    assert_eq!(json["files"][0]["ok"], false);
    assert_eq!(json["files"][0]["error"]["code"], "E_IO");
}

#[test]
fn quiet_success_says_nothing_at_all() {
    let output = run(&[
        "info",
        &fixture("two-pages.pdf").display().to_string(),
        "--quiet",
    ]);
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).is_empty(), "quiet must mean quiet");
}

#[test]
fn one_bad_file_does_not_stop_the_rest_of_a_batch() {
    let dir = scratch("batch");
    std::fs::copy(fixture("two-pages.pdf"), dir.join("good-1.pdf")).expect("copies");
    std::fs::copy(fixture("many-pages.pdf"), dir.join("good-2.pdf")).expect("copies");
    std::fs::write(dir.join("broken.pdf"), b"not a pdf at all").expect("writes");

    let pattern = format!("{}/*.pdf", dir.display());
    let output = run(&["info", &pattern, "--json"]);
    let json = json_of(&output);

    assert_eq!(json["summary"]["total"], 3);
    assert_eq!(json["summary"]["processed"], 2);
    assert_eq!(json["summary"]["failed"], 1);
    assert_eq!(
        output.status.code(),
        Some(3),
        "the failure's own code, so a script can still branch on it"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_existing_output_is_never_replaced_by_accident() {
    let dir = scratch("guard");
    let target = dir.join("out.pdf");
    std::fs::write(&target, b"precious").expect("writes");

    let output = run(&[
        "extract",
        &fixture("many-pages.pdf").display().to_string(),
        "--pages",
        "1-2",
        "-o",
        &target.display().to_string(),
    ]);

    assert_eq!(output.status.code(), Some(11), "E_OUTPUT_EXISTS");
    assert_eq!(
        std::fs::read(&target).expect("still there"),
        b"precious",
        "the existing file must be untouched"
    );

    // And with permission, it goes through.
    let output = run(&[
        "extract",
        &fixture("many-pages.pdf").display().to_string(),
        "--pages",
        "1-2",
        "-o",
        &target.display().to_string(),
        "--overwrite",
    ]);
    assert_eq!(output.status.code(), Some(0));
    assert_ne!(std::fs::read(&target).expect("written"), b"precious");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fail_on_gates_a_pipeline_without_calling_the_scan_a_failure() {
    let output = run(&[
        "security-scan",
        &fixture("hostile.pdf").display().to_string(),
        "--fail-on",
        "warning",
        "--json",
    ]);
    let json = json_of(&output);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json["files"][0]["ok"], true, "the file was read fine");
    assert_eq!(json["files"][0]["result"]["flagged"], true);
    assert_eq!(json["summary"]["failed"], 0);
}

#[test]
fn a_clean_file_passes_the_same_gate() {
    let output = run(&[
        "security-scan",
        &fixture("many-pages.pdf").display().to_string(),
        "--fail-on",
        "warning",
    ]);
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn the_scanner_never_turns_a_url_into_something_clickable() {
    // Detection must not become an invitation to click: the links are printed
    // as plain text so someone can read where the file wants to send them.
    let output = run(&[
        "security-scan",
        &fixture("hostile.pdf").display().to_string(),
    ]);
    let text = stdout(&output);
    assert!(text.contains("http"), "the URLs must be shown: {text}");
    assert!(
        !text.contains("\u{1b}]8;;"),
        "no terminal hyperlink escapes"
    );
}

#[test]
fn merging_reports_the_pages_it_produced() {
    let dir = scratch("merge");
    let target = dir.join("merged.pdf");

    let output = run(&[
        "merge",
        &fixture("two-pages.pdf").display().to_string(),
        &fixture("many-pages.pdf").display().to_string(),
        "-o",
        &target.display().to_string(),
        "--json",
    ]);
    let json = json_of(&output);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(json["result"]["pages"], 14);
    assert!(target.is_file());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn splitting_names_every_piece_it_wrote() {
    let dir = scratch("split");
    let output = run(&[
        "split",
        &fixture("many-pages.pdf").display().to_string(),
        "-o",
        &dir.display().to_string(),
        "--every",
        "5",
        "--json",
    ]);
    let json = json_of(&output);

    let files = json["result"]["files"].as_array().expect("an array");
    assert_eq!(files.len(), 3, "12 pages in fives is 5 + 5 + 2");
    for file in files {
        assert!(Path::new(file["path"].as_str().expect("a path")).is_file());
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unusable_preset_fails_before_a_single_file_is_touched() {
    let dir = scratch("preset");
    let output = run(&[
        "compress",
        &fixture("many-pages.pdf").display().to_string(),
        "-o",
        &dir.join("out.pdf").display().to_string(),
        "--preset",
        "Nonexistent",
    ]);

    assert_eq!(output.status.code(), Some(8), "E_CONFIG");
    assert!(
        std::fs::read_dir(&dir).expect("reads").next().is_none(),
        "nothing may have been written"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reading_metadata_never_writes_to_the_file() {
    let dir = scratch("metadata");
    let copy = dir.join("doc.pdf");
    std::fs::copy(fixture("metadata.pdf"), &copy).expect("copies");
    let before = std::fs::read(&copy).expect("reads");

    let output = run(&["metadata", &copy.display().to_string(), "--json"]);
    let json = json_of(&output);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(json["files"][0]["result"]["title"], "Quarterly Report");
    assert_eq!(std::fs::read(&copy).expect("reads"), before);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn editing_metadata_in_place_needs_permission_and_then_works() {
    let dir = scratch("metadata-edit");
    let copy = dir.join("doc.pdf");
    std::fs::copy(fixture("metadata.pdf"), &copy).expect("copies");
    let before = std::fs::read(&copy).expect("reads");

    let refused = run(&[
        "metadata",
        &copy.display().to_string(),
        "--set-title",
        "Renamed",
    ]);
    assert_ne!(refused.status.code(), Some(0));
    assert_eq!(std::fs::read(&copy).expect("reads"), before);

    let allowed = run(&[
        "metadata",
        &copy.display().to_string(),
        "--set-title",
        "Renamed",
        "--overwrite",
        "--json",
    ]);
    assert_eq!(allowed.status.code(), Some(0));

    let read_back = run(&["metadata", &copy.display().to_string(), "--json"]);
    assert_eq!(
        json_of(&read_back)["files"][0]["result"]["title"],
        "Renamed"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_protected_document_needs_its_password_and_says_which_problem_it_is() {
    let dir = scratch("protect");
    let protected = dir.join("protected.pdf");

    let output = run(&[
        "encrypt",
        &fixture("many-pages.pdf").display().to_string(),
        "-o",
        &protected.display().to_string(),
        "--user-password",
        "open-me",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        json_of(&output)["files"][0]["result"]["algorithm"],
        "AES-256"
    );

    // Without the password there is no document, and the error says so rather
    // than blaming the file for being corrupt.
    let refused = run(&["info", &protected.display().to_string(), "--json"]);
    assert_eq!(refused.status.code(), Some(4));
    assert_eq!(
        json_of(&refused)["files"][0]["error"]["code"],
        "E_PASSWORD_REQUIRED"
    );

    // A wrong one is a different answer again.
    let wrong = run(&[
        "info",
        &protected.display().to_string(),
        "--password",
        "not-it",
        "--json",
    ]);
    assert_eq!(wrong.status.code(), Some(4));
    assert_eq!(
        json_of(&wrong)["files"][0]["error"]["code"],
        "E_PASSWORD_WRONG"
    );

    // With it, the document reads and reports what protects it.
    let opened = run(&[
        "info",
        &protected.display().to_string(),
        "--password",
        "open-me",
        "--json",
    ]);
    assert_eq!(opened.status.code(), Some(0));
    let result = &json_of(&opened)["files"][0]["result"];
    assert_eq!(result["pages"], 12);
    assert_eq!(result["encryption"], "AES-256");
    assert_eq!(result["open_password_required"], true);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restrictions_are_written_and_read_back_in_words() {
    let dir = scratch("restrictions");
    let protected = dir.join("protected.pdf");

    run(&[
        "encrypt",
        &fixture("many-pages.pdf").display().to_string(),
        "-o",
        &protected.display().to_string(),
        "--user-password",
        "open-me",
        "--algorithm",
        "aes128",
        "--no-copy",
        "--no-modify",
    ]);

    let output = run(&[
        "diagnostics",
        &protected.display().to_string(),
        "--password",
        "open-me",
        "--json",
    ]);
    let result = &json_of(&output)["files"][0]["result"];

    assert_eq!(result["encryption"], "AES-128");
    let restrictions = result["restrictions"].as_array().expect("an array");
    let words: Vec<&str> = restrictions.iter().filter_map(|v| v.as_str()).collect();
    assert!(words.contains(&"copying text"), "{words:?}");
    assert!(words.contains(&"editing"), "{words:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn decrypting_needs_the_password_and_then_gives_a_plain_document() {
    let dir = scratch("decrypt");
    let protected = dir.join("protected.pdf");
    let plain = dir.join("plain.pdf");

    run(&[
        "encrypt",
        &fixture("two-pages.pdf").display().to_string(),
        "-o",
        &protected.display().to_string(),
        "--user-password",
        "open-me",
    ]);

    // No password, no document. There is no flag that gets round this.
    let refused = run(&[
        "decrypt",
        &protected.display().to_string(),
        "-o",
        &plain.display().to_string(),
    ]);
    assert_eq!(refused.status.code(), Some(4));
    assert!(!plain.exists(), "nothing may be written");

    let allowed = run(&[
        "decrypt",
        &protected.display().to_string(),
        "-o",
        &plain.display().to_string(),
        "--password",
        "open-me",
    ]);
    assert_eq!(allowed.status.code(), Some(0));

    let output = run(&["info", &plain.display().to_string(), "--json"]);
    let result = &json_of(&output)["files"][0]["result"];
    assert_eq!(result["pages"], 2);
    assert_eq!(result["encrypted"], false);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_password_can_come_from_the_environment_instead_of_the_command_line() {
    // A password in an argument is visible to anything that can list processes.
    let dir = scratch("password-env");
    let protected = dir.join("protected.pdf");

    run(&[
        "encrypt",
        &fixture("two-pages.pdf").display().to_string(),
        "-o",
        &protected.display().to_string(),
        "--user-password",
        "open-me",
    ]);

    let output = Command::new(binary())
        .args(["info", &protected.display().to_string(), "--json"])
        .env("YPDF_PASSWORD", "open-me")
        .output()
        .expect("the binary runs");

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(json_of(&output)["files"][0]["result"]["pages"], 2);

    let _ = std::fs::remove_dir_all(&dir);
}

/// A stand-in for Tesseract: prints canned TSV for any image it is given.
///
/// The contract is small — take an image path, print TSV — so standing in for
/// it exercises everything around recognition without needing the real engine
/// installed. Recognition quality is Tesseract's business and no fake can say
/// anything about it.
fn tesseract_stub(dir: &Path) -> PathBuf {
    // Built as separate lines rather than one continued literal: a stray
    // indent inside the TSV shifts every column and the parse silently yields
    // nothing.
    let rows = [
        "level	page_num	block_num	par_num	line_num	word_num	left	top	width	height	conf	text",
        "5	1	1	1	1	1	100	120	180	40	96.5	Quarterly",
        "5	1	1	1	1	2	300	120	120	40	95.1	Report",
    ]
    .join(
        "
",
    );

    if cfg!(windows) {
        let path = dir.join("tesseract.bat");
        let mut script = String::from("@echo off\r\n");
        script.push_str("if \"%1\"==\"--version\" (echo tesseract 5.3.3-stub & exit /b 0)\r\n");
        script.push_str(
            "if \"%1\"==\"--list-langs\" (echo List of available languages ^(1^): & echo eng & exit /b 0)\r\n",
        );
        for line in rows.lines() {
            script.push_str(&format!("echo {line}\r\n"));
        }
        script.push_str("exit /b 0\r\n");
        std::fs::write(&path, script).expect("writes the stub");
        path
    } else {
        let path = dir.join("tesseract.sh");
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n--version) echo 'tesseract 5.3.3-stub'; exit 0;;\n             --list-langs) printf 'List of available languages (1):\\neng\\n'; exit 0;;\nesac\n             printf '%s' '{rows}'\n"
        );
        std::fs::write(&path, script).expect("writes the stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("makes it executable");
        }
        path
    }
}

#[test]
fn ocr_adds_a_searchable_layer_without_changing_the_page() {
    let dir = scratch("ocr");
    let stub = tesseract_stub(&dir);
    let target = dir.join("searchable.pdf");

    let output = run(&[
        "ocr",
        &fixture("two-pages.pdf").display().to_string(),
        "-o",
        &target.display().to_string(),
        "--tesseract",
        &stub.display().to_string(),
        // The fixture has real text; --force is what makes this about the
        // layer rather than about the skip.
        "--force",
        "--dpi",
        "150",
        "--json",
    ]);

    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let result = &json_of(&output)["files"][0]["result"];
    assert_eq!(result["pages_recognized"], 2);
    assert!(result["words"].as_u64().unwrap_or(0) >= 4, "{result}");

    // The recognized words have to come back out, or the feature did nothing.
    let text = lopdf::Document::load(&target)
        .expect("re-opens")
        .extract_text(&[1])
        .expect("extracts");
    assert!(text.contains("Quarterly"), "{text:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ocr_leaves_pages_that_already_have_text_alone_by_default() {
    // Laying a guess over real text gives search two answers for the same
    // words, and the guess is the worse one.
    let dir = scratch("ocr-skip");
    let stub = tesseract_stub(&dir);
    let target = dir.join("out.pdf");

    let output = run(&[
        "ocr",
        &fixture("many-pages.pdf").display().to_string(),
        "-o",
        &target.display().to_string(),
        "--tesseract",
        &stub.display().to_string(),
        "--json",
    ]);

    let result = &json_of(&output)["files"][0]["result"];
    assert_eq!(result["pages_recognized"], 0);
    assert_eq!(result["pages_skipped_with_text"], 12);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_language_pack_that_is_not_installed_fails_before_any_work() {
    let dir = scratch("ocr-lang");
    let stub = tesseract_stub(&dir);
    let target = dir.join("out.pdf");

    let output = run(&[
        "ocr",
        &fixture("two-pages.pdf").display().to_string(),
        "-o",
        &target.display().to_string(),
        "--tesseract",
        &stub.display().to_string(),
        "--lang",
        "jpn",
    ]);

    assert_ne!(output.status.code(), Some(0));
    assert!(!target.exists(), "nothing may have been written");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn search_and_redact_takes_the_words_out_of_the_file() {
    // The whole promise, end to end and through real PDFium: the tool is told
    // a word, finds it on the page, and the saved file no longer contains it
    // anywhere — not in a stream, not compressed, nowhere.
    let dir = scratch("redact");
    let target = dir.join("redacted.pdf");

    let output = run(&[
        "redact",
        &fixture("many-pages.pdf").display().to_string(),
        "-o",
        &target.display().to_string(),
        "--find",
        "Page 3",
        "--json",
    ]);

    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let result = &json_of(&output)["files"][0]["result"];
    assert!(
        result["glyphs_removed"].as_u64().unwrap_or(0) > 0,
        "{result}"
    );

    let document = lopdf::Document::load(&target).expect("re-opens");
    let text = document.extract_text(&[3]).expect("extracts");
    assert!(!text.contains("Page 3"), "still readable: {text:?}");

    // And page 4 is untouched, so the redaction went where it was aimed.
    let other = document.extract_text(&[4]).expect("extracts");
    assert!(other.contains('4'), "{other:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_search_that_matches_nothing_fails_loudly_and_writes_nothing() {
    // Being told "done" when the pattern was wrong is the most dangerous
    // answer this command can give: the file gets sent.
    let dir = scratch("redact-no-match");
    let target = dir.join("out.pdf");

    let output = run(&[
        "redact",
        &fixture("many-pages.pdf").display().to_string(),
        "-o",
        &target.display().to_string(),
        "--find",
        "no-such-text-anywhere",
    ]);

    assert_ne!(output.status.code(), Some(0));
    assert!(!target.exists(), "nothing may have been written");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_explicit_area_needs_no_search_at_all() {
    let dir = scratch("redact-rect");
    let target = dir.join("out.pdf");

    let output = run(&[
        "redact",
        &fixture("two-pages.pdf").display().to_string(),
        "-o",
        &target.display().to_string(),
        "--rect",
        "1:0,0,612,792",
        "--json",
    ]);

    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let text = lopdf::Document::load(&target)
        .expect("re-opens")
        .extract_text(&[1])
        .expect("extracts");
    assert!(
        text.trim().is_empty(),
        "the whole page should be gone: {text:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn redacting_nothing_at_all_is_refused() {
    let dir = scratch("redact-nothing");
    let output = run(&[
        "redact",
        &fixture("two-pages.pdf").display().to_string(),
        "-o",
        &dir.join("out.pdf").display().to_string(),
    ]);
    assert_ne!(output.status.code(), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn help_lists_every_command_the_spec_asks_for() {
    let output = run(&["--help"]);
    let text = stdout(&output);
    for command in [
        "info",
        "merge",
        "split",
        "compress",
        "images",
        "metadata",
        "security-scan",
        "diagnostics",
        "encrypt",
        "decrypt",
        "ocr",
        "redact",
    ] {
        assert!(text.contains(command), "--help omits {command}:\n{text}");
    }
}
