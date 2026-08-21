# yPDF — Development Plan (Rust Native GUI)

Implementation plan derived from [pdf-tool-spec.md](pdf-tool-spec.md).

Target: native Rust desktop GUI (egui/eframe), shared engine crates, CLI on the same
engine. Local-first, no network dependency for any core operation.

---

## 1. Technology Decisions

| Layer | Choice | Rationale |
|---|---|---|
| GUI | **egui + eframe** | Immediate-mode. Page render = RGBA texture upload, trivial. Command palette, tool panels, drag-drop lists all cheap. Fast iteration. iced/Slint cost more ceremony with no payoff for a dev-tool UI. |
| Render + text | **pdfium-render** (PDFium, BSD-3) | Only realistic path to correct rendering, text extraction with char bounding boxes, form widgets, annotation appearance streams. Pure-Rust renderers are not production-grade. |
| Structure ops | **lopdf** | Merge, split, extract, delete, reorder, rotate, metadata, bookmarks, links, attachments. Pure Rust, safe to parallelize. |
| Heavy surgery | **qpdf** via `qpdf-rs` (Apache-2.0) | Linearization, AES-128/256 encryption, permission bits, xref repair, structural JSON dump. Saves months of work. |
| Generation | **printpdf** + `image` | Images to PDF, TXT/Markdown to PDF, watermark stamp layers. |
| OCR | **leptess** (Tesseract + Leptonica) | Spec requires msa/ara/zho/jpn/kor. Pure-Rust `ocrs` cannot cover CJK or Arabic. |
| Compression | `image` + `mozjpeg` + `oxipng` + qpdf | Downsample, requantize, then object-stream compression + linearize. |
| Parallelism | **rayon** (CPU) + `crossbeam` channels | No tokio in the desktop build. Tokio enters only with `ypdf-api`. |
| Storage | **rusqlite** | Recent files, job history, presets, cache index. |
| Logging | **tracing** + `tracing-subscriber` | Structured, JSON format, operation/job IDs (spec §31). |
| CLI | **clap** (derive) | Spec §22. |

### Rejected

- **MuPDF / `mupdf-rs`** — AGPL. Forecloses commercial and closed-source distribution.
- **Tauri** — spec lists it as an option, but it reintroduces a JS/Node toolchain and a
  webview IPC hop on every page blit. Native egui keeps one language and one process.
- **Pure-Rust rendering stack** — not viable at the required fidelity today.

---

## 2. Architecture

### 2.1 Threading model — hard constraint

PDFium keeps global state and is **not** thread-safe. Page rendering must not be
rayon-fanned-out.

```text
  +------------------------------+
  |  GUI thread (egui/eframe)    |  never blocks
  +---+----------------------+---+
      | RenderCmd            | Job
      v                      v
  +------------------+   +----------------------+
  | Render thread    |   | Rayon worker pool    |
  | (owns Pdfium)    |   | lopdf / qpdf /       |
  | serial queue     |   | image / tesseract    |
  +---+--------------+   +---+------------------+
      | RenderedPage         | Progress / Result
      +----------+-----------+
                 v
        ctx.request_repaint()
```

- Exactly one dedicated render thread owning the `Pdfium` instance, fed by a priority
  queue (visible page > adjacent pages > thumbnails) with a cancel token per request.
- Everything else (structure ops, compression, OCR, scanning) goes to rayon.
- Progress and results return over `crossbeam` channels; the GUI calls
  `ctx.request_repaint()` on receipt.

Bake this in at M0. Retrofitting it later means rewriting every call site.

### 2.2 Layering rule

Every feature lands in an engine crate first. GUI and CLI are both thin callers.
No PDF logic in `apps/desktop`. This is what makes spec §21 (batch) and §22 (CLI)
nearly free, and makes the engine testable without a window.

### 2.3 Repository structure

Trimmed from spec §40 — 8 crates instead of 12. Crates for signing, forms,
conversion, and the API get created when their milestone starts, not stubbed now.

```text
yPDF/
├── Cargo.toml                 # workspace
├── PLAN.md
├── pdf-tool-spec.md
├── crates/
│   ├── ypdf-core/             # types, Error+codes, Config, presets, Progress, CancelToken
│   ├── ypdf-doc/              # lopdf+qpdf: pages, merge/split, metadata, bookmarks, links, attachments
│   ├── ypdf-render/           # pdfium wrapper, render thread, texture cache, text extract/search
│   ├── ypdf-optimize/         # compression, downsampling, linearization, image extraction
│   ├── ypdf-ocr/              # tesseract, invisible-text-layer searchable PDF
│   ├── ypdf-security/         # security scanner, encryption, permissions
│   ├── ypdf-jobs/             # queue, worker pool, progress, history (sqlite)
│   └── ypdf-cli/              # clap, JSON output, exit codes
├── apps/
│   └── desktop/               # egui application
├── tests/
│   ├── fixtures/              # corpus including deliberately malformed PDFs
│   ├── integration/
│   └── fuzz/
└── docs/
```

Later additions: `crates/ypdf-sign`, `ypdf-forms`, `ypdf-convert`, `apps/server`.

---

## 3. Milestones

Week numbers are relative effort, single developer.

### M0 — Skeleton (week 1) — DONE

- [x] Cargo workspace (edition 2024, resolver 3), workspace lints, `rust-toolchain.toml`.
- [x] `ypdf-core`: `Error` with stable codes and exit codes (spec §35), `Config` with
  the five-source layering (spec §32), `Progress` with throttled emission and ETA,
  `CancelToken`, `Preset` parsing the spec §29 TOML plus the seven built-ins.
- [x] `tracing` init, JSON log format, `OperationRecord` matching the spec §31 shape,
  path redaction on by default.
- [x] `apps/desktop` boots an eframe window, accepts dropped files.
- [x] `git init`, CI on Windows/Linux/macOS (`cargo fmt --check`, `clippy -D warnings`,
  `test`).

