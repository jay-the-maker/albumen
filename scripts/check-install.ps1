# SPDX-License-Identifier: Apache-2.0
# Exercise install.ps1 without a release binary. A failed check must not copy.

$ErrorActionPreference = 'Stop'
$Installer = Join-Path $PSScriptRoot 'install.ps1'
$Artifact = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'albumen-windows-aarch64' } else { 'albumen-windows-x86_64' }
$Bins = @('albumen.exe', 'albumen-mcp.exe', 'albumen-receive.exe')
$Work = Join-Path ([IO.Path]::GetTempPath()) ("albumen-check-install-" + [guid]::NewGuid().ToString('n'))
New-Item -ItemType Directory -Force -Path $Work | Out-Null

function Get-Sha256Hex([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -Path $Path).Hash.ToLowerInvariant()
}

function Write-Ascii([string]$Path, [string]$Text) {
    $utf8 = New-Object System.Text.UTF8Encoding $false
    [IO.File]::WriteAllText($Path, $Text, $utf8)
}

function New-Archive([string]$Dest, [string]$Mode) {
    $stage = Join-Path $Work ("stage-" + [guid]::NewGuid().ToString('n'))
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    foreach ($bin in $Bins) {
        Write-Ascii (Join-Path $stage $bin) "albumen-bin $bin`n"
    }
    $lines = @()
    foreach ($bin in $Bins) {
        if ($Mode -eq 'bad-inner' -and $bin -eq $Bins[0]) {
            $hash = '0' * 64
        } else {
            $hash = Get-Sha256Hex (Join-Path $stage $bin)
        }
        $lines += "$hash  $bin"
    }
    Write-Ascii (Join-Path $stage 'SHA256SUMS') (($lines -join "`n") + "`n")
    New-Item -ItemType Directory -Force -Path $Dest | Out-Null
    $archive = Join-Path $Dest "$Artifact.tar.gz"
    Push-Location $stage
    try {
        & tar -czf $archive SHA256SUMS @Bins
        if ($LASTEXITCODE -ne 0) { throw 'tar failed' }
    } finally {
        Pop-Location
    }
    $leaf = [IO.Path]::GetFileName($archive)
    Write-Ascii "$archive.sha256" "$(Get-Sha256Hex $archive)  $leaf`n"
}

function Assert-Failed([scriptblock]$Body, [string]$UserRoot) {
    $failed = $false
    try {
        & $Body
    } catch {
        $failed = $true
    }
    if (-not $failed) { throw 'installer succeeded and should have failed' }
    $bin = Join-Path $UserRoot "AppData\Local\albumen\bin\$($Bins[0])"
    if (Test-Path -LiteralPath $bin) { throw "installer wrote $bin after a failed check" }
}

$savedProfile = $env:USERPROFILE
$savedLocal = $env:LOCALAPPDATA
$savedArchive = $env:ALBUMEN_ARCHIVE
$savedHome = $env:ALBUMEN_HOME
$savedPath = $env:PATH
try {
    $good = Join-Path $Work 'good'
    New-Archive $good 'good'
    $userRoot = Join-Path $Work 'home-good'
    New-Item -ItemType Directory -Force -Path $userRoot | Out-Null
    $env:USERPROFILE = $userRoot
    $env:LOCALAPPDATA = Join-Path $userRoot 'AppData\Local'
    $env:ALBUMEN_ARCHIVE = Join-Path $good "$Artifact.tar.gz"
    $env:ALBUMEN_HOME = $null
    $out = & $Installer | Out-String
    if ($out -notmatch 'attestation: not available; SHA-256 was checked') { throw "missing attestation note`n$out" }
    if ($out -notmatch 'installed:') { throw "missing installed line`n$out" }
    $placed = Join-Path $env:LOCALAPPDATA "albumen\bin\$($Bins[0])"
    $text = [IO.File]::ReadAllText($placed)
    if ($text -notmatch [regex]::Escape("albumen-bin $($Bins[0])")) { throw "installed bytes were not the archive: $text" }

    $bad = Join-Path $Work 'bad'
    New-Archive $bad 'good'
    Write-Ascii (Join-Path $bad "$Artifact.tar.gz.sha256") "$((('0' * 64)))  $Artifact.tar.gz`n"
    $userRoot = Join-Path $Work 'home-bad'
    New-Item -ItemType Directory -Force -Path $userRoot | Out-Null
    Assert-Failed {
        $env:USERPROFILE = $userRoot
        $env:LOCALAPPDATA = Join-Path $userRoot 'AppData\Local'
        $env:ALBUMEN_ARCHIVE = Join-Path $bad "$Artifact.tar.gz"
        & $Installer | Out-Null
    } $userRoot

    $inner = Join-Path $Work 'inner'
    New-Archive $inner 'bad-inner'
    $userRoot = Join-Path $Work 'home-inner'
    New-Item -ItemType Directory -Force -Path $userRoot | Out-Null
    Assert-Failed {
        $env:USERPROFILE = $userRoot
        $env:LOCALAPPDATA = Join-Path $userRoot 'AppData\Local'
        $env:ALBUMEN_ARCHIVE = Join-Path $inner "$Artifact.tar.gz"
        & $Installer | Out-Null
    } $userRoot

    $fake = Join-Path $Work 'fake'
    New-Item -ItemType Directory -Force -Path $fake | Out-Null
    $log = Join-Path $Work 'cosign.log'
    Write-Ascii (Join-Path $fake 'cosign.cmd') "@echo off`r`necho %*>> `"%COSIGN_LOG%`"`r`nexit /b %COSIGN_RC%`r`n"
    $cosignDir = Join-Path $Work 'cosign-reject'
    New-Archive $cosignDir 'good'
    Write-Ascii (Join-Path $cosignDir "$Artifact.$($Bins[0]).bundle") ''
    $userRoot = Join-Path $Work 'home-cosign'
    New-Item -ItemType Directory -Force -Path $userRoot | Out-Null
    $env:PATH = "$fake;$savedPath"
    $env:COSIGN_LOG = $log
    $env:COSIGN_RC = '1'
    Assert-Failed {
        $env:USERPROFILE = $userRoot
        $env:LOCALAPPDATA = Join-Path $userRoot 'AppData\Local'
        $env:ALBUMEN_ARCHIVE = Join-Path $cosignDir "$Artifact.tar.gz"
        & $Installer | Out-Null
    } $userRoot
    $logged = [IO.File]::ReadAllText($log)
    if ($logged -notmatch 'verify-blob') { throw "cosign was not invoked: $logged" }

    $userRoot = Join-Path $Work 'home-outside'
    New-Item -ItemType Directory -Force -Path $userRoot | Out-Null
    $outside = Join-Path $Work 'outside-dest'
    Assert-Failed {
        $env:USERPROFILE = $userRoot
        $env:LOCALAPPDATA = Join-Path $userRoot 'AppData\Local'
        $env:ALBUMEN_HOME = $outside
        $env:ALBUMEN_ARCHIVE = Join-Path $good "$Artifact.tar.gz"
        & $Installer | Out-Null
    } $userRoot
    if (Test-Path -LiteralPath $outside) { throw 'installer created a directory outside the home directory' }

    Write-Output 'install.ps1 checks: ok'
} finally {
    $env:USERPROFILE = $savedProfile
    $env:LOCALAPPDATA = $savedLocal
    $env:ALBUMEN_ARCHIVE = $savedArchive
    $env:ALBUMEN_HOME = $savedHome
    $env:PATH = $savedPath
    Remove-Item -LiteralPath $Work -Recurse -Force -ErrorAction SilentlyContinue
}
