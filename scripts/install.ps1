<#
.SYNOPSIS
    Installs a prebuilt hrc executable on Windows.

.DESCRIPTION
    The Windows half of PRD requirements HRC-TECH-011 and HRC-TECH-012.
    Installing must need no Rust toolchain and no .NET SDK, Node.js, or
    Python runtime, so this uses only what Windows PowerShell already
    provides: Invoke-WebRequest, Get-FileHash, and Expand-Archive.

    It refuses to install an artifact whose checksum does not match the
    published one. That is the point of publishing checksums; a verification
    that can be skipped by accident is not one.

.PARAMETER Version
    The release tag to install, for example v0.1.0.

.PARAMETER Prefix
    Directory to install into. Defaults to %LOCALAPPDATA%\Programs\hrc.

.PARAMETER BaseUrl
    Where to fetch the artifact from. Defaults to the GitHub release. A local
    directory may be given so the release fixture can exercise this script
    rather than a copy of its logic.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$Prefix = "$env:LOCALAPPDATA\Programs\hrc",
    [string]$BaseUrl = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = 'czinegeroland/herdr-remote-channel'
$target = 'x86_64-pc-windows-msvc'

if (-not [System.Environment]::Is64BitOperatingSystem) {
    throw 'no prebuilt binary for 32-bit Windows; see the release page'
}

$archive = "hrc-$Version-$target.tar.gz"
if ([string]::IsNullOrWhiteSpace($BaseUrl)) {
    $BaseUrl = "https://github.com/$repo/releases/download/$Version"
}

$work = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $work -Force | Out-Null

try {
    $archivePath = Join-Path $work $archive
    $sumPath = "$archivePath.sha256"

    function Get-Artifact([string]$Name, [string]$Destination) {
        if ($BaseUrl -match '^(file://|[A-Za-z]:\\|\\\\|/)') {
            $source = Join-Path ($BaseUrl -replace '^file://', '') $Name
            if (-not (Test-Path -LiteralPath $source)) { throw "no such file: $source" }
            Copy-Item -LiteralPath $source -Destination $Destination
        } else {
            Invoke-WebRequest -Uri "$BaseUrl/$Name" -OutFile $Destination -UseBasicParsing
        }
    }

    Write-Host "Downloading $archive"
    Get-Artifact -Name $archive -Destination $archivePath
    Get-Artifact -Name "$archive.sha256" -Destination $sumPath

    # Only the digest is compared. The name beside it may carry a path from
    # wherever the checksum was generated, and a path difference must not be
    # mistaken for a mismatch — nor a mismatch for a path difference.
    $published = ((Get-Content -LiteralPath $sumPath -First 1) -split '\s+')[0]
    if ([string]::IsNullOrWhiteSpace($published)) {
        throw 'the published checksum file is empty'
    }

    $actual = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($published.ToLowerInvariant() -ne $actual) {
        throw @"
checksum mismatch for $archive
  published: $published
  actual:    $actual
Refusing to install. The artifact does not match what the release published.
"@
    }
    Write-Host "Checksum verified: $actual"

    tar -xzf $archivePath -C $work
    if ($LASTEXITCODE -ne 0) { throw 'could not unpack the archive' }

    $binary = Join-Path $work "hrc-$Version-$target\hrc.exe"
    if (-not (Test-Path -LiteralPath $binary)) {
        throw 'the archive did not contain an hrc executable'
    }

    New-Item -ItemType Directory -Path $Prefix -Force | Out-Null
    # Written to a staging name and moved into place, so an interrupted
    # install cannot leave a half-copied executable on the path.
    $staged = Join-Path $Prefix ".hrc.$PID.exe"
    Copy-Item -LiteralPath $binary -Destination $staged -Force
    Move-Item -LiteralPath $staged -Destination (Join-Path $Prefix 'hrc.exe') -Force

    Write-Host "Installed $Prefix\hrc.exe"
    & (Join-Path $Prefix 'hrc.exe') --version

    if (-not (($env:PATH -split ';') -contains $Prefix)) {
        Write-Host "Add $Prefix to your PATH to run hrc from anywhere."
    }
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