Stack notes from the build: egui/eframe are at **0.35**, which replaced
`SidePanel`/`TopBottomPanel` with a unified `egui::Panel`, moved the app entry point
from `App::update(ctx, frame)` to `App::ui(ui, frame)`, and defaults to the **wgpu**
renderer (glow is off, so `on_exit` takes no GL context).

### M1 — Viewer (weeks 2-4) — spec §2.1 — DONE

- [x] PDFium vendored per-triple under `vendor/pdfium/<triple>/bin/`, fetched by
  `scripts/fetch-pdfium.{ps1,sh}`, pinned at `chromium/8009` (PDFium 153.0.8009).
  Loading searches `YPDF_PDFIUM_PATH`, the vendor directory beside the executable and
  up its ancestors, the executable's own directory, then the system library, and
  reports every path it tried when it fails (Risk R1).
  The **non-V8** build is deliberate: yPDF detects JavaScript (spec §20, §26) and must
  never be able to execute it.
- [x] Render thread owning the one `Pdfium` instance; priority queue (visible >
  nearby > thumbnail), LIFO within a lane, de-duplicated by page+width+rotation,
  thumbnails capped at 64 queued.
- [x] Cancellation by **generation counter** rather than a token per request: a zoom or
  rotation bumps the document's generation and everything older is dropped, in the
  queue and again immediately before rendering.
- [x] Page canvas: zoom steps, fit-width, fit-page, rotate, continuous scroll, every
  page allocated its box up front so the scrollbar never jumps.
- [x] Thumbnail sidebar at a fixed 192 px render width, so zooming the document never
  invalidates it.
- [x] Byte-budgeted LRU texture cache (`cache.texture_budget_mb`, default 512), keyed by
  page + pixel width + rotation + inversion; nearest-width raster used as a stand-in
  while the sharp one renders.
- [x] Multi-tab, dark theme, page-colour inversion (I), full-screen (F11),
  drag-and-drop, recent files, keyboard shortcuts, file paths on the command line.

**Gate:** a synthetic 500-page document opens and renders; RSS 348 MB in a **debug**
build, under the 500 MB ceiling. Two parts of the gate are not yet verified: frame
timing has not been measured, and the corpus has no genuine 200 MB file — both need
`tests/fixtures/` to grow real documents.

Deferred out of M1: a password prompt (the engine takes a password; nothing asks for
one yet) and printing.

Render performance observed: ~4 ms per Letter page at 1024 px, debug build.

### M2 — Text layer (week 5) — spec §2.1 — DONE

- [x] Text extraction as characters **with boxes**, not a flat string: every operation
  that follows — highlighting, selection, copy — has to map a position in the text back
  to a rectangle on the page, so the two are kept aligned from the start. A character
  PDFium gives no box for is dropped rather than silently desynchronizing the pair.
- [x] Search with case-sensitive and whole-word toggles, hit count, next/previous with
  wrap, highlights drawn per line so a match spanning a line break is not painted as one
  enormous box. Navigating to a hit also selects it, so "find" and "copy what I found"
  are one gesture.
- [x] Search is incremental: the query is kept and re-run against each page as its text
  arrives, so results accumulate while the user is already reading them, instead of
  blocking until the whole document is extracted.
- [x] Text selection by dragging, hit-tested against character boxes, `Ctrl+C` copies.
- [x] Links: hover cursor, click to follow. External URLs go to the system browser —
  the one place the application reaches outside itself, and only on an explicit click.
  Internal links jump to their page. **Launch actions are never followed**; they run a
  program, and yPDF reports them (spec §20) rather than offering them as a link.
- [x] All text, link, and selection geometry lives in unrotated PDF point space and is
  mapped to the screen in one place (`viewer::pt_to_screen`), so rotation and zoom work
  without the extraction code knowing either exists. The mapping is round-trip tested at
  all four rotations.

Text extraction runs on the render thread but strictly *behind* every raster, so
searching a whole document cannot stall the page being read.

Deferred: selection does not span pages — stitching page text layers into one reading
order belongs with the extraction work, not with the mouse handling. There is also no
results *list* panel; the find bar shows a count and steps through hits.

### M3 — Page management (weeks 6-7) — spec §3 — DONE (completes spec Phase 1)

- [x] `ypdf-doc` on lopdf — pure Rust, never loads PDFium, so its operations are safe to
  run in parallel across files. That is what will make batch processing (spec §21) cheap.
- [x] **The page tree is flattened on load.** PDF allows an arbitrarily nested page tree
  with attributes inherited down it, and reordering pages inside such a tree while
  preserving inheritance is where page editors quietly corrupt files. Inherited
  `/Resources`, `/MediaBox`, `/CropBox` and `/Rotate` are copied onto each page first,
  then the tree becomes one level whose `/Kids` array *is* the page order. Operations
  after that just permute a list of object ids; no object is ever rewritten.
- [x] extract, delete, duplicate, reorder, move, reverse, rotate, insert-from, replace.
  Deleting every page is refused — a PDF with no pages is not a document.
- [x] Merge, with object renumbering on import; merging a document with itself is tested,
  because that is where a naive implementation silently overwrites half the file.
- [x] Split by each page, every N pages, page ranges, and bookmarks. Splitting an
  unbookmarked file by bookmarks yields one piece rather than an error — refusing would
  fail on exactly the files people try it on first.
- [x] `PageSpec`: `1-20,30,45-`, shared by the GUI, the CLI, and project files. Order is
  preserved and repeats are kept, so the same expression can select *or* reorder.
- [x] **Undo is an operation log replayed from the original file**, not a stack of saved
  document states (Risk R4 resolved). A PDF state is megabytes; an operation is a few
  bytes. It also means undo cannot desynchronize from what the file says, because there
  is only one source of truth and it is on disk. A rejected operation is removed from
  the log, so one bad edit cannot poison every later replay.
