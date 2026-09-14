<#
.SYNOPSIS
    Puts an hrc executable where the Herdr plugin manifest expects one.

.DESCRIPTION
    The Windows half of herdr/build.sh. Herdr runs this once, at
    `herdr plugin install` time, with the plugin directory as the working
    directory and before it registers the plugin. A failure halts the
    installation, which is what we want: a plugin registered without its
    binary fails later and less clearly.

    Two outcomes, in order of preference:

      1. A published release artifact, downloaded and checksum-verified by
         scripts\install.ps1. No Rust toolchain required.
      2. cargo build --release, for a checkout newer than the last tag and
         for the period before the first tag exists at all.

    Either way the binary ends up in two places, because two different things
    look for it: bin\hrc.exe under the plugin root, which is what the
    manifest invokes and which does not depend on PATH; and the install
    prefix, so that `hrc send`, `hrc doctor`, and the agent skill's
    `hrc --help` discovery work in a terminal. The second copy is a
    convenience and its failure is not fatal.
#>
[CmdletBinding()]
param(
    [string]$Version = $env:HRC_VERSION,
    [string]$Prefix = "$env:LOCALAPPDATA\Programs\hrc"
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$pluginRoot = Split-Path -Parent $PSScriptRoot
Set-Location $pluginRoot

function Say([string]$message) { Write-Host "herdr/build.ps1: $message" }

function Install-ToPluginRoot([string]$source) {
    $binDir = Join-Path $pluginRoot 'bin'
    New-Item -ItemType Directory -Force -Path $binDir | Out-Null
    # Staged then moved, so a half-copied file is never left where the
    # manifest points. Herdr may start the plugin as soon as this returns.
    $staged = Join-Path $binDir ".hrc.$PID.exe"
    Copy-Item -Force $source $staged
    Move-Item -Force $staged (Join-Path $binDir 'hrc.exe')
    Say "installed $binDir\hrc.exe"
}

function Install-ToPrefix([string]$source) {
    try {
        New-Item -ItemType Directory -Force -Path $Prefix | Out-Null
        Copy-Item -Force $source (Join-Path $Prefix 'hrc.exe')
        Say "installed $Prefix\hrc.exe"
    } catch {
        Say "could not write $Prefix\hrc.exe; the plugin still works, but the CLI is not on PATH"
    }
}

# The tag to try, if the caller did not name one. git describe is the only
# thing here that knows about releases; a checkout with no tags simply has no
# answer and falls through to building.
if (-not $Version -and (Get-Command git -ErrorAction SilentlyContinue)) {
    $Version = (& git describe --tags --abbrev=0 2>$null)
}

if ($Version) {
    Say "trying published release $Version"
    # Installed to a scratch prefix first: scripts\install.ps1 verifies a
    # checksum and refuses a mismatch, and letting it write straight into
    # bin\ would mean a failed verification had already replaced a working
    # binary.
    $staging = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
    try {
        & "$pluginRoot\scripts\install.ps1" -Version $Version -Prefix $staging
        Install-ToPluginRoot (Join-Path $staging 'hrc.exe')
        Install-ToPrefix (Join-Path $staging 'hrc.exe')
        Remove-Item -Recurse -Force $staging -ErrorAction SilentlyContinue
        exit 0
    } catch {
        Remove-Item -Recurse -Force $staging -ErrorAction SilentlyContinue
        Say 'no usable release artifact for this platform; building from source'
    }
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error @"
no published release covers this platform and cargo is not installed.
Install a Rust toolchain from https://rustup.rs and retry, or install a
prebuilt hrc yourself and copy it to $pluginRoot\bin\hrc.exe.
"@
    exit 1
}

Say 'building from source (this takes a few minutes the first time)'
& cargo build --release --locked --bin hrc
if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }

$built = Join-Path $pluginRoot 'target\release\hrc.exe'
Install-ToPluginRoot $built
Install-ToPrefix $built

Say 'done'
