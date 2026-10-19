# SPDX-License-Identifier: Apache-2.0
# Install albumen, albumen-mcp, and albumen-receive into a directory you own.
# Downloads a GitHub release archive, or uses ALBUMEN_ARCHIVE for a local one.
# The archive SHA-256 and the SHA256SUMS inside it are required. A cosign
# bundle is checked when cosign is on PATH. A GitHub attestation is checked
# when gh is on PATH and the archive came from github.com. A missing tool is
# reported. A rejected signature is an error. Nothing is copied until those
# checks pass. This script does not elevate.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repo = if ($env:ALBUMEN_REPO) { $env:ALBUMEN_REPO } else { 'jay-the-maker/albumen' }
$ReleaseBase = if ($env:ALBUMEN_RELEASE_BASE) { $env:ALBUMEN_RELEASE_BASE } else { "https://github.com/$Repo/releases" }
$Tag = if ($env:ALBUMEN_RELEASE) { $env:ALBUMEN_RELEASE } else { 'latest' }
$CosignIssuer = 'https://token.actions.githubusercontent.com'
$CosignIdentity = '^https://github.com/jay-the-maker/albumen/\.github/workflows/macos-binaries\.yml@.*'
$Bins = @('albumen.exe', 'albumen-mcp.exe', 'albumen-receive.exe')

function Get-ArtifactName {
    if ($env:ALBUMEN_ARTIFACT) { return $env:ALBUMEN_ARTIFACT }
    if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { return 'albumen-windows-aarch64' }
    'albumen-windows-x86_64'
}

function Get-Sha256Hex([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -Path $Path).Hash.ToLowerInvariant()
}

function Assert-Member([string]$Member) {
    $text = $Member.Trim()
    if ($text.StartsWith('/') -or $text.StartsWith('\') -or $text.Contains('..')) {
        throw "refusing archive member $text"
    }
}

function Read-SumLine([string]$Line) {
    $trimmed = $Line.Trim()
    if ($trimmed -notmatch '^([0-9a-fA-F]{64})\s+\*?(.+)$') {
        throw "checksum line is not SHA-256: $Line"
    }
    @{ Hash = $Matches[1].ToLowerInvariant(); Name = $Matches[2].Trim() }
}

function Assert-ChecksumFile([string]$File, [string]$Sums) {
    $parsed = Read-SumLine (Get-Content -Path $Sums -TotalCount 1)
    $leaf = [IO.Path]::GetFileName($File)
    if ($parsed.Name -ne $leaf) {
        throw "checksum is for $($parsed.Name), not $leaf"
    }
    $actual = Get-Sha256Hex $File
    if ($actual -ne $parsed.Hash) {
        throw "SHA-256 mismatch for $leaf`n  expected $($parsed.Hash)`n  actual   $actual"
    }
}

function Assert-InnerSums([string]$Dir) {
    $seen = @{}
    foreach ($line in Get-Content -Path (Join-Path $Dir 'SHA256SUMS')) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        $parsed = Read-SumLine $line
        Assert-Member $parsed.Name
        $path = Join-Path $Dir $parsed.Name
        if (-not (Test-Path -LiteralPath $path)) {
            throw "archive is missing $($parsed.Name)"
        }
        $actual = Get-Sha256Hex $path
        if ($actual -ne $parsed.Hash) {
            throw "SHA-256 mismatch for $($parsed.Name)`n  expected $($parsed.Hash)`n  actual   $actual"
        }
        $seen[$parsed.Name] = $true
    }
    foreach ($bin in $Bins) {
        if (-not $seen.ContainsKey($bin)) {
            throw "SHA256SUMS does not list $bin"
        }
    }
}