- [x] GUI: sidebar multi-select (click / `Ctrl` / `Shift`), drag to reorder, a Pages menu,
  `Ctrl+Z`, `Ctrl+S`, Save as, Open, and an unsaved-edits marker.
- [x] Edits reach the viewer through `RenderHandle::reopen`: `ypdf-doc` produces the
  edited bytes and PDFium re-reads them under the same document id, so the tab keeps its
  place and everything derived from the old contents is dropped.

Every page-operation test saves the document and re-opens it before asserting. Anything
less passes on a document lopdf holds happily in memory but writes out corrupt, which is
the failure mode that matters.

Deferred: a merge dialog with reordering and previews across several files (the engine
merge and the ops are there; the multi-file UI is not), and page-level clipboard.
`reopen` re-serializes the whole document per edit — fine for ordinary files, worth
revisiting for very large ones.

### M4 — Metadata, diagnostics, security scanner (weeks 8-9) — spec §13, §14, §20, §26 — DONE

- [x] Metadata read and edit. PDF text strings are either PDFDocEncoded or UTF-16BE with
  a byte order mark, and dates look like `D:20240305142201+08'00'`; both are decoded, and
  edits are written back as UTF-16 so a non-ASCII title survives. Clearing a field
  removes the key rather than storing an empty string — a reader shows "(none)" for the
  first and a blank box for the second, and the first is what clearing means.
- [x] `/Creator`, `/Producer` and the dates are shown read-only. They describe who made
  the file; offering to edit them would be offering to lie about provenance.
- [x] XMP packet is read and shown. It is **not** rewritten to match an `/Info` edit — a
  disagreement between the two is reported as `D_XMP_STALE` instead, because silently
  rewriting someone's XMP is a bigger change than they asked for.
- [x] Diagnostics (spec §14): version, pages, size, objects, fonts, images, annotations,
  form fields, embedded files, encryption, linearization, and the PDF/A part the XMP
  *claims* — labelled as a claim, since validating it is spec §15 and a later milestone.
- [x] Structural issues: undecodable streams, dangling references, oversized images, and
  a document with no fonts at all (which is the signal that its pages are scans and want
  OCR).
- [x] **Security scanner** (spec §20, §26) in its own crate. Detects JavaScript, launch
  actions, open-actions and event-triggered actions, embedded files, embedded
  executables, rich media, XFA forms, remote destinations, signature fields, encryption,
  and every external URL — classified Info / Warning / High / Critical.
- [x] URLs are classified from their text alone, with no network access ever: `javascript:`
  and `file:` schemes, bare IP addresses, and punycode hostnames are each called out.
  Userinfo is stripped before the host is read, so `https://www.bank.example@192.0.2.1/`
  is reported as what it is.
- [x] The scanner walks *nested* dictionaries, not just indirect objects — an annotation
  carries its action inline, so most launch actions and URIs never appear as objects of
  their own.
- [x] GUI: an Inspect panel with Metadata / Diagnostics / Security tabs, and a toolbar
  badge coloured by the worst finding. External links are listed as plain text, never as
  clickable links: the list exists so someone can read where a file wants to send them.

Two rules the scanner is built on:

* **Nothing found is ever executed.** The vendored PDFium has no JavaScript engine, this
  crate only reads bytes, and the viewer refuses to follow launch actions. Detection must
  never become execution.
* **Findings are evidence, not verdicts.** A form that submits to a URL is ordinary in an
  expense claim and alarming in an invoice from a stranger. The scanner reports what is
  in the file and how unusual it is; the person reading supplies the context.

`tests/fixtures/hostile.pdf` is a hand-built file carrying an open-action that runs
JavaScript, a launch action, an embedded `.exe`, a `javascript:` link, a link to a bare
IP address, event-triggered actions, and a rich-media annotation. All inert; the test
suite asserts every detector fires and that scanning leaves the file byte-identical.

Deferred: attachment extraction (the scanner names embedded files but nothing saves
them out yet) and a JSON form of the reports, which arrives with the CLI at M6.

### M5 — Compression (weeks 10-11) — spec §4 — DONE

- [x] `ypdf-optimize`: image recompression, unreferenced-object removal, stream
  compression, metadata stripping, and a report that names what it refused.
- [x] Image downsampling to a target DPI, computed from how wide the image is actually
  *drawn* on its page rather than from its pixel count. Without page geometry "150 dpi"
  has no meaning, and a full-page scan would be downsampled as though it were a
  thumbnail. Where the display size is unknown the image keeps its size.
- [x] JPEG quality 20-100, with 100 meaning **resample only, never re-encode** — that is
  the lossless preset, and it is a real guarantee rather than a high quality number.
- [x] Every re-encode is compared against the original and **kept only if it is at least
  10% smaller**. That makes a second pass over an already-compressed file safe, which
  is the case that breaks naive compressors: they re-encode JPEG as JPEG and the file
  grows while the pictures get worse.
- [x] Anything not fully understood is left byte-identical and counted: JPEG 2000, JBIG2,
  CCITT fax, CMYK and indexed colour, 16-bit samples, and stencil masks. Re-encoding one
  of those from a partial understanding is how a compressor corrupts a document.
- [x] Presets wired to spec §29's builtins (Maximum Compression, Balanced, Print PDF, …),
  with the settings visible and editable rather than hidden behind a level name.
- [x] GUI: a Compress dialog with the before/after statistics block from spec §4, the
  count of images left alone *with reasons*, and a "Not done" line for requested work
  this build cannot do.
- [x] Compression runs **into memory only**. Nothing is written until the user picks a
  file; Discard restores the document from the op-log. It is the one operation that
  destroys information, so the original is never the thing being overwritten. The status
  bar carries "Compressed — not saved" while a result is pending.
