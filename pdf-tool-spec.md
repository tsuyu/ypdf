# PDF Tool — Complete Product Specification

## 1. Product Overview

A fast, privacy-first PDF toolkit for developers and power users.

### Primary goals

- Local-first PDF processing
- High performance
- Batch processing
- Developer-friendly CLI
- Optional REST API
- Desktop GUI
- Strong PDF diagnostics and security inspection
- Offline-capable processing
- Modular Rust core

---

# 2. Core Features

## 2.1 PDF Viewer

- Open PDF
- Fast page rendering
- Thumbnail sidebar
- Page navigation
- Zoom in/out
- Fit width
- Fit page
- Rotate pages
- Full-screen mode
- Dark mode
- Multi-tab documents
- Text search
- Case-sensitive search
- Whole-word search
- Search result highlighting
- Text selection
- Copy text
- Open external links
- Print PDF

---

## 2.2 PDF Creation

Create PDFs from:

- HTML
- Markdown
- Images
- Plain text
- DOCX
- XLSX
- PPTX

Options:

- Templates
- Custom page sizes
- A4
- A5
- Letter
- Legal
- Portrait
- Landscape
- Custom margins
- Headers
- Footers
- Page numbers
- Watermarks
- Custom fonts

---

# 3. PDF Page Management

## 3.1 Merge

- Merge multiple PDFs
- Drag-and-drop ordering
- Preview pages
- Remove files before merging
- Reorder source documents

## 3.2 Split

Split by:

- Page range
- Individual pages
- Every N pages
- Bookmarks

## 3.3 Page Operations

- Extract pages
- Delete pages
- Duplicate pages
- Reorder pages
- Rotate pages
- Insert pages from another PDF
- Replace pages
- Reverse page order

---

# 4. PDF Compression

## Presets

- Maximum compression
- Balanced
- High quality
- Lossless

## Controls

- Image DPI
- JPEG quality
- PNG optimization
- Image downsampling
- Remove unused objects
- Remove metadata
- Compress streams
- Optimize fonts
- Linearize PDF

## Statistics

Display:

```text
Original: 48.2 MB
Optimized: 12.7 MB
Reduction: 73.7%
```

---

# 5. PDF Conversion

## To PDF

- DOCX → PDF
- XLSX → PDF
- PPTX → PDF
- HTML → PDF
- Markdown → PDF
- Images → PDF
- TXT → PDF

## From PDF

- PDF → Images
- PDF → TXT
- PDF → HTML
- PDF → Markdown
- PDF → CSV where applicable

Conversion quality should be reported rather than guaranteeing perfect layout preservation.

---

# 6. Image Processing

Supported formats:

- JPG
- PNG
- WebP
- TIFF

Features:

- Image → PDF
- PDF → JPG
- PDF → PNG
- Extract embedded images
- Image compression
- DPI control
- Crop
- Rotate
- Resize
- Image ordering
- Multi-image PDF creation

---

# 7. OCR

## Features

- OCR scanned PDFs
- OCR selected pages
- OCR entire document
- Automatic language detection
- Searchable PDF generation
- Extract recognized text
- Preserve original visual appearance
- OCR confidence scores
- OCR bounding boxes

## Language Packs

Initial support:

- English
- Malay
- Arabic
- Chinese
- Japanese
- Korean

Language packs should be configurable and installable separately.

---

# 8. PDF Security

## Password Protection

- Open password
- Owner password

## Permissions

- Printing
- Copying
- Editing
- Form filling
- Annotation

## Encryption

- AES-128
- AES-256

## Security Information

Display:

```text
Encryption: AES-256
Password protected: YES
Printing: Allowed
Copying: Not allowed
Editing: Not allowed
```

---

# 9. Digital Signatures

- Draw signature
- Import signature image
- Certificate-based signing
- Certificate management
- Signature verification
- Certificate information
- Signature validity
- Signing timestamp
- Multiple signatures
- PAdES support
- CMS/PKCS#7 support

---

# 10. PDF Annotation

Supported annotations:

- Highlight
- Underline
- Strike-through
- Sticky notes
- Text boxes
- Freehand drawing
- Rectangle
- Circle
- Arrow
- Line
- Stamp

Controls:

- Color
- Opacity
- Line width
- Font
- Font size

---

# 11. Redaction

Redaction must permanently remove the underlying content.

Features:

- Text redaction
- Image redaction
- Region redaction
- Search-and-redact
- Apply redaction
- Remove underlying objects

Example targets:

- IC numbers
- Email addresses
- Phone numbers
- Addresses
- API keys
- Confidential text

Do not implement redaction as only placing a black rectangle over existing content.