function Get-BundleSubject([string]$BundleName, [string]$Artifact, [string]$Archive, [string]$Extract) {
    $base = [IO.Path]::GetFileNameWithoutExtension($BundleName)
    $prefix = "$Artifact."
    if (-not $base.StartsWith($prefix)) {
        throw "bundle name does not match ${Artifact}: $BundleName"
    }
    $rest = $base.Substring($prefix.Length)
    if ($rest -eq [IO.Path]::GetFileName($Archive)) {
        return $Archive
    }
    Assert-Member $rest
    $subject = Join-Path $Extract $rest
    if (-not (Test-Path -LiteralPath $subject)) {
        throw "bundle subject is not in the archive: $rest"
    }
    $subject
}

function Invoke-BundleCheck([string]$Archive, [string]$Extract, [string]$Artifact) {
    $dir = Split-Path -Parent $Archive
    $bundles = @(Get-ChildItem -Path $dir -Filter "$Artifact.*.bundle" -File -ErrorAction SilentlyContinue)
    if ($bundles.Count -eq 0) { return $false }
    $cosign = Get-Command cosign -ErrorAction SilentlyContinue
    if (-not $cosign) {
        Add-Note 'attestation: bundle present, cosign not on PATH; SHA-256 was checked'
        return $true
    }
    foreach ($bundle in $bundles) {
        $subject = Get-BundleSubject $bundle.Name $Artifact $Archive $Extract
        & cosign verify-blob --bundle $bundle.FullName --certificate-oidc-issuer $CosignIssuer --certificate-identity-regexp $CosignIdentity $subject
        if ($LASTEXITCODE -ne 0) {
            throw "attestation: cosign rejected $($bundle.Name)"
        }
    }
    Add-Note "attestation: cosign verified $($bundles.Count) bundle(s)"
    return $true
}

function Invoke-GhCheck([string]$Archive) {
    $gh = Get-Command gh -ErrorAction SilentlyContinue
    if (-not $gh) { return $false }
    $output = & gh attestation verify $Archive --repo $Repo 2>&1 | Out-String
    if ($LASTEXITCODE -eq 0) {
        Add-Note "attestation: gh verified $([IO.Path]::GetFileName($Archive))"
        return $true
    }
    if ($output -match 'no matching attestations|no attestations|failed to fetch|HTTP 40[13]|authentication|unauthorized|not found') {
        Add-Note 'attestation: gh could not find one; SHA-256 was checked'
        return $true
    }
    throw "attestation: gh rejected $([IO.Path]::GetFileName($Archive))`n$output"
}

function Test-WantGh([bool]$LocalArchive) {
    if ($env:ALBUMEN_ATTEST_WITH_GH -eq '1') { return $true }
    if ($LocalArchive) { return $false }
    if ($ReleaseBase -notlike 'https://github.com/*') { return $false }
    return [bool](Get-Command gh -ErrorAction SilentlyContinue)
}

function Save-Url([string]$Url, [string]$Dest) {
    Invoke-WebRequest -Uri $Url -OutFile $Dest -UseBasicParsing | Out-Null
}

function Try-SaveUrl([string]$Url, [string]$Dest) {
    try {
        Save-Url $Url $Dest
        return $true
    } catch {
        Remove-Item -LiteralPath $Dest -Force -ErrorAction SilentlyContinue
        return $false
    }
}