- [x] Metadata stripping is opt-in and separate from size: it removes `/Info` and the XMP
  packet, which is a privacy and provenance decision rather than a compression one.

Not done, and the report says so at runtime rather than silently skipping:
**linearization** and **font subsetting** — both need qpdf (R6), which arrives with the
encryption work at M7. PNG optimization goes through `image` + `flate2` rather than
oxipng; oxipng's gains are real but small next to downsampling, and it is another
dependency for the tail of the benefit.

Tested against a document generated in the test itself rather than committed — three
page-sized photographic images plus a CMYK image and a tiny one. The assertions are
better than half the file removed, lower settings producing a smaller file, the CMYK
image refused with a stated reason, linearization reported as not done, a second pass
not ballooning the file, and a text-only document coming through with its text
extractable and its page count unchanged.

### M6 — CLI + batch (weeks 12-13) — spec §21, §22 — DONE

Pulled forward from spec Phase 5. It needed no new engine work beyond image extraction,
and it doubles as the integration test harness for every engine crate.

- [x] `ypdf-cli info | diagnostics | security-scan | metadata | merge | split | extract |
  compress | images`. The binary is `ypdf-cli` rather than `ypdf` because the desktop
  application already owns that name.
- [x] `--json` on stdout, human text on stdout, **everything else on stderr** — progress,
  per-file lines, warnings, and the `tracing` log, which was writing to stdout until this
  milestone and would have landed inside the JSON a script was parsing.
- [x] Stable exit codes straight from `Error::exit_code`, so `E_PARSE_XREF` is 3 whether
  it came from a batch of one or a batch of a thousand. A run where anything failed exits
  with **that failure's code**, not a generic 1, so a script can still branch on it.
- [x] `--verbose`, `--quiet` (quiet wins when both are given), `--config`, `--workers`,
  `--overwrite`.
- [x] Batch (spec §21): globs expanded **by the tool**, because Windows shells do not
  expand them and a batch feature that works on one platform is not a batch feature.
  Matches are sorted, so `chapter-*.pdf` merges predictably. rayon over files, per-file
  error isolation, and the summary block from spec §21.
- [x] Results come back in **input order** whatever order the workers finished in, so two
  runs of the same batch produce the same output and a diff between them means something.
- [x] `security-scan --fail-on <severity>` as a pipeline gate: a flagged file exits 1
  while still reporting `"ok": true`, because the scan succeeded — it found something.
  That is a different thing from failing to read the file, and the exit codes say so.
- [x] `images` writes every embedded image out. A DCTDecode stream **is** a JPEG, so it is
  copied byte for byte rather than decoded and re-encoded; everything else is written as
  PNG. Names are `page-003-img-01.jpg`, zero-padded so a directory listing sorts the way
  the document reads.
- [x] Ctrl-C sets the same cancellation token every engine operation already checks, so a
  run stops between steps instead of being killed mid-write. A half-written PDF is worse
  than no PDF. Pressing it twice exits immediately.
- [x] Every write goes through one overwrite guard, and needs `--overwrite` to replace
  anything — including an in-place `metadata --set-title`. A batch is exactly where a
  silent overwrite is both easy to trigger and impossible to undo. It has its own error,
  `E_OUTPUT_EXISTS` (exit 11), rather than being reported as a configuration mistake:
  the command was valid and the refusal protected a file.

Fourteen end-to-end tests run the built binary rather than calling the functions, because
exit codes and the stdout/stderr split only exist at that boundary and that is where a
script depends on them.

Not covered by an automatic test: the Ctrl-C **handler wiring**. The cancellation path it
drives is tested (a pre-cancelled token leaves nothing written), but delivering a real
console signal to a child process on Windows is not something the test harness can do
honestly, so it was checked by hand instead.

Deferred: `ocr` is listed in spec §22 but has no engine behind it until M7, so it is not
offered as a command that would only ever fail. Watermark, encrypt, and convert are the
same, and arrive with their milestones.

### M7a — Encryption (week 14) — spec §8 — DONE

**R6 dissolved.** qpdf turned out to be unnecessary: lopdf 0.44 implements the standard
security handler, so encryption is pure Rust with no native build and no second PDF
library in the process. What qpdf is still wanted for is linearization and font
subsetting, which stay unimplemented and stay reported as "Not done".

- [x] `ypdf-crypt`: AES-128 (V4/AESV2) and AES-256 (V5/AESV3), user and owner passwords,
  and the full permission set. RC4 is **read** but never written — producing new files
  with a cipher broken since 2001 while calling it encryption would be a lie told in a
  dropdown.
- [x] Encryption is a property of the **written file**, not of the document in memory.
  `encrypt` returns bytes rather than mutating a `Pdf`, because the objects have to be
  plaintext to be worked on and ciphertext to be stored; there is no state in which a
  `Pdf` "is the encrypted one" and no way to save one by accident.
- [x] Opening decrypts, and `/Encrypt` is moved out of the trailer at load. A trailer
  still pointing at an encryption dictionary while the objects sit in memory as plaintext
  describes a file that does not exist, and saving it would hand a reader plaintext it
  would try to decrypt. The state is kept to one side so the protection can still be
  *reported* without the document lying about what it is.
- [x] Because of that, saving a protected document writes an **unprotected** copy. That is
  the only honest behaviour — but it is a bad surprise, so it is announced three times:
  in the status bar, in the Inspect panel, and in the log when the CLI does it.
- [x] Permission flags are carried and reported, and everywhere they appear the interface
  says they are **advisory**: conforming readers honour them, nothing enforces them, and
  the open password is what actually protects the file. Someone who believes "cannot
  copy" is enforced will put a secret in a document and send it to a stranger.
