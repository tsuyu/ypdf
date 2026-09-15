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
fn a_pdfa_check_fails_a_file_that_is_not_pdfa_and_says_what_it_missed() {
    let output = run(&[
        "pdfa",
        &fixture("two-pages.pdf").display().to_string(),
        "--json",
    ]);
    let json = json_of(&output);
    let result = &json["files"][0]["result"];

    // A failed check is a finding, not an error: the file was read fine.
    assert_eq!(
        output.status.code(),
        Some(1),
        "a failing check gates a script"
    );
    assert_eq!(result["passed"], false);
    assert_eq!(result["claimed"], serde_json::Value::Null);
    assert_eq!(result["checked"], "PDF/A-2b");

    let codes: Vec<String> = result["violations"]
        .as_array()
        .expect("violations")
        .iter()
        .map(|v| v["code"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(codes.contains(&"PDFA_NO_XMP".to_string()), "{codes:?}");

    // Every report carries what it did not check, in JSON as well as in text.
    assert!(
        !result["not_checked"].as_array().expect("limits").is_empty(),
        "a verdict without its limits is the one thing this must not print"
    );
}

#[test]
fn a_pdfa_level_that_does_not_exist_is_refused_before_any_file_is_read() {
    let output = run(&[
        "pdfa",
        &fixture("two-pages.pdf").display().to_string(),
        "--level",
        "1u",
        "--json",
    ]);
    // PDF/A-1u has never existed. Exit 8 is E_CONFIG: the command was wrong,
    // not the document.
    assert_eq!(output.status.code(), Some(8));
    let json = json_of(&output);
    assert_eq!(json["error"]["code"], "E_CONFIG");
}

#[test]
fn a_signed_document_verifies_and_still_refuses_to_call_itself_trusted() {
    let output = run(&[
        "signatures",
        &fixture("signed.pdf").display().to_string(),
        "--json",
    ]);
    let json = json_of(&output);
    let result = &json["files"][0]["result"];

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(result["signed"], true);
    assert_eq!(result["all_intact"], true);
    assert_eq!(result["signatures"][0]["verdict"], "intact");
    assert_eq!(result["signatures"][0]["covers_whole_file"], true);
    assert_eq!(result["signatures"][0]["signer"], "Ada Lovelace");

    // The one thing the command must never do is imply trust.
    assert!(
        !result["not_established"]
            .as_array()
            .expect("limits")
            .is_empty()
    );
}

#[test]
fn a_signed_document_with_a_byte_changed_fails_the_run() {
    let dir = scratch("tampered-signature");
    let path = dir.join("tampered.pdf");
    let mut bytes = std::fs::read(fixture("signed.pdf")).expect("the fixture");

    // Inside the signed range, and visible: the stated reason for signing.
    let at = bytes
        .windows(9)
        .position(|window| window == b"(I agree)")
        .expect("the reason");
    bytes[at + 1] = b'X';
    std::fs::write(&path, &bytes).expect("writes");

    let output = run(&["signatures", &path.display().to_string(), "--json"]);
    let json = json_of(&output);
    let result = &json["files"][0]["result"];

    assert_eq!(
        output.status.code(),
        Some(1),
        "a broken signature gates a script"
    );
    assert_eq!(result["all_intact"], false);
    assert_eq!(result["signatures"][0]["verdict"], "document_altered");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unsigned_file_passes_unless_signatures_were_required() {
    let path = fixture("two-pages.pdf").display().to_string();

    let relaxed = run(&["signatures", &path, "--json"]);
    assert_eq!(relaxed.status.code(), Some(0), "unsigned is not a failure");

    let strict = run(&["signatures", &path, "--require-signed", "--json"]);
    assert_eq!(strict.status.code(), Some(1));
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
        "pdfa",
        "signatures",
        "annotate",
        "forms",
    ] {
        assert!(text.contains(command), "--help omits {command}:\n{text}");
    }
}

#[test]
fn markdown_rebuilds_headings_paragraphs_and_lists() {
    // The fixture is set at three sizes with a bold subheading and two
    // bulleted lines, so every inference this command makes is exercised.
    let output = run(&["markdown", &fixture("structured.pdf").display().to_string()]);
    let text = stdout(&output);

    assert!(
        text.contains("# Quarterly Report"),
        "the largest line should be the top heading:\n{text}"
    );
    assert!(
        text.contains("## Findings"),
        "the middle size should be the second level:\n{text}"
    );
    assert!(
        text.contains("sets out what changed, why it changed"),
        "two lines of one paragraph should be joined:\n{text}"
    );
    assert!(
        text.contains("- Revenue rose by four per cent.\n- Costs were flat."),
        "bulleted lines should be a tight list:\n{text}"
    );
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn markdown_writes_a_file_when_told_where() {
    let dir = scratch("markdown-out");
    let target = dir.join("report.md");
    let output = run(&[
        "markdown",
        &fixture("structured.pdf").display().to_string(),
        "-o",
        &target.display().to_string(),
    ]);

    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let written = std::fs::read_to_string(&target).expect("the markdown file");
    assert!(written.starts_with("# Quarterly Report"), "{written}");
}

#[test]
fn markdown_reports_a_page_with_no_text_rather_than_going_quiet() {
    // Spec §5: conversion quality is reported, not promised. A scan has no
    // text layer, and silence would read as "this page was empty".
    let output = run(&[
        "markdown",
        &fixture("no-fonts.pdf").display().to_string(),
        "--json",
    ]);
    let json = json_of(&output);

    let warnings = json["files"][0]["result"]["warnings"]
        .as_array()
        .expect("warnings are a list");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("no text layer")),
        "expected a no-text-layer warning, got {warnings:?}"
    );
}

#[test]
fn docx_writes_a_word_package_with_the_structure_it_found() {
    let dir = scratch("docx-out");
    let target = dir.join("report.docx");
    let output = run(&[
        "docx",
        &fixture("structured.pdf").display().to_string(),
        "-o",
        &target.display().to_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));

    let bytes = std::fs::read(&target).expect("the docx file");
    assert_eq!(&bytes[..2], b"PK", "a docx is a ZIP");

    // The entries are stored rather than deflated, so the parts are findable
    // in the bytes as they are. That is what makes this checkable without a
    // ZIP reader in the test.
    let haystack = String::from_utf8_lossy(&bytes);
    for needle in [
        "word/document.xml",
        "word/styles.xml",
        "word/numbering.xml",
        r#"<w:pStyle w:val="Heading1"/>"#,
        r#"<w:pStyle w:val="Heading2"/>"#,
        "Quarterly Report",
        "Revenue rose by four per cent.",
    ] {
        assert!(
            haystack.contains(needle),
            "{needle} is missing from the docx"
        );
    }
}

#[test]
fn docx_refuses_to_clobber_without_overwrite() {
    let dir = scratch("docx-existing");
    let target = dir.join("taken.docx");
    std::fs::write(&target, b"not a docx").expect("the file in the way");

    let output = run(&[
        "docx",
        &fixture("structured.pdf").display().to_string(),
        "-o",
        &target.display().to_string(),
    ]);

    assert_eq!(output.status.code(), Some(11), "E_OUTPUT_EXISTS");
    assert_eq!(
        std::fs::read(&target).expect("still there"),
        b"not a docx",
        "the file in the way must be untouched"
    );
}

#[test]
fn pages_rotates_only_the_pages_named() {
    let dir = scratch("pages-rotate");
    let target = dir.join("turned.pdf");
    let output = run(&[
        "pages",
        &fixture("two-pages.pdf").display().to_string(),
        "--rotate",
        "90",
        "--pages",
        "1",
        "-o",
        &target.display().to_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));

    // Written small and uncompressed by the writer, so the page dictionaries
    // are readable in the bytes: one page turned, the other left alone.
    let bytes = std::fs::read(&target).expect("the rotated file");
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(
        text.matches("/Rotate 90").count(),
        1,
        "exactly one page should carry a rotation"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pages_reports_what_it_did_in_json() {
    let dir = scratch("pages-json");
    let target = dir.join("shorter.pdf");
    let output = run(&[
        "pages",
        &fixture("many-pages.pdf").display().to_string(),
        "--delete",
        "--pages",
        "2-4",
        "-o",
        &target.display().to_string(),
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));

    let json = json_of(&output);
    let result = &json["files"][0]["result"];
    assert_eq!(result["action"], "delete");
    assert_eq!(result["pages_before"], 12);
    assert_eq!(result["pages_after"], 9);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pages_without_an_operation_says_what_is_on_offer() {
    let dir = scratch("pages-no-op");
    let target = dir.join("nothing.pdf");
    let output = run(&[
        "pages",
        &fixture("two-pages.pdf").display().to_string(),
        "-o",
        &target.display().to_string(),
    ]);

    assert_ne!(output.status.code(), Some(0), "nothing was asked for");
    let said = format!(
        "{}{}",
        stdout(&output),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(said.contains("--rotate"), "{said}");
    assert!(!target.exists(), "nothing should have been written");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pages_refuses_two_operations_at_once() {
    let dir = scratch("pages-two-ops");
    let target = dir.join("both.pdf");
    let output = run(&[
        "pages",
        &fixture("two-pages.pdf").display().to_string(),
        "--reverse",
        "--delete",
        "--pages",
        "1",
        "-o",
        &target.display().to_string(),
    ]);

    assert_ne!(output.status.code(), Some(0), "one operation at a time");
    assert!(!target.exists(), "nothing should have been written");
    let _ = std::fs::remove_dir_all(&dir);
}