function Get-InstallDest([string]$Requested) {
    if (-not $env:USERPROFILE) { throw 'USERPROFILE is not set' }
    $home = [IO.Path]::GetFullPath($env:USERPROFILE).TrimEnd('\')
    $dest = $Requested.TrimEnd('\')
    if ($dest.Contains('..')) {
        throw 'refusing a path that climbs out of the home directory'
    }
    if (-not [IO.Path]::IsPathRooted($dest)) {
        $dest = Join-Path $home $dest
    }
    $full = [IO.Path]::GetFullPath($dest).TrimEnd('\')
    $prefix = $home + '\'
    if (-not $full.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to install outside your home directory: $full"
    }
    New-Item -ItemType Directory -Force -Path (Join-Path $full 'bin') | Out-Null
    $full
}

$script:Notes = @()
function Add-Note([string]$Message) {
    $script:Notes += $Message
}

$Artifact = Get-ArtifactName
$Work = Join-Path ([IO.Path]::GetTempPath()) ("albumen-install-" + [guid]::NewGuid().ToString('n'))
New-Item -ItemType Directory -Force -Path $Work | Out-Null
$Extract = Join-Path $Work 'extract'
New-Item -ItemType Directory -Force -Path $Extract | Out-Null
$LocalArchive = [bool]$env:ALBUMEN_ARCHIVE

try {
    if ($LocalArchive) {
        $Archive = [IO.Path]::GetFullPath($env:ALBUMEN_ARCHIVE)
        if ($env:ALBUMEN_ARCHIVE_SHA256) {
            $Sums = [IO.Path]::GetFullPath($env:ALBUMEN_ARCHIVE_SHA256)
        } else {
            $Sums = "$Archive.sha256"
        }
        if (-not (Test-Path -LiteralPath $Archive) -or -not (Test-Path -LiteralPath $Sums)) {
            throw 'ALBUMEN_ARCHIVE needs the archive and a sibling .sha256 file'
        }
    } else {
        if ($Tag -eq 'latest') {
            $Prefix = "$ReleaseBase/latest/download"
        } else {
            $Prefix = "$ReleaseBase/download/$Tag"
        }
        $Archive = Join-Path $Work "$Artifact.tar.gz"
        $Sums = Join-Path $Work "$Artifact.tar.gz.sha256"
        Save-Url "$Prefix/$Artifact.tar.gz" $Archive
        Save-Url "$Prefix/$Artifact.tar.gz.sha256" $Sums
        foreach ($name in ($Bins + @("$Artifact.tar.gz"))) {
            Try-SaveUrl "$Prefix/$Artifact.$name.bundle" (Join-Path $Work "$Artifact.$name.bundle") | Out-Null
        }
    }

    Assert-ChecksumFile $Archive $Sums
    $members = & tar -tzf $Archive
    if ($LASTEXITCODE -ne 0) { throw 'archive listing failed' }
    foreach ($member in $members) {
        if (-not [string]::IsNullOrWhiteSpace($member)) { Assert-Member $member }
    }
    & tar -xzf $Archive -C $Extract
    if ($LASTEXITCODE -ne 0) { throw 'archive extract failed' }
    if (-not (Test-Path -LiteralPath (Join-Path $Extract 'SHA256SUMS'))) {
        throw 'archive has no SHA256SUMS'
    }
    Assert-InnerSums $Extract

    $attested = Invoke-BundleCheck $Archive $Extract $Artifact
    if (Test-WantGh $LocalArchive) {
        if (Invoke-GhCheck $Archive) { $attested = $true }
    }
    if (-not $attested) {
        Add-Note 'attestation: not available; SHA-256 was checked'
    }

    if ($env:ALBUMEN_HOME) {
        $requested = $env:ALBUMEN_HOME
    } elseif ($env:LOCALAPPDATA) {
        $requested = Join-Path $env:LOCALAPPDATA 'albumen'
    } else {
        $requested = Join-Path $env:USERPROFILE 'AppData\Local\albumen'
    }
    $Dest = Get-InstallDest $requested
    foreach ($bin in $Bins) {
        $installedBin = Join-Path $Dest "bin\$bin"
        Copy-Item -LiteralPath (Join-Path $Extract $bin) -Destination $installedBin -Force
        Unblock-File -LiteralPath $installedBin -ErrorAction SilentlyContinue
    }
    Copy-Item -LiteralPath (Join-Path $Extract 'SHA256SUMS') -Destination (Join-Path $Dest 'SHA256SUMS') -Force
    foreach ($note in $script:Notes) { Write-Output $note }
    Write-Output "installed: $(Join-Path $Dest 'bin')"
} finally {
    Remove-Item -LiteralPath $Work -Recurse -Force -ErrorAction SilentlyContinue
}