- [x] Extraction for accessibility is always permitted, whatever else is switched off.
  PDF 2.0 requires the bit, and a document a screen reader cannot read has not been
  protected — it has been broken for some of its readers.
- [x] `/ID` is generated when a document has none: it feeds the key derivation for
  everything before AES-256, and hand-built files often have no identifier at all.
- [x] AES-256 bumps the header to PDF 2.0. A 1.7 header over an AESV3 filter is a file a
  strict reader is entitled to reject.
- [x] CLI: `encrypt` and `decrypt`, plus a global `--password` — read from `YPDF_PASSWORD`
  when the flag is absent, because a password in an argument is visible to anything that
  can list processes. `info`, `diagnostics`, and `security-scan` report the protection.
- [x] GUI: a password prompt that says **which** of the two problems it hit — no password
  or the wrong one, since "could not open" leaves someone retyping a password that was
  never going to work — and a Protect dialog with the passwords, the cipher, and the
  flags.
- [x] The edit session keeps the password in memory for as long as the document is open,
  because every undo replays from the original file. It is never written anywhere.

**One rule the whole crate is built on: there is no way here to open a document whose
password nobody has.** Recovering a lost password is password cracking whatever the menu
calls it, and a tool that offers it is a tool for opening other people's files.

Cross-checked against a second implementation: `ypdf-crypt` writes the encryption through
lopdf and **PDFium reads it back**, for both ciphers, in
`crates/ypdf-render/tests/render.rs`. A file only this project could open would be worse
than no encryption feature at all.

### M7b — OCR (weeks 15-17) — spec §7 — DONE

**Tesseract is a sidecar process, not a linked library.** Linking it means Leptonica,
vcpkg, and a build that breaks on the machines least able to fix it. Running it means one
`Command` and a text format that has been stable since Tesseract 3.05. That is the answer
to **R2**, and it costs nothing at build time on any platform.

- [x] `ypdf-ocr`: binary discovery, language checks, TSV parsing, and the text layer.
- [x] Discovery gets the same treatment as PDFium: `YPDF_TESSERACT_PATH`, a vendored copy,
  the usual install locations, then the PATH — and when nothing is found, the error
  **names every path that was tried**. "OCR unavailable" is a dead end; a list of paths is
  something someone can act on.
- [x] Language packs are checked **before any work starts**, and a missing one is reported
  as `language pack(s) not installed: jpn. Installed: eng, msa` rather than as Tesseract's
  message about traineddata files.
- [x] **The scan is never modified.** The recognized words go into a second content
  stream in text render mode 3, which draws nothing: the page looks exactly as it did.
  A wrong text layer costs a bad search hit; replacing the image with recognized text
  would cost the page, and no accuracy figure makes that a good trade on someone's only
  copy of a document.
- [x] Pages that already have text are skipped by default. Laying a guess over real text
  gives search two answers for the same words, and the guess is the worse one. `--force`
  overrides it.
- [x] Words below a confidence threshold are dropped. A layer full of low-confidence
  guesses makes search *worse*: every false hit costs a page turn.
- [x] Words the layer font cannot encode — Chinese, Arabic, Devanagari — are **counted and
  reported**, not written. Mojibake in a text layer is worse than an absence because it
  looks like data. This is the honest limit of a Helvetica/WinAnsi layer; a CID font would
  lift it and is not in this build.
- [x] The page raster is written to a temporary file for Tesseract to read, and removed
  when the operation ends whatever happens. That page is the user's document; leaving
  copies of it in a world-readable temporary directory is a privacy leak, not a
  housekeeping detail (spec §27).
- [x] CLI: `ocr` with `--lang`, `--dpi`, `--pages`, `--min-confidence`, `--force`, and
  `--tesseract`. It runs its own sequential loop rather than the parallel batch runner,
  because PDFium rasterizes on one thread by design.
- [x] GUI: an OCR dialog with progress and a page counter. The work is split three ways —
  the render thread rasterizes, a worker thread recognizes, the UI thread only passes
  rasters along — so the window keeps drawing throughout, and the run can be stopped.
- [x] The result is held in memory until the user saves it, like compression. A text layer
  is an addition rather than a loss, but it is still a change to someone's document.

Tested against a **stand-in for Tesseract**: a script that prints canned TSV. The contract
is small enough to fake — take an image path, print TSV — so discovery, invocation,
parsing, coordinate mapping, the skip rule, engine failure, and whether the words come
back out of the finished document are all covered without the real binary. The CLI test
goes further and rasterizes through real PDFium first.

What that deliberately does not cover is **recognition quality**, which is Tesseract's
business and about which a fake can say nothing. The one test that needs a real
installation is marked `#[ignore]` with the reason in its name, so it reads as skipped
rather than as passing. There is no Tesseract on the machine this was built on, and that
is stated here rather than left to be discovered.

### M8 — Watermark (spec §12) — DONE

- [x] `ypdf-watermark`: text and image watermarks, over the page or behind it.
- [x] **Everything it does is additive.** One content stream and the resources that stream
  needs; nothing the page already draws is touched, moved, or re-encoded. Removing a
  watermark applied this way means deleting one object, which is what makes it safe to
  stamp a document someone still has to work on.
- [x] The stream is wrapped in `q`/`Q` and its text in `BT`/`ET`, so nothing it sets —
  colour, transparency, the matrix — can leak into the page's own drawing. A watermark
  that changed the fill colour of the document underneath it would be a very hard bug to
  find later, and the test suite asserts the operators balance.
- [x] Positions: nine anchors plus **tiled**, which is the one that cannot be cropped off.
  Rotation, opacity via `ExtGState` (`CA` and `ca`, so it covers text and images alike),
  scale, colour, and a choice of Helvetica or Helvetica-Bold.
- [x] Text is placed using **real Helvetica metrics**, not an average advance. The glyphs
  are visible here, unlike the OCR text layer's, so "centred" has to actually be centred.
  Characters outside the table fall back to an average, which is a percent or two off and
  better than refusing to stamp.
