<#
.SYNOPSIS
Download the vendored PDFium binary for this machine.

.DESCRIPTION
Fetches the pinned bblanchon/pdfium-binaries release into
vendor/pdfium/<target-triple>/bin/, where ypdf-render looks for it at runtime.

The archive is checked against scripts/pdfium.sha256 before anything is
unpacked. This binary is linked into every yPDF build and parses untrusted
files for a living, so a download that does not match the pin is thrown away
rather than extracted and inspected afterwards.

The non-V8 build is deliberate: yPDF detects JavaScript in a document and must
never be able to execute it.

.PARAMETER Tag
Release tag to fetch. Changing it means replacing scripts/pdfium.sha256 too;
the checksum file names the tag it belongs to.
#>
[CmdletBinding()]
param(
    [string]$Tag = 'chromium/8009',
    [string]$Triple = 'x86_64-pc-windows-msvc',
    [string]$Asset = 'pdfium-win-x64.tgz'
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "vendor/pdfium/$Triple"
$url = "https://github.com/bblanchon/pdfium-binaries/releases/download/$Tag/$Asset"
$sums = Join-Path $PSScriptRoot 'pdfium.sha256'

# The expected digest, read before the download so a missing or unlisted entry
# fails immediately rather than after fetching several megabytes.
if (-not (Test-Path $sums)) {
    throw "no checksum file at $sums; refusing to fetch an unverified binary"
}
$expected = $null
foreach ($line in Get-Content $sums) {
    $line = $line.Trim()
    if (-not $line -or $line.StartsWith('#')) { continue }
    $fields = $line -split '\s+', 2
    if ($fields.Count -eq 2 -and $fields[1].Trim() -eq $Asset) {
        $expected = $fields[0].ToLower()
        break
    }
}
if (-not $expected) {
    throw "$Asset is not listed in $sums; add its digest before fetching it"
}

$archive = Join-Path ([System.IO.Path]::GetTempPath()) $Asset

Write-Host "Downloading $url"
# The progress bar makes Invoke-WebRequest an order of magnitude slower on
# Windows PowerShell and adds nothing to a script's output.
$progress = $ProgressPreference
$ProgressPreference = 'SilentlyContinue'
try {
    Invoke-WebRequest -Uri $url -OutFile $archive
}
finally {
    $ProgressPreference = $progress
}

$actual = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLower()
if ($actual -ne $expected) {
    Remove-Item $archive -Force -ErrorAction SilentlyContinue
    throw @"
PDFium checksum mismatch for $Asset
  expected $expected
  got      $actual
The download was deleted. Either the pin in scripts/pdfium.sha256 is stale, or
this is not the binary the release recorded. Do not extract it by hand.
"@
}
Write-Host "sha256 $actual (matches the pin)"

New-Item -ItemType Directory -Force -Path $dest | Out-Null
# licenses/ comes too: the release archives redistribute this binary, and
# PDFium and the libraries inside it ask for their notices to travel with it.
tar -xzf $archive -C $dest bin LICENSE VERSION licenses
Remove-Item $archive -Force

Write-Host "PDFium installed to $dest"
Get-Content (Join-Path $dest 'VERSION')