---

# 12. Watermark

## Text Watermark

Examples:

- CONFIDENTIAL
- DRAFT
- COPY
- INTERNAL

Controls:

- Position
- Rotation
- Opacity
- Scale
- Font
- Color
- Selected pages
- All pages

## Image Watermark

- Logo
- Signature
- Custom image

---

# 13. Metadata

## Read

- Title
- Author
- Subject
- Keywords
- Creator
- Producer
- Creation date
- Modification date
- PDF version
- Page count
- File size

## Edit

- Title
- Author
- Subject
- Keywords

## Advanced

- XMP metadata viewer
- XMP metadata editor

---

# 14. PDF Diagnostics

Display:

```text
PDF Version       1.7
Pages             124
File Size         42.8 MB
Fonts             17
Images            83
Annotations       14
Forms             3
Embedded Files    2
Encryption        AES-256
Linearized        Yes
PDF/A             No
```

Detect:

- Broken objects
- Missing fonts
- Invalid xref
- Corrupted streams
- Unsupported features
- Duplicate objects
- Oversized images
- Embedded files
- JavaScript
- External links
- Forms
- Annotations

---

# 15. PDF/A

Support validation for:

- PDF/A-1
- PDF/A-2
- PDF/A-3

Display:

```text
PDF/A compliant: YES
Standard: PDF/A-2b
```

---

# 16. PDF Forms

Support:

- AcroForm detection
- Text fields
- Checkboxes
- Radio buttons
- Dropdowns
- Lists
- Signature fields

Operations:

- Fill forms
- Clear forms
- Flatten forms
- Extract form data
- Export form data
- Import form data

Formats:

- FDF
- XFDF
- JSON

---

# 17. Bookmarks / Outline

- View bookmarks
- Create bookmark
- Delete bookmark
- Rename bookmark
- Reorder bookmark
- Set destination
- Generate bookmarks from headings

---

# 18. Links

Detect:

- HTTP/HTTPS links
- Email links
- Internal page links
- External document links

Operations:

- Create link
- Delete link
- Change destination

---

# 19. Embedded Attachments

- List attachments
- Extract attachment
- Add attachment
- Remove attachment
- Display MIME type
- Display file size
- Detect potentially dangerous file types

---

# 20. Embedded Content and JavaScript

Security inspection should detect:

- JavaScript
- Launch actions
- Embedded files
- Auto actions
- External URLs
- Suspicious objects

Example:

```text
JavaScript              FOUND
Embedded files          2
External URLs           4
Launch actions          FOUND
Suspicious objects      3
Encryption              AES-256
```

---

# 21. Batch Processing

Batch processing is a first-class feature.

Example:

```bash
pdf-tool compress ./input/*.pdf --output ./compressed/
```

Supported batch operations:

- Merge
- Split
- Extract
- OCR
- Convert
- Watermark
- Encrypt
- Optimize
- Metadata processing
- Security scanning

Example result:

```text
1,248 PDFs
Processed: 1,248
Failed: 3
Total: 14.2 GB → 5.8 GB
```

---

# 22. CLI

The CLI should be scriptable and automation-friendly.

## Examples

```bash
pdf-tool info document.pdf
```

```bash
pdf-tool merge a.pdf b.pdf -o merged.pdf
```

```bash
pdf-tool split document.pdf --pages 1-20 -o output/
```

```bash
pdf-tool compress document.pdf -o compressed.pdf
```

```bash
pdf-tool ocr scan.pdf --lang eng+msa
```

```bash
pdf-tool images document.pdf -o images/
```

```bash
pdf-tool metadata document.pdf
```

```bash
pdf-tool security-scan document.pdf
```

## CLI Requirements

- Machine-readable JSON output
- Human-readable output
- Exit codes
- Verbose mode
- Quiet mode
- Progress output
- Error reporting
- Cancellation
- Configuration file support

---

# 23. Developer REST API

Optional API service.

## Endpoints

```http
POST /api/v1/pdf/merge
POST /api/v1/pdf/split
POST /api/v1/pdf/compress
POST /api/v1/pdf/ocr
POST /api/v1/pdf/convert
POST /api/v1/pdf/watermark
POST /api/v1/pdf/sign
POST /api/v1/pdf/redact
POST /api/v1/pdf/security-scan

GET /api/v1/pdf/{id}/info
GET /api/v1/jobs/{id}
DELETE /api/v1/jobs/{id}
```

## Async Jobs

For expensive operations:

```json
{
  "job_id": "abc123",
  "status": "processing"
}
```

Query:

```http
GET /api/v1/jobs/abc123
```

