# yPDF

A high-performance, privacy-first PDF toolkit for developers, automation, and
power users. Native Rust GUI, shared engine crates, scriptable CLI.

Nothing leaves the machine. No upload, no telemetry, no network calls in the
engine.

## Status

**M14a — viewer, editor, CLI, encryption, OCR, watermarks, redaction, navigation, forms, annotations, PDF/A checking, and signature verification.** Opens and renders PDFs:
continuous scroll, zoom and fit modes, rotation, thumbnails, multiple tabs,
inverted pages for dark reading, full-document search, text selection and copy,
clickable links. Pages can be selected, reordered by dragging, rotated,
duplicated, deleted, extracted, and merged, with undo and save. The Inspect panel
reads metadata, structural diagnostics, and a security scan, and the Compress
dialog shrinks a file with before/after statistics. `ypdf-cli` does all of it
from a script, over one file or a thousand. Documents can be protected with a
password and permission flags, and protected documents can be opened, read, and
written back out. Scans can be made searchable with OCR, and pages can be
stamped with a text or image watermark, and content can be redacted —
removed from the file, not covered over. Bookmarks and links can be
listed and edited, forms can be filled, cleared, and flattened, and a document can be
marked up with highlights, notes, freehand ink, shapes, and stamps. A file can be checked
against PDF/A — with the list of what that check does not cover printed beside the
verdict — and digital signatures can be read and verified, including what each one
actually covers.

See [PLAN.md](PLAN.md) for the milestone plan and [pdf-tool-spec.md](pdf-tool-spec.md)
for the full product specification.

## Layout

| Path | Contents |
|---|---|
| `crates/ypdf-core` | Errors with stable codes, layered configuration, presets, progress, cancellation |
| `crates/ypdf-render` | PDFium on a dedicated thread: document loading, page rasterization, the render queue |
| `crates/ypdf-doc` | Pure-Rust page operations: extract, delete, reorder, rotate, merge, split, metadata, diagnostics |
| `crates/ypdf-security` | Security scanning: JavaScript, launch actions, embedded executables, suspicious links |
| `crates/ypdf-optimize` | Compression: image downsampling and recompression, unused-object removal, stream compression, image extraction |
| `crates/ypdf-crypt` | Encryption: AES-128 and AES-256, passwords, permission flags, security information |
| `crates/ypdf-ocr` | OCR through a Tesseract sidecar: recognition and invisible text layers |
| `crates/ypdf-watermark` | Text and image watermarks, over the page or behind it |
| `crates/ypdf-redact` | Redaction: deleting glyphs from content streams and pixels from images |
| `crates/ypdf-outline` | Bookmarks and links: reading and editing how a document is navigated |
| `crates/ypdf-forms` | AcroForm fields: filling, clearing, flattening, and FDF/XFDF/JSON data |
| `crates/ypdf-annot` | Annotations: highlights, notes, ink, shapes, and stamps, each with its own appearance |
| `crates/ypdf-pdfa` | PDF/A checking: parts 1–3, levels a/b/u, and an explicit list of what is not checked |
| `crates/ypdf-sign` | Digital signatures: CMS/PKCS#7 verification, certificate details, and byte-range coverage |
| `crates/ypdf-cli` | Scriptable command line and batch processing (`ypdf-cli` binary) |
| `apps/desktop` | egui desktop application (`ypdf` binary) |

Engine crates (`ypdf-jobs`) are added
as their milestones start.

## Build

Fetch the PDFium binary first — it is a native library, vendored rather than
committed:

```bash
pwsh scripts/fetch-pdfium.ps1     # Windows
./scripts/fetch-pdfium.sh         # Linux, macOS
cargo run -p ypdf-desktop -- document.pdf
```

Requires Rust 1.92. On Linux, eframe needs the GTK and xkbcommon development
packages — see [.github/workflows/ci.yml](.github/workflows/ci.yml).

If PDFium cannot be found, the application says so at startup and lists every
path it searched. `YPDF_PDFIUM_PATH` overrides the search.

## Keyboard

| Key | Action |
|---|---|
| `PageUp` / `PageDown` | Previous / next page |
| `Home` / `End` | First / last page |
| `+` / `-` | Zoom in / out |
| `0` / `1` | Fit page / 100% |
| `Ctrl+F` | Find |
| `Enter` / `Shift+Enter` | Next / previous match |
| `Ctrl+C` | Copy selection |
| `Ctrl+Z` | Undo the last page edit |
| `Ctrl+S` | Save |
| `Esc` | Close find, clear selection |
| `R` | Rotate 90° clockwise |
| `I` | Invert page colours |
| `F11` | Full screen |