- [x] Image watermarks carry their **alpha through as an `/SMask`**. A logo is mostly
  transparent, and one pasted onto a white rectangle over someone's page is not a
  watermark — it is a hole in the document. A JPEG goes in byte for byte, since it is
  already a DCTDecode stream.
- [x] Text the built-in fonts cannot draw is **refused**, and the CLI and GUI both fail
  rather than writing a copy that looks identical to its input. Reporting success for a
  file nothing happened to sends someone looking for the problem in the wrong place.
- [x] An opacity of zero is refused for the same reason: it would write an object nobody
  can see and report success.
- [x] CLI: `watermark` with `--text`/`--image`, `--position`, `--rotation`, `--opacity`,
  `--scale`, `--size`, `--colour`, `--pages`, and `--under`.
- [x] GUI: a dialog with the four presets from spec §12 (CONFIDENTIAL, DRAFT, COPY,
  INTERNAL), live settings, and the same hold-in-memory-until-saved rule as compression
  and OCR.

The WinAnsi encoder moved to `ypdf-doc` in this milestone: OCR and watermarking both need
one and they have to behave identically, since both refuse rather than substitute.

**Verified by rendering, not by inspection.** A content stream can be well-formed and
still draw nothing — a bad matrix, a missing resource, an opacity that never reaches the
page. `crates/ypdf-render/tests/render.rs` rasterizes stamped and unstamped copies through
real PDFium and compares pixels: the stamp must change a real share of the page, a
background stamp must too, and stamping page 2 must leave page 1 byte-identical.

### M9 — Redaction (spec §11) — DONE

The spec is explicit that a black rectangle is not redaction, and it is right: the text
stays in the file, `Ctrl+A` still copies it, and every "redacted document" scandal of the
last twenty years has been exactly that mistake. So `ypdf-redact` takes things out.

- [x] **Glyphs are deleted from the content stream.** The page's operators are decoded,
  the graphics and text state are tracked — `cm`, `q`/`Q`, `Tm`, `Td`, `TD`, `T*`, `Tf`,
  `Tc`, `Tw`, `Tz`, `Ts`, and all four show operators — and every glyph inside a
  rectangle is removed from the stream that gets written back.
- [x] Each removed glyph is replaced by the **kerning that advances past it**, so the
  text on either side does not move. A redaction that reflowed the line would be obvious,
  and obvious in a way that tells the reader how long the removed text was.
- [x] **Widths come from the document's own fonts** — `/Widths`, or the base-14 metrics
  for Helvetica, Times, and Courier. Guessing produces drift, and by the middle of a line
  a guess can be a whole word out: a redaction a word out either leaves the secret behind
  or eats the sentence.
- [x] **Image pixels are overwritten in the image itself**: the overlapping region is
  decoded, filled, and re-encoded as deflated RGB. Not JPEG — re-encoding a cleared region
  as JPEG would leave ringing around the black box that traces the shape of what was
  removed. `/SMask` and `/Mask` are dropped so nothing shows through.
- [x] An image drawn on more than one page is **copied first**, so redacting page 4 does
  not silently blank the same logo on page 1.
- [x] Paths drawn entirely inside a rectangle are dropped; **annotations that overlap one
  are deleted**, since a link or a comment carries its own text and its own target,
  neither of which lives in the content stream.
- [x] An opaque box is drawn last. It is **not** the redaction — it covers vector artwork
  that crosses the boundary and cannot be split, and it makes the removal visible as a
  deliberate act. `--no-cover` turns it off, and the text is still gone; there is a test
  that asserts exactly that, and if it ever fails the crate has become the thing spec §11
  forbids.
- [x] Two things it cannot do precisely, both **reported** rather than hidden: text in a
  font it cannot measure (composite fonts, whose CMap it does not read) is removed a whole
  show operation at a time, and an image it cannot decode has its draw removed entirely.
  In both cases more comes out than was asked for, which is the safe direction — and the
  report says so in words.
