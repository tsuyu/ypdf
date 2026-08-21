#!/usr/bin/env bash
# Download the vendored PDFium binary for this machine.
#
# Fetches the pinned bblanchon/pdfium-binaries release into
# vendor/pdfium/<target-triple>/bin/, where ypdf-render looks for it at runtime.
#
# The non-V8 build is deliberate: yPDF detects JavaScript in a document and must
# never be able to execute it.
set -euo pipefail

TAG="${PDFIUM_TAG:-chromium/8009}"

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)   ASSET=pdfium-linux-x64.tgz  ; TRIPLE=x86_64-unknown-linux-gnu ;;
    Linux-aarch64)  ASSET=pdfium-linux-arm64.tgz; TRIPLE=aarch64-unknown-linux-gnu ;;
    Darwin-x86_64)  ASSET=pdfium-mac-x64.tgz    ; TRIPLE=x86_64-apple-darwin ;;
    Darwin-arm64)   ASSET=pdfium-mac-arm64.tgz  ; TRIPLE=aarch64-apple-darwin ;;
    *) echo "Unsupported platform: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/vendor/pdfium/$TRIPLE"
URL="https://github.com/bblanchon/pdfium-binaries/releases/download/$TAG/$ASSET"
ARCHIVE="$(mktemp -t pdfium.XXXXXX.tgz)"

echo "Downloading $URL"
curl -sSL -o "$ARCHIVE" "$URL"

mkdir -p "$DEST"
tar -xzf "$ARCHIVE" -C "$DEST" lib LICENSE VERSION
rm -f "$ARCHIVE"

# ypdf-render looks in bin/ on every platform; the archives use lib/ on Unix.
mkdir -p "$DEST/bin"
cp "$DEST"/lib/libpdfium.* "$DEST/bin/"

echo "PDFium installed to $DEST"
cat "$DEST/VERSION"