---

# 24. Job System

States:

```text
Queued
   ↓
Processing
   ↓
Completed
```

Failure:

```text
Queued
   ↓
Processing
   ↓
Failed
```

Features:

- Progress percentage
- ETA
- Cancellation
- Retry
- Error logs
- Job history
- Concurrent processing
- Resource limits
- Maximum file size
- Maximum processing time

---

# 25. Performance Architecture

The core should be written in Rust.

Architecture:

```text
                 ┌──────────────────────┐
                 │         UI           │
                 │ Desktop / Web / TUI  │
                 └──────────┬───────────┘
                            │
                 ┌──────────▼───────────┐
                 │      PDF Engine      │
                 │        Rust          │
                 ├──────────────────────┤
                 │ Parser               │
                 │ Renderer             │
                 │ Modifier             │
                 │ OCR                  │
                 │ Compressor           │
                 │ Security Scanner     │
                 │ Signature Engine     │
                 │ Conversion           │
                 └──────────┬───────────┘
                            │
                 ┌──────────▼───────────┐
                 │       Storage        │
                 │ Files / Cache / Jobs │
                 └──────────────────────┘
```

## Performance requirements

- Parallel page processing where safe
- Streaming processing where possible
- Memory-mapped files for large documents where appropriate
- Low-copy data paths
- Worker pool
- Cancellation support
- Progress reporting
- Memory limits
- CPU limits
- Large-file support

Do not load an entire multi-gigabyte PDF into memory unnecessarily.

---

# 26. Security Scanner

Security scanning is a major differentiating feature.

## Scan

- JavaScript
- Launch actions
- Embedded executables
- Suspicious URLs
- Auto actions
- Attachments
- Encryption
- Malformed objects
- Suspicious PDF structures

## Example

```text
PDF SECURITY SCAN

JavaScript              ⚠ FOUND
Embedded files          ⚠ 2
External URLs           ℹ 8
Launch actions          ⚠ FOUND
Encrypted               ✓
Digital signature       ✓
Suspicious objects      ⚠ 3
```

The scanner should clearly distinguish:

- Informational
- Warning
- High risk
- Critical

---

# 27. Privacy

Local-first design:

- No upload required
- Offline processing
- Local OCR
- Local conversion
- Local temporary files
- Configurable telemetry
- No telemetry by default
- Secure temporary-file handling
- Optional secure deletion

---

# 28. Developer UX

## Command Palette

```text
> Merge PDFs
> Compress PDF
> OCR PDF
> Extract images
> Inspect PDF
> Security scan
> Edit metadata
> Split PDF
> Redact PDF
```

## Other UX

- Keyboard shortcuts
- Drag and drop
- Recent files
- Processing history
- Saved presets
- Context menus
- File preview
- Operation preview
- Undo where technically possible

---

# 29. Presets

Built-in presets:

```text
Web PDF
Archive PDF
Email PDF
Print PDF
OCR Document
Maximum Compression
Company Standard
```

Users can create custom presets.

Example configuration:

```toml
[preset]
name = "Company Standard"

[compression]
dpi = 150
quality = 80

[metadata]
remove = false

[security]
scan = true
```

---

# 30. Workspace / Project

Support repeatable PDF workflows.

Example:

```text
project/
├── input/
├── output/
├── templates/
├── scripts/
└── config.toml
```

Run:

```bash
pdf-tool run project.toml
```

---

# 31. Logging

Levels:

- ERROR
- WARN
- INFO
- DEBUG
- TRACE

Features:

- Structured logs
- JSON logs
- Operation ID
- Job ID
- Processing time
- Input/output size
- Error details
- Performance metrics

Example:

```json
{
  "operation": "compress",
  "input_size": 48200000,
  "output_size": 12700000,
  "duration_ms": 1820,
  "pages": 124
}
```

---

# 32. Configuration

Configuration sources:

1. CLI arguments
2. Project configuration
3. User configuration
4. System configuration
5. Environment variables

Example:

```toml
[engine]
workers = 8
max_memory_mb = 4096

[ocr]
language = "eng+msa"

[security]
scan_embedded_files = true

[logging]
level = "info"
format = "json"
```

---

# 33. Plugin Architecture

Future plugin system:

```text
plugins/
├── ocr/
├── converters/
├── exporters/
├── scanners/
└── integrations/
```

Potential plugins:

- OCR engines
- Cloud storage
- Enterprise signing
- Document management systems
- Custom converters
- Custom security rules

Plugins should run with explicit permissions.

---

# 34. Storage and Cache