- [x] CLI: `redact` with `--find` (search-and-redact, through PDFium's character boxes) and
  `--rect page:x0,y0,x1,y1`. **A `--find` term that matches nothing is an error**, names
  the term, and writes no file. Being told "done" when the pattern was wrong is the most
  dangerous answer this command can give, because the file then gets sent.
- [x] GUI: a Redact panel, drag-to-mark on the page in translucent red — a colour nobody
  could mistake for a finished redaction — plus "Mark all" for a search term. Marking and
  applying are separate steps, because applying cannot be undone.

**The test that matters** asserts the redacted string is absent from the saved file
entirely: not just from `extract_text`, but from the raw bytes and from every stream in
the document, decompressed. A compressed content stream would hide text from a plain
search while leaving it perfectly recoverable, and that is the failure this is looking
for. Verified again by hand, outside the test suite, with an independent script.

### M10 — Bookmarks and links (spec §17, §18) — DONE

Both are structure rather than content — they say where to go, not what a page shows — so
`ypdf-outline` handles them together and neither touches a content stream.

- [x] Bookmarks read as a tree, written as a tree. Writing **replaces the whole outline**:
  editing the linked list in place means repairing `/Prev`, `/Next`, `/First`, `/Last`,
  `/Parent`, and `/Count` on every touched node, and a mistake in any one of them produces
  an outline that looks right in one reader and is empty in another.
- [x] Every walk is depth- and cycle-limited. `/First` and `/Next` are pointers a producer
  can get wrong, and a `/Next` that points backwards would otherwise fill memory with the
  same three bookmarks.
- [x] Titles are PDF text strings, written as UTF-16, so an outline is **not** limited to
  what a base-14 font can draw — unlike the watermark, which has to draw its text.
- [x] An empty tree removes `/Outlines` entirely rather than leaving an empty one, which
  some readers show as a blank panel.
- [x] A bookmark pointing past the last page is written with no destination rather than
  quietly aimed at whatever page happens to be last.
- [x] Links list what they are: web addresses, `mailto:`, internal pages, and pages of
  other documents. Anything else — a named destination, a launch action, embedded
  JavaScript — is listed as what the file calls it. **A listing that quietly omitted the
  launch action would be worse than useless: it would be reassuring.**
- [x] `Target::Other` cannot be created. Nothing here builds a `/Launch` link: it runs a
  program on the reader's machine, and a tool that offers that in an "add a link" menu is
  a tool for building a trap. `ypdf-security` reports them; this creates none.
- [x] CLI: `bookmarks` (list, `--add Title=page`, `--set tree.json`, `--clear`) and `links`
  (list, `--add page:x0,y0,x1,y1=target`, `--remove-all`, `--remove-external`,
  `--remove-matching`). Both **read by default** and only write when asked; a tool that
  rewrote the outline of every file it was pointed at is one people learn to fear.
  `--set` takes the same JSON the command prints, so an outline can be read out, edited,
  and put back.
- [x] `--remove-external` is the one people actually want before a document leaves the
  building: web, email, and cross-document links go, internal navigation stays.
- [x] GUI: a Bookmarks panel that lists the outline, jumps on click, and adds, renames, and
  removes. Deleting a heading **keeps what was under it** — the children move up rather
  than a chapter of navigation vanishing with one click.

One design correction found while wiring the panel: an outline edit is recorded as an
operation in the edit log, like a page edit, rather than written straight into a copy.
Otherwise adding a bookmark and then rotating a page would silently drop the bookmark,
because every replay starts from the original file. It now takes part in undo for free.

### M11 — Forms (spec §16) — DONE

- [x] `ypdf-forms`: AcroForm detection, every field kind, fill, clear, flatten, and
  JSON / FDF / XFDF interchange.
- [x] Fields are read by **walking the field tree with inherited attributes**, the same
  shape of problem as the page tree in `ypdf-doc`. Type, flags, and options can each come
  from an ancestor, and a reader that skips inheritance mislabels half of a real form.
- [x] **Value and appearance are one job.** Writing `/V` without rebuilding the appearance
  stream is the classic form bug: the file looks filled in one reader, empty in another,
  and prints blank. Filling writes both, and sets `/NeedAppearances` as well so conforming
  readers can redo the work with their own typography.
- [x] A checkbox is turned on with **the state its own appearance dictionary names**, not
  the `/Yes` everyone assumes. The test fixture deliberately uses `/On`: writing `/Yes`
  into it would give a box that is ticked in the data and blank on the page.
- [x] A radio group is one field with several widgets, and exactly one widget ends up
  showing an on-state.
- [x] **A signature field is never filled.** Putting a value in one produces a document
  that claims to be signed and is not. It is listed, and refused, with the reason.
- [x] A read-only field is listed and not written: whoever built the form marked it that
  way, and overriding it quietly produces a document that disagrees with itself.
- [x] **A name that does not exist is reported.** A typo in a field name would otherwise
  look exactly like a successful fill, which is how a form gets sent in blank. In the CLI
  it also flags the run, so a script exits non-zero.
- [x] Flattening draws each widget's current appearance into the page, removes every
  widget, and removes `/AcroForm` — a document with a form dictionary and no fields makes
  some readers show an empty form bar. One-way, and a separate button that says so.
- [x] FDF and XFDF are written and parsed by hand: both are small, and the alternative is
  a dependency that would parse far more of them than this needs. Round-trip tested with
  the characters that break each format — parentheses for FDF, angle brackets and
  ampersands for XFDF.
- [x] Export is sorted, so exporting the same form twice produces the same bytes and a
  diff between two filled copies shows what actually differs.
- [x] CLI: `forms` — list, `--fill name=value`, `--import`, `--export`, `--clear`,
  `--flatten`. Reads by default; exporting counts as a read, since it writes a data file
  rather than the document.
- [x] GUI: a Form panel listing every field with the right editor for its kind. Only
  **edited** values are sent, so applying does not rebuild appearances for fields nobody
  touched. Importing puts values into the panel rather than the document, where they can
  be read before Apply writes them.

`tests/fixtures/form.pdf` is hand-built to catch the easy mistakes: a checkbox whose
on-state is `/On`, a radio group of two widgets under one field, a dropdown with options,
and a signature field that must come through untouched.

**Verified by rendering.** `crates/ypdf-render/tests/render.rs` fills a form, rasterizes
it through real PDFium, and requires the page to have changed — a value with a stale
appearance would pass every structural test and print blank. The same check runs after
flattening, so flattening cannot silently lose what was typed.

### M12 — Annotations (spec §10) — DONE

- [x] `ypdf-annot`: ten shapes — highlight, underline, strikeout, sticky note, text box,
  freehand ink, rectangle, ellipse, line (with an optional arrowhead), and stamp — each
  added, listed, and removed.
- [x] **Every annotation carries its own appearance stream.** The same rule the forms work
  arrived at: a `/Rect` and a colour with no `/AP` is drawn by whichever reader feels like
  it, differently in each, and prints blank in some. Each shape is drawn into a Form
  XObject, so the mark on screen is the mark on paper. An integration test asserts the
  stream exists and the print flag is set for every kind.
- [x] Highlights are drawn with `/BM /Multiply` in an `/ExtGState`, so the text underneath
  stays readable instead of being painted over by a flat block of colour.
- [x] Quad points are written **top-left, top-right, bottom-left, bottom-right**. The order
  in the specification and the order readers actually accept are the same here, and the
  bow-tie a naïve corner order produces is one of the classic broken-highlight bugs; a
  test pins it.
- [x] Ellipses are four Bézier arcs, not a polygon: a circle drawn from line segments
  looks fine at 100% and faceted the moment anyone zooms in to read.
- [x] **A stroke has width, and the bounding box has to know.** `bounds_with_width` pads a
  line or an ink path by the stroke, plus extra for an arrowhead. Found by the render
  test: a perfectly horizontal line has zero height, and was refused as "no area".
- [x] Text that the built-in font cannot draw is **not silently dropped**. The comment
  stays in `/Contents`, where every reader shows it, and the log says the painted copy is
  incomplete — the same refuse-rather-than-substitute rule as the OCR layer.
- [x] Removal is by predicate, so `--remove-author` deletes one reviewer's marks and leaves
  the rest. **Form fields and links are not annotations for this purpose**: they live in
  the same `/Annots` array, and a "clear annotations" that emptied it would quietly
  destroy a form. Two tests hold that line.
- [x] CLI: `annotate` — lists by default; `--highlight`, `--underline`, `--strike`,
  `--note`, `--text-box`, `--rect`, `--ellipse`, `--line`, `--stamp`, each repeatable, with
  `--colour`, `--opacity`, `--width`, `--author`, `--comment`, `--arrow`, and
  `--remove-all` / `--remove-author`.
- [x] GUI: an Annotate panel with a tool in hand. Drawing tools take the drag and show the
  shape as it is dragged out; the text tools leave the drag to selection and mark whatever
  the selection covered. The panel lists what is on the document, jumps to the page, and
  removes one or all.

**Verified by rendering.** `crates/ypdf-render/tests/render.rs` marks up a page, rasterizes
it through real PDFium, and requires the pixels to have changed — and, for a highlight laid
over the page's own text, requires the count of dark pixels not to drop, so a highlight
cannot paint over the words it was meant to mark.

### M13+ — Professional features, in difficulty order
1. **PDF/A validation** (spec §15).
2. **Digital signatures** (spec §9) — PAdES, CMS/PKCS#7, certificate management,
   verification. Hardest single item in the spec; budget 4+ weeks on its own.
3. **REST API** (spec §23, §24) — `apps/server`, axum + tokio, async job system reusing
   `ypdf-jobs`.
4. **Plugin system** (spec §33).

---

## 4. Scope Cuts

Deliberately out of the MVP:

- **DOCX / XLSX / PPTX to PDF.** No sane Rust path. Means bundling headless LibreOffice
  and shipping hundreds of MB. Ship as an optional plugin later, or not at all.
- **PDF to HTML with layout fidelity.** Same class of problem. PDF to TXT and PDF to
  Markdown stay; both are cheap on top of the pdfium text layer.
- **Mobile and WebAssembly** (spec §37 "Future"). PDFium and Tesseract both complicate
  wasm badly. Revisit only after the desktop product is stable.

Kept and cheap: Markdown / HTML / images / TXT to PDF via printpdf.

---

## 5. Risks

| ID | Risk | Mitigation |
|---|---|---|
| R1 | PDFium binary distribution — needs per-platform `.dll` / `.so` / `.dylib`, wired into CI and installers. | Solve during M1, not later. Vendor prebuilt binaries, checksum them, add a startup diagnostic for a missing or mismatched library. |
| ~~R2~~ | ~~Tesseract C dependencies on Windows (vcpkg friction).~~ **Retired at M7b:** Tesseract is driven as a sidecar process, so nothing links against Leptonica and the build is unchanged on every platform. The cost moved to discovery, which is handled the same way as PDFium. | — |
| R3 | egui has no text selection over an image. | Custom hit-testing against pdfium char boxes. About 2 days. |
| R4 | Undo is not universally possible on PDF mutations. **Confirmed at M9:** redaction is applied to a replayed copy and held in memory until saved, and the interface says it cannot be undone before the button is pressed. | Operation log replayed from the original document, with a memory cap and an explicit "cannot undo" state for destructive ops (redaction, flatten, encrypt). |
| R5 | Large-file memory blowup (spec §25 explicitly forbids full-file loads). | Lazy page access, LRU texture cache with a byte budget, memory-map inputs where the backend allows, streaming writes. Enforced by the M1 gate. |
| ~~R6~~ | ~~`qpdf-rs` binding maturity and build friction.~~ **Retired at M7a for encryption:** lopdf implements the standard security handler, so no qpdf and no native build. Still open for linearization and font subsetting, which remain unimplemented and are reported as such. | Isolate any future qpdf call behind a `ypdf-doc` trait so it can be swapped for a `qpdf` CLI sidecar without touching callers. |

---

## 6. Cross-Cutting Requirements

Applied from M0 onward, not bolted on later:

- **Errors** (spec §35): stable codes, human message, cause, suggested action, JSON form.
- **Cancellation**: every long operation takes a `CancelToken`. No exceptions.
- **Progress**: every long operation reports percent and ETA over a channel.
- **Privacy** (spec §27): no network calls in the engine; telemetry off and absent by
  default; temp files in a per-run directory with a secure-delete option.
- **Testing** (spec §36): unit tests per crate; integration tests driven through the CLI;
  `cargo-fuzz` targets on the object parser, xref parser, and stream decoder from M4;
  a regression corpus of malformed PDFs in `tests/fixtures/`.
- **Presets** (spec §29) and **project workflows** (spec §30) read the same TOML types
  defined in `ypdf-core` at M0.

---

## 7. Immediate Next Steps (M0)

1. Create the workspace `Cargo.toml` and `crates/ypdf-core`.
2. Define `Error` (with codes), `Config` (five-source layering), `Progress`,
   `CancelToken`, and `Preset` in `ypdf-core`.
3. Initialize `tracing` with JSON output and operation IDs.
4. Create `apps/desktop` with a booting eframe window.
5. `git init`, `.gitignore`, CI running fmt / clippy / test.
6. Then M1: vendor PDFium and get the first page on screen.
