<#
.SYNOPSIS
    Build the Macros Windows release binary and an (unsigned) MSI locally.

.DESCRIPTION
    Local equivalent of the `windows` job in .forgejo/workflows/release.yml, so
    the WiX packaging path can be tested on a Windows dev box without pushing to
    CI. It does NOT sign the MSI — it only proves `wix build` yields an installer
    from `wix\main.wxs`.

    Steps:
      1. Verify the toolchain (rustc/cargo, dotnet, wix).
      2. Provision the WixUI extension (pinned to the installed wix version).
      3. cargo build --release --bin macros   (skip with -SkipBinaryBuild)
      4. Read [package].version from Cargo.toml and run `wix build`.
      5. Report the produced .msi.

    Run from anywhere; paths are resolved relative to this script.

.PARAMETER SkipBinaryBuild
    Skip `cargo build --release` (use when the release binary is already built).

.EXAMPLE
    pwsh -File scripts\build_win.ps1
.EXAMPLE
    pwsh -File scripts\build_win.ps1 -SkipBinaryBuild
#>
[CmdletBinding()]
param(
    [switch]$SkipBinaryBuild
)

$ErrorActionPreference = 'Stop'

$RepoRoot = Split-Path -Parent $PSScriptRoot   # scripts\ -> repo root
Set-Location $RepoRoot

function Write-Step([string]$msg) {
    Write-Host ""
    Write-Host "==> $msg" -ForegroundColor Cyan
}

# Native commands don't throw on nonzero exit even with $ErrorActionPreference =
# 'Stop'. Wrap them so a failure aborts loudly.
function Invoke-Native {
    param([Parameter(Mandatory)][string]$Exe, [string[]]$Args)
    & $Exe @Args
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed (exit $LASTEXITCODE): $Exe $($Args -join ' ')"
    }
}

# ── 1. Toolchain ─────────────────────────────────────────────────────────────
Write-Step "Verify toolchain"
foreach ($tool in 'rustc', 'cargo', 'dotnet', 'wix') {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "Required tool '$tool' not found on PATH. " +
              "Install Rust (rustup), the .NET SDK, and WiX (`dotnet tool install --global wix --version 4.*`)."
    }
}
rustc --version
cargo --version
dotnet --version
wix --version

# ── 2. Resolve version from Cargo.toml [package] ─────────────────────────────
$inPkg = $false
$version = $null
foreach ($line in Get-Content (Join-Path $RepoRoot 'Cargo.toml')) {
    if ($line -match '^\[') { $inPkg = ($line -match '^\[package\]'); continue }
    if ($inPkg -and $line -match '^\s*version\s*=\s*"([^"]+)"') { $version = $Matches[1]; break }
}
if (-not $version) { throw "Could not read [package].version from Cargo.toml" }
Write-Host "Version: $version"

# ── 3. Provision WixUI extension ─────────────────────────────────────────────
Write-Step "Provision WiX UI extension"
$wixVer = (wix --version).Split('+')[0]
Write-Host "wix tool version: $wixVer"
Invoke-Native wix @('extension', 'add', '-g', "WixToolset.UI.wixext/$wixVer")
wix extension list -g

# ── 4. Release binary ────────────────────────────────────────────────────────
if ($SkipBinaryBuild) {
    Write-Step "Skipping release binary build (-SkipBinaryBuild)"
} else {
    Write-Step "Build release binary"
    Invoke-Native cargo @('build', '--release', '--bin', 'macros')
}

# ── 5. Build MSI ─────────────────────────────────────────────────────────────
Write-Step "Build MSI"
$targetDir = (cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).target_directory
$wixDir = Join-Path $targetDir 'wix'
New-Item -ItemType Directory -Force -Path $wixDir | Out-Null
$binDir = Join-Path $targetDir 'release'
$dest = Join-Path $wixDir "Macros-windows-x86_64-$version.msi"
Invoke-Native wix @('build', 'wix\main.wxs',
    '-ext', 'WixToolset.UI.wixext',
    '-arch', 'x64',
    '-d', "Version=$version",
    '-d', "CargoTargetBinDir=$binDir",
    '-o', $dest)

# ── 6. Verify output ─────────────────────────────────────────────────────────
Write-Step "Verify MSI"
if (-not (Test-Path $dest)) { throw "wix build produced no .msi at $dest" }
$msi = Get-Item $dest
Write-Host ""
Write-Host "Built $($msi.FullName) ($([math]::Round($msi.Length/1MB,1)) MB)" -ForegroundColor Green
Write-Host "(unsigned — for local testing only, do not distribute)"