- Temporary file management
- Processing cache
- Thumbnail cache
- OCR cache
- Conversion cache
- Configurable cache size
- Cache cleanup
- Cache invalidation
- Per-project cache

---

# 35. Error Handling

Errors should be actionable.

Example:

```text
ERROR: PDF cannot be parsed.

Reason:
Invalid cross-reference table.

Suggested action:
Try "Repair PDF" or inspect the file using:

pdf-tool diagnostics document.pdf
```

Requirements:

- Stable error codes
- Human-readable messages
- JSON errors for API/CLI
- Detailed debug information
- No sensitive data in logs by default

---

# 36. Testing

## Unit Tests

- PDF parser
- Metadata
- Page operations
- Compression
- Encryption
- OCR integration
- Security scanner

## Integration Tests

- Merge
- Split
- Conversion
- Signing
- Redaction
- Forms

## Fuzz Testing

Important targets:

- PDF parser
- Object parser
- XRef parser
- Stream decoder
- Font parser
- Image parser
- JavaScript detection

## Regression Tests

Maintain a corpus of problematic PDFs.

---

# 37. Compatibility

Target platforms:

- Windows
- Linux
- macOS

Future:

- WebAssembly
- Mobile

PDF versions:

- PDF 1.0+
- PDF 1.7
- PDF 2.0 where supported

---

# 38. MVP Roadmap

## Phase 1 — Core

- [ ] PDF parser
- [ ] PDF viewer
- [ ] PDF metadata
- [ ] Page extraction
- [ ] Page deletion
- [ ] Page reordering
- [ ] Merge
- [ ] Split

## Phase 2 — Optimization

- [ ] Compression
- [ ] Image extraction
- [ ] PDF → image
- [ ] Image → PDF
- [ ] PDF diagnostics
- [ ] Linearization

## Phase 3 — OCR & Security

- [ ] OCR
- [ ] Searchable PDF
- [ ] Security scanner
- [ ] Encryption
- [ ] Password protection
- [ ] Embedded-file inspection

## Phase 4 — Professional

- [ ] Digital signatures
- [ ] Redaction
- [ ] Forms
- [ ] PDF/A
- [ ] Advanced annotations
- [ ] Bookmarks
- [ ] Links

## Phase 5 — Developer Platform

- [ ] CLI
- [ ] Batch processing
- [ ] REST API
- [ ] Async jobs
- [ ] JSON output
- [ ] Project workflows
- [ ] Plugin system

---

# 39. Recommended Technology Stack

## Core

- Rust
- Tokio where asynchronous processing is needed
- Rayon for safe CPU parallelism where appropriate

## CLI

- Rust CLI
- clap
- structured logging

## API

- Axum
- Tokio
- JSON
- Multipart upload
- Async job system

## Desktop

Possible options:

- Tauri
- Native Rust UI
- Web UI with Rust backend

## Storage

For a local desktop application:

- Filesystem
- SQLite for metadata/jobs/history

For server mode:

- SQLite for simple deployments
- PostgreSQL for multi-user deployments
- Filesystem or object storage for documents

---

# 40. Suggested Repository Structure

```text
pdf-tool/
├── crates/
│   ├── pdf-core/
│   ├── pdf-parser/
│   ├── pdf-renderer/
│   ├── pdf-editor/
│   ├── pdf-compressor/
│   ├── pdf-ocr/
│   ├── pdf-security/
│   ├── pdf-sign/
│   ├── pdf-forms/
│   ├── pdf-converter/
│   ├── pdf-cli/
│   └── pdf-api/
│
├── apps/
│   ├── desktop/
│   └── server/
│
├── plugins/
│
├── tests/
│   ├── fixtures/
│   ├── integration/
│   └── fuzz/
│
├── docs/
│
├── examples/
│
├── Cargo.toml
└── README.md
```

---

# 41. Product Differentiators

The tool should not compete only as another PDF editor.

Primary differentiators:

1. Rust performance
2. Local-first processing
3. Developer-first CLI
4. Batch automation
5. PDF diagnostics
6. PDF security scanner
7. OCR
8. REST API
9. Scriptable workflows
10. Privacy
11. Large-file handling
12. Extensible architecture

---

# 42. Recommended Product Positioning

> **A high-performance, privacy-first PDF toolkit built for developers, automation, and power users.**

Core experience:

```text
Open
   ↓
Inspect
   ↓
Edit
   ↓
Optimize
   ↓
Secure
   ↓
Automate
```

The strongest initial product should focus on:

**PDF manipulation + compression + OCR + diagnostics + security scanning + CLI automation.**

Advanced editing, signatures, forms, and full document-authoring capabilities can follow after the core engine is stable.