## Editing pages

Select pages in the thumbnail sidebar — click, `Ctrl`-click, `Shift`-click — then
use the **Pages** menu. With nothing selected, operations act on the page being
read. Drag a thumbnail onto another to move it.

Undo replays an operation log from the original file rather than keeping copies
of the document, so it costs a few bytes per edit and can never disagree with
what is on disk. Saving over the original clears the log: at that point the
earlier state is gone, and pretending otherwise would be a lie.

## Inspecting a document

The **Inspect** panel has three tabs: editable metadata, a structural report, and
a security scan. The toolbar badge is coloured by the worst thing the scan found.

The scanner reads only. Nothing it finds is executed, followed, or fetched: the
vendored PDFium has no JavaScript engine, URLs are classified from their text
without any network access, and the viewer refuses to follow launch actions.
Findings are evidence rather than verdicts — a form that submits to a URL is
ordinary in an expense claim and alarming in an invoice from a stranger, and only
the person reading knows which this is.

## Compressing

**Compress** in the toolbar opens the dialog: a preset, a target image resolution,
a JPEG quality, and switches for unused objects, stream compression, and metadata.

Two rules make it safe to run on anything:

- A re-encoded image replaces the original **only if it is at least 10% smaller**.
  Running compression twice therefore does not degrade a file — the second pass
  finds nothing worth taking.
- Anything the optimizer does not fully understand — JPEG 2000, JBIG2, CCITT fax,
  CMYK or indexed colour, 16-bit samples, stencil masks — is left byte-identical
  and reported as skipped with a reason.

The result is held in memory. Nothing is written until **Save as…**; **Discard**
puts the document back. Linearization and font subsetting are not implemented in
this build, and the report says "Not done" rather than quietly leaving them out.

## Command line

`ypdf-cli` is the same engine without the window. The desktop binary is `ypdf`; the
command line takes the longer name rather than shadowing it.

```bash
ypdf-cli info document.pdf
ypdf-cli merge chapter-*.pdf -o book.pdf
ypdf-cli split document.pdf --pages 1-20,21-40 -o out/
ypdf-cli extract document.pdf --pages 1-10,last -o excerpt.pdf
ypdf-cli compress ./invoices/*.pdf -o ./small/ --preset "Email PDF"
ypdf-cli images scan.pdf -o images/
ypdf-cli metadata report.pdf --set-title "Q1 Results" --overwrite
ypdf-cli encrypt report.pdf -o protected.pdf --user-password s3cret --no-copy
ypdf-cli decrypt protected.pdf -o plain.pdf --password s3cret
ypdf-cli ocr scan.pdf -o searchable.pdf --lang eng+msa
ypdf-cli watermark report.pdf -o stamped.pdf --text CONFIDENTIAL --opacity 0.15
ypdf-cli redact contract.pdf -o safe.pdf --find "account 12345678"
ypdf-cli forms application.pdf --fill "name=Ada Lovelace" -o filled.pdf
ypdf-cli annotate draft.pdf --highlight "1:72,700,300,720" --author Ada -o marked.pdf
ypdf-cli pdfa archive.pdf --level 2b
ypdf-cli signatures contract.pdf
ypdf-cli bookmarks report.pdf
ypdf-cli links ./inbox/*.pdf --json
ypdf-cli security-scan ./inbox/*.pdf --fail-on high
ypdf-cli diagnostics broken.pdf --json
```

Globs are expanded by the tool, so `*.pdf` works in PowerShell and `cmd` as well as in a
POSIX shell, and matches are sorted so a merge is predictable.

`--json` puts a machine-readable document on stdout and nothing else — progress,
per-file lines, and logs all go to stderr, so `ypdf-cli info x.pdf --json | jq` works.
Exit codes are stable and come from the same error type the GUI uses:

| Code | Meaning |
|---|---|
| 0 | Everything succeeded |
| 1 | Succeeded, but `--fail-on` was tripped |
| 2 | `E_IO` — the file could not be read or written |
| 3 | `E_PARSE*` — the document could not be parsed |
| 6 | `E_PAGE_RANGE` / `E_PAGE_SPEC` — bad page numbers |
| 7 | `E_UNSUPPORTED` |
| 4 | `E_PASSWORD_REQUIRED` / `E_PASSWORD_WRONG` |
| 8 | `E_CONFIG` — bad configuration, preset, or arguments |
| 11 | `E_OUTPUT_EXISTS` — an output file exists and `--overwrite` was not given |
| 130 | Cancelled with Ctrl-C |

In a batch, one bad file does not stop the others; the run ends with a summary and exits
with the first failure's own code so a script can still branch on it.

