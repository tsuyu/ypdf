#!/usr/bin/env bash
# Download the vendored PDFium binary for this machine.
#
# Fetches the pinned bblanchon/pdfium-binaries release into
# vendor/pdfium/<target-triple>/bin/, where ypdf-render looks for it at runtime.
#
# The archive is checked against scripts/pdfium.sha256 before anything is
# unpacked. This binary is linked into every yPDF build and parses untrusted
# files for a living, so a download that does not match the pin is thrown away
# rather than extracted and inspected afterwards.
#
# The non-V8 build is deliberate: yPDF detects JavaScript in a document and must
# never be able to execute it.
#
# PDFIUM_TAG overrides the release; doing that means replacing
# scripts/pdfium.sha256 as well, since the checksum file names the tag it
# belongs to.
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
SUMS="$ROOT/scripts/pdfium.sha256"

# Read the expected digest before downloading, so an unlisted asset fails at
# once rather than after several megabytes.
if [ ! -f "$SUMS" ]; then
    echo "no checksum file at $SUMS; refusing to fetch an unverified binary" >&2
    exit 1
fi
EXPECTED="$(awk -v asset="$ASSET" '$1 !~ /^#/ && $2 == asset { print $1; exit }' "$SUMS")"
if [ -z "$EXPECTED" ]; then
    echo "$ASSET is not listed in $SUMS; add its digest before fetching it" >&2
    exit 1
fi

ARCHIVE="$(mktemp -t pdfium.XXXXXX.tgz)"
trap 'rm -f "$ARCHIVE"' EXIT

echo "Downloading $URL"
curl -sSL -o "$ARCHIVE" "$URL"

# sha256sum on Linux, shasum on macOS; neither is present on both.
if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL="$(sha256sum "$ARCHIVE" | cut -d' ' -f1)"
elif command -v shasum >/dev/null 2>&1; then
    ACTUAL="$(shasum -a 256 "$ARCHIVE" | cut -d' ' -f1)"
else
    echo "no sha256sum or shasum available; cannot verify the download" >&2
    exit 1
fi

if [ "$ACTUAL" != "$EXPECTED" ]; then
    cat >&2 <<EOF
PDFium checksum mismatch for $ASSET
  expected $EXPECTED
  got      $ACTUAL
The download was deleted. Either the pin in scripts/pdfium.sha256 is stale, or
this is not the binary the release recorded. Do not extract it by hand.
EOF
    exit 1
fi
echo "sha256 $ACTUAL (matches the pin)"

mkdir -p "$DEST"
# licenses/ comes too: the release archives redistribute this binary, and
# PDFium and the libraries inside it ask for their notices to travel with it.
tar -xzf "$ARCHIVE" -C "$DEST" lib LICENSE VERSION licenses

# ypdf-render looks in bin/ on every platform; the archives use lib/ on Unix.
mkdir -p "$DEST/bin"
cp "$DEST"/lib/libpdfium.* "$DEST/bin/"

echo "PDFium installed to $DEST"
cat "$DEST/VERSION"
