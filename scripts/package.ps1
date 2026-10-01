<#
.SYNOPSIS
    Build yPDF and lay out a release archive.

.DESCRIPTION
    Produces dist/ypdf-<version>-<target>.zip containing both binaries and
    PDFium in the layout the application looks for at run time:

        ypdf.exe
        ypdf-cli.exe
        vendor/pdfium/<target>/bin/pdfium.dll

    That layout is not decoration. `ypdf-render` searches
    `vendor/pdfium/<target-triple>/bin/` next to the executable before it
    falls back to the system library, so a zip that flattens the DLL beside
    the exe still works, and one that leaves it out opens no documents at all.

    The same script runs locally and in CI, so a release built on a laptop and
    a release built by the tag workflow are assembled the same way.

.PARAMETER Version
    Release version. Defaults to the workspace version in Cargo.toml.

.PARAMETER Target
    Rust target triple. Must match a subdirectory of vendor/pdfium.

.PARAMETER OutDir
    Where the archive is written. Defaults to dist/.

.PARAMETER SkipBuild
    Package whatever was already built. For iterating on packaging.

.PARAMETER DynamicCrt
    Link the Microsoft C runtime dynamically, as cargo does by default.

    The default here is the static CRT instead, which is what makes the release
    run on a machine nobody has prepared. A dynamic build imports
    VCRUNTIME140.dll, which is absent from a fresh Windows Server: the binaries
    fail at start with a missing-DLL box until someone installs the Visual C++
    redistributable. Static linking costs about 100 KB per binary and removes
    that prerequisite. PDFium is unaffected either way — it carries its own
    runtime and imports none.

.EXAMPLE
    ./scripts/package.ps1
    ./scripts/package.ps1 -Version 0.2.0 -OutDir C:\tmp\release
#>

[CmdletBinding()]
param(
    [string]$Version,
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$OutDir = "dist",
    [switch]$SkipBuild,
    [switch]$DynamicCrt
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    if (-not $Version) {
        # The workspace version is the single source of truth; reading it here
        # keeps the archive name from drifting away from what was built.
        $manifest = Get-Content "Cargo.toml" -Raw
        if ($manifest -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"') {
            throw "could not read the workspace version out of Cargo.toml"
        }
        $Version = $Matches[1]
    }

    $dll = Join-Path $root "vendor/pdfium/$Target/bin/pdfium.dll"
    if (-not (Test-Path $dll)) {
        $have = (Get-ChildItem (Join-Path $root "vendor/pdfium") -Directory |
                 Select-Object -ExpandProperty Name) -join ", "
        throw "no vendored PDFium for $Target. vendor/pdfium holds: $have"
    }

    # An explicit --target keeps the CRT flags off build scripts and proc
    # macros, which cannot be built against a static CRT; it also puts the
    # output under target/<triple>/ rather than target/.
    $built = Join-Path $root "target/$Target/release"

    if (-not $SkipBuild) {
        $crt = if ($DynamicCrt) { "dynamic" } else { "static" }
        Write-Host "building $Version for $Target ($crt CRT)" -ForegroundColor Cyan

        $previous = $env:RUSTFLAGS
        if (-not $DynamicCrt) {
            $env:RUSTFLAGS = (@($env:RUSTFLAGS, "-C target-feature=+crt-static") |
                              Where-Object { $_ }) -join " "
        }
        try {
            # --locked so a release never silently picks up a dependency the
            # committed Cargo.lock does not name.
            & cargo build --release --locked --target $Target
            if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
        }
        finally {
            $env:RUSTFLAGS = $previous
        }
    }

    $name = "ypdf-$Version-$Target"
    $stage = Join-Path $OutDir $name
    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
    New-Item -ItemType Directory -Path $stage -Force | Out-Null

    foreach ($exe in @("ypdf.exe", "ypdf-cli.exe")) {
        $from = Join-Path $built $exe
        if (-not (Test-Path $from)) { throw "$exe was not built: $from" }
        Copy-Item $from (Join-Path $stage $exe)
    }

    $vendorRoot = Join-Path $stage "vendor/pdfium/$Target"
    $vendorDir = Join-Path $vendorRoot "bin"
    New-Item -ItemType Directory -Path $vendorDir -Force | Out-Null
    Copy-Item $dll (Join-Path $vendorDir "pdfium.dll")

    # The archive redistributes PDFium, so PDFium's own notices travel with it:
    # its LICENSE, the VERSION that says which build this is, and the
    # third-party texts for the libraries compiled into it.
    $source = Join-Path $root "vendor/pdfium/$Target"
    foreach ($item in @("LICENSE", "VERSION", "licenses")) {
        $from = Join-Path $source $item
        if (Test-Path $from) {
            Copy-Item $from (Join-Path $vendorRoot $item) -Recurse
        }
        else {
            Write-Warning "vendor/pdfium/$Target/$item is missing; re-run scripts/fetch-pdfium.ps1"
        }
    }

    $docs = @("README.md", "LICENSE", "LICENSE-MIT", "LICENSE-APACHE")
    $copied = @()
    foreach ($doc in $docs) {
        $path = Join-Path $root $doc
        if (Test-Path $path) {
            Copy-Item $path (Join-Path $stage $doc)
            $copied += $doc
        }
    }
    # Dual-licensed projects have no plain LICENSE, so what matters is that the
    # archive carries at least one licence, not any particular file name.
    if (-not ($copied | Where-Object { $_ -like "LICENSE*" })) {
        Write-Warning "no licence file found, though Cargo.toml says $(
            (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^license\s*=' |
             Select-Object -First 1).Line.Trim())"
    }

    $zip = Join-Path $OutDir "$name.zip"
    if (Test-Path $zip) { Remove-Item $zip -Force }
    Compress-Archive -Path $stage -DestinationPath $zip -CompressionLevel Optimal

    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
    $sums = Join-Path $OutDir "$name.zip.sha256"
    "$hash  $name.zip" | Set-Content $sums -Encoding ascii -NoNewline

    $size = "{0:N1} MB" -f ((Get-Item $zip).Length / 1MB)
    Write-Host ""
    Write-Host "packaged $zip ($size)" -ForegroundColor Green
    Write-Host "sha256   $hash"
    Write-Host ""
    Write-Host "contents:"
    Get-ChildItem $stage -Recurse -File |
        ForEach-Object { "  " + $_.FullName.Substring($stage.Length + 1) }
}
finally {
    Pop-Location
}