Nothing is ever overwritten without `--overwrite`, including an in-place
`metadata --set-title`. Ctrl-C cancels between steps rather than killing the process
mid-write; pressing it twice quits immediately.

## Protecting a document

**Protect** in the toolbar, or `ypdf-cli encrypt`, writes a protected copy with AES-256
(or AES-128 for older readers). Two passwords, and they do different jobs: the **open
password** is needed to read the file at all, and the **owner password** is needed to
change the protection or ignore the flags.

Opening a protected file asks for the password — and says whether none was given or the
one given was refused, which are different problems.

Three things worth being clear about:

- **Permission flags are advisory.** Every conforming reader honours "do not copy"; none
  is obliged to. The open password is what actually protects a document. The interface
  says so wherever the flags appear rather than leaving it to be discovered.
- **Saving a protected document writes an unprotected copy.** Opening it decrypted it;
  a save writes what is in memory. The status bar and the Inspect panel say so while the
  document is open, and the CLI logs it.
- **There is no way to open a document whose password nobody has.** Not a missing
  feature — a deliberate one. Recovering a lost password is password cracking whatever
  the menu calls it.

RC4-encrypted files are read, since plenty exist, but never written: producing a new file
with a cipher broken since 2001 and calling it encryption would be dishonest.

A password given as `--password` is visible to anything that can list processes and lands
in the shell history. `YPDF_PASSWORD` is read when the flag is absent, and is the better
habit in a script.

## OCR

