<#
.SYNOPSIS
Download the vendored PDFium binary for this machine.

.DESCRIPTION
Fetches the pinned bblanchon/pdfium-binaries release into
vendor/pdfium/<target-triple>/bin/, where ypdf-render looks for it at runtime.

The non-V8 build is deliberate: yPDF detects JavaScript in a document and must
never be able to execute it.
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
$archive = Join-Path ([System.IO.Path]::GetTempPath()) $Asset

Write-Host "Downloading $url"
Invoke-WebRequest -Uri $url -OutFile $archive

New-Item -ItemType Directory -Force -Path $dest | Out-Null
tar -xzf $archive -C $dest bin LICENSE VERSION
Remove-Item $archive -Force

Write-Host "PDFium installed to $dest"
Get-Content (Join-Path $dest 'VERSION')