**OCR** in the toolbar, or `ypdf-cli ocr`, reads the words on a scan and makes them
searchable. It needs [Tesseract](https://github.com/tesseract-ocr/tesseract) installed —
it is run as a separate program rather than linked, so there is nothing extra to build.
If it cannot be found, the error lists every path that was searched;
`YPDF_TESSERACT_PATH` overrides the search.

```bash
ypdf-cli ocr scan.pdf -o searchable.pdf --lang eng --dpi 300
ypdf-cli ocr ./scans/*.pdf -o ./searchable/ --lang eng+msa
```

**The scan itself is never changed.** The recognized words go into an invisible layer over
the page: it looks exactly as it did, and the text is there to be searched, selected, and
copied. A wrong text layer costs a bad search hit — replacing the image with recognized
text would cost the page.

- Pages that already have text are left alone unless you pass `--force`. Laying a guess
  over real text gives search two answers for the same words.
- Low-confidence words are dropped. A layer full of guesses makes search worse, not
  better.
- Words the layer font cannot encode — Chinese, Arabic, Devanagari — are reported rather
  than written as mojibake. The text layer covers Latin scripts.
- The page image is written to a temporary file for Tesseract to read, and deleted when
  the run ends.

## Forms

**Form** in the toolbar lists every field with the right editor for its kind; only the
fields you edit are written. From the command line:

```bash
ypdf-cli forms application.pdf                                  # list
ypdf-cli forms application.pdf --fill "name=Ada Lovelace" -o filled.pdf
ypdf-cli forms filled.pdf --export data.fdf                     # or .json, .xfdf
ypdf-cli forms application.pdf --import data.fdf -o filled.pdf  # fill a thousand the same way
ypdf-cli forms filled.pdf --flatten -o final.pdf
```

Filling writes the value **and** rebuilds the appearance stream. A file with the right
value and a stale appearance looks filled in one reader, empty in another, and prints
blank — so both are written, every time.

- A checkbox is ticked using the state its own appearance names, which is not always
  `Yes`.
- **Signature fields are never filled.** A value in one produces a document that claims to
  be signed and is not.
- Read-only fields are listed and left alone.
- A field name that does not exist is reported and makes the command exit non-zero. Being
  told "done" after a typo is how a form gets sent in blank.
- `--flatten` draws the values into the page and removes the form. One-way.

## Annotations

**Annotate** in the toolbar puts a tool in your hand: highlight, underline, strikeout,
note, text box, ink, rectangle, ellipse, line, arrow, or stamp. The drawing tools take the
drag and show the shape as you pull it out; the text tools mark whatever the selection
covered. The panel lists what is on the document, jumps to the page, and removes one mark
or all of them. From the command line:

```bash
ypdf-cli annotate draft.pdf                                          # list
ypdf-cli annotate draft.pdf --highlight "1:72,700,300,720" -o marked.pdf
ypdf-cli annotate draft.pdf --note "1:400,600=check this figure" -o marked.pdf
ypdf-cli annotate draft.pdf --line "2:100,100,300,300" --arrow --colour "#cc0000" -o marked.pdf
ypdf-cli annotate draft.pdf --stamp "1:400,60,560,110=DRAFT" -o marked.pdf
ypdf-cli annotate marked.pdf --remove-author Ada -o clean.pdf
```

Areas are `page:x0,y0,x1,y1` in points measured from the bottom-left corner, which is how a
PDF measures itself. `--colour`, `--opacity`, `--width`, `--author`, and `--comment` apply
to everything the command adds.

- Every mark carries its own **appearance stream**, so it looks the same in every reader
  and prints the way it looks. A rectangle with no appearance is a reader's guess.
- A highlight is drawn in multiply mode. It tints the words instead of painting over them.
- Text a built-in font cannot draw — Chinese, Arabic, Devanagari — stays in the comment,
  where readers show it in full, and the log says the painted copy is incomplete. Nothing
  becomes mojibake.
- **Form fields and links are not annotations here.** They share the same array in the
  file, and `--remove-all` leaves them alone; clearing the array would destroy a form.

## Digital signatures

**Inspect → Signatures** shows what a document is signed with, or from the command line:

```bash
ypdf-cli signatures contract.pdf
ypdf-cli signatures ./inbox/*.pdf --json
ypdf-cli signatures contract.pdf --require-signed   # non-zero if it is not signed
```

A signature that verifies proves exactly one thing: **these bytes have not changed since
someone signed them with the private key belonging to this certificate.** It does not prove
who that someone is. There is no trust store here, and revocation checking (CRL, OCSP)
needs network access this tool does not have — so every report ends with the list of what
it did not establish, and nothing here will ever print a green tick that means "trusted".

Four outcomes, kept separate because they mean different things:

| Verdict | What happened |
|---|---|
| `intact` | the digest matches the bytes, and the signature over it verifies |
| `document altered` | the bytes are not the bytes that were signed |
| `signature does not verify` | the document is as signed; the signature is not the key holder's |
| `not checked` | a real signature using something not implemented here — not a failure |

**What a signature covers is checked as carefully as the cryptography.** Most signature
fraud is not cryptographic. A file can carry a perfectly valid signature over an earlier,
honest revision and a page of something else appended after it; the answer is *intact, and
it does not cover the whole file*, with the byte count. Also reported: a range that does not
start at the beginning of the file, a range reaching past its end, and a `/Contents` hole
bigger than the signature sitting in it — room left over for content nobody signed.

Read and reported: signer name, reason, location, claimed time, certification (DocMDP)
signatures, empty signature fields, multiple signatures, timestamp tokens (reported, not
verified), SHA-1 (verified, and flagged as no longer proving much), and the certificate's
subject, issuer, serial, validity, and key.

Signing — creating signatures rather than checking them — is the next milestone.

## PDF/A

**Inspect → PDF/A** checks the open document, or from the command line:

```bash
ypdf-cli pdfa archive.pdf                  # against the level the file claims
ypdf-cli pdfa archive.pdf --level 2b       # against the level you require
ypdf-cli pdfa ./archive/*.pdf --json       # exits non-zero if any file fails
```

Levels are `1a`, `1b`, `2a`, `2b`, `2u`, `3a`, `3b`, `3u`. A level that has never existed
— `1u` — is refused rather than rounded to the nearest one.

**What a pass means, exactly.** It means every check that ran passed. It does not mean the
file conforms: PDF/A has several hundred requirements and this implements the structural
ones. Every report ends with the list of what it did not look at, and says in as many words
that passing is not a certificate. That list is the point of the feature — a tool that
answers YES without it produces confident archives that are rejected on arrival.

Checked here: PDF/A identification in XMP, and XMP agreeing with `/Info`; encryption; the
file identifier; PDF version against the part; fonts embedded, including the standard
fourteen; `/ToUnicode` for levels a and u; an output intent with its ICC profile, when the
pages actually paint in device colour; JavaScript and actions that leave the document;
attachments, which part 1 forbids, part 2 allows only if they are themselves PDF/A, and
part 3 allows with an `/AFRelationship`; optional content; LZW; streams whose data lives in
another file; `/NeedAppearances`; transparency and `/Interpolate`; tagging and `/Lang` for
level a; annotation types, appearances, flags, and opacity.

Not checked: the contents of ICC profiles, glyph coverage inside font programs,
content-stream operators, structure-tree semantics, XMP schema validity, the full
conformance of an attachment, halftones and transfer functions, and PDF/A-4.

The rules differ by part, and so does the answer: transparency fails part 1 and is a
feature of part 2; an attachment fails part 1 and passes part 3. Reporting a conforming
feature as a fault is the same kind of bug as missing a real one.

**Conversion to PDF/A is not offered.** It would mean embedding fonts the file does not
carry and picking a colour profile on your behalf — changes to the document, made
silently. This reports; it does not fix.

## Bookmarks and links

**Bookmarks** in the toolbar opens the outline: click to jump, and add, rename, or remove
entries. Deleting a heading keeps what was under it. From the command line:

```bash
ypdf-cli bookmarks report.pdf                          # list
ypdf-cli bookmarks report.pdf --add "Results=7" -o out.pdf
ypdf-cli bookmarks report.pdf --json > outline.json    # read out, edit, put back
ypdf-cli bookmarks report.pdf --set outline.json -o out.pdf
```

`links` lists where a document wants to send its reader, and edits that:

```bash
ypdf-cli links inbox.pdf                               # list, as plain text
ypdf-cli links inbox.pdf --remove-external -o safe.pdf # strip anything outward-facing
ypdf-cli links doc.pdf --add "1:100,700,200,720=page:4" -o out.pdf
```

Both commands **read by default** and write only when asked.

A link listing reports what the file says, including a `javascript:` URL or a launch
action — a listing that tidied those away would be reassuring rather than useful. Nothing
here can *create* a launch link: it runs a program on the reader's machine.

## Redaction

**Redact** in the toolbar, or `ypdf-cli redact`, removes content from the document. Not a
black rectangle over the top — the glyphs come out of the content stream and the pixels
come out of the images.

```bash
ypdf-cli redact contract.pdf -o safe.pdf --find "account 12345678"
ypdf-cli redact ./forms/*.pdf -o ./safe/ --find "@example.com" --find "555-0100"
ypdf-cli redact scan.pdf -o safe.pdf --rect "1:100,650,210,662"
```

In the viewer, open the Redact panel, tick **Mark areas**, and drag over what should go.
Marks are translucent red — nothing has happened yet. **Apply** removes the content;
that step cannot be undone, which is why it is separate from marking.

What it does, for every marked area:

- deletes the glyphs inside it from the content stream, replacing each with the kerning
  that advances past it so the surrounding text does not shift;
- overwrites the covered pixels inside the image itself and drops any soft mask;
- drops paths drawn entirely inside it, and deletes annotations that overlap it, since a
  link or comment carries its own text;
- draws an opaque box last. The box is not the redaction — pass `--no-cover` and the
  content is still gone. It covers vector artwork that crosses the boundary and could not
  be split, and it makes the removal visible.

Two limits, reported rather than hidden. Text in a font the tool cannot measure is removed
a whole run at a time, and an image it cannot decode is removed entirely rather than partly
cleared. Both take out **more** than was marked, which is the safe direction, and the
report says which happened.

A `--find` term that matches nothing is an error and writes no file. Being told "done"
when the pattern was wrong is how a document gets sent out believing it was cleaned.

## Watermarking

**Watermark** in the toolbar, or `ypdf-cli watermark`, stamps pages with text or an image.

```bash
ypdf-cli watermark report.pdf -o stamped.pdf --text CONFIDENTIAL
ypdf-cli watermark ./out/*.pdf -o ./stamped/ --text DRAFT --position tiled --opacity 0.1
ypdf-cli watermark letter.pdf -o headed.pdf --image logo.png --position top-right --under
```

Everything it does is additive: one content stream plus the resources it needs. Nothing
the page already draws is touched, so removing a watermark means deleting one object.

- `--under` draws behind the page instead of on top. What a letterhead needs — nothing is
  obscured, but opaque content already on the page hides it.
- `--position tiled` repeats the mark across the page, which is the one placement that
  cannot be cropped off.
- Image watermarks keep their transparency. A logo without its alpha mask would be drawn
  on a black rectangle.
- Text the built-in fonts cannot draw is refused rather than written as mojibake, and the
  command fails instead of writing a copy identical to its input.

## Configuration

Five layers, lowest precedence first: system file, user file, project
`config.toml`, `YPDF_*` environment variables, CLI arguments. A layer overrides
only the keys it sets.

```toml
[engine]
workers = 8
max_memory_mb = 4096

[ocr]
language = "eng+msa"

[logging]
level = "info"
format = "json"
```

`YPDF_LOG` accepts full `tracing` filter syntax (`info,ypdf_render=trace`) and
overrides `logging.level`.

## Conventions

Two rules hold everywhere:

* every long-running operation takes a `CancelToken`;
* every long-running operation reports `Progress`.

And one layering rule: PDF logic lives in an engine crate, never in
`apps/desktop`. The GUI and the CLI are both thin callers, which is what makes
batch processing and automation nearly free.

## License

MIT OR Apache-2.0.
