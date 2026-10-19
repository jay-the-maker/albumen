# SPDX-License-Identifier: Apache-2.0
# Four user-facing steps from scripts/quickstart.json: install, receive,
# upload, verify. The source file is read and then checked again.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

if ($args -contains '--steps') {
    Get-Content -Raw -Path (Join-Path $PSScriptRoot 'quickstart.json')
    return
}

function Get-Sha256Hex([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -Path $Path).Hash.ToLowerInvariant()
}

function Set-FixedMtime([string]$Path) {
    $item = Get-Item -LiteralPath $Path
    $item.LastWriteTimeUtc = [datetime]::SpecifyKind([datetime]'2020-09-13T12:00:00', 'Utc')
}

function Get-UnixMtime([string]$Path) {
    $item = Get-Item -LiteralPath $Path
    [DateTimeOffset]::new($item.LastWriteTimeUtc).ToUnixTimeSeconds()
}

if (-not $env:USERPROFILE) { throw 'USERPROFILE is not set' }
$SourceDir = Join-Path $env:USERPROFILE 'originals'
$Library = Join-Path $env:USERPROFILE 'albumen-library'
$Source = Join-Path $SourceDir 'IMG_QUICKSTART.jpg'
New-Item -ItemType Directory -Force -Path $SourceDir | Out-Null
$payload = [Text.Encoding]::ASCII.GetBytes("albumen synthetic original. Not a photograph.`n")
[IO.File]::WriteAllBytes($Source, $payload)
Set-FixedMtime $Source
$HashBefore = Get-Sha256Hex $Source
$MtimeBefore = Get-UnixMtime $Source

# step: install
& (Join-Path $PSScriptRoot 'install.ps1')
if ($env:ALBUMEN_HOME) {
    $InstallRoot = $env:ALBUMEN_HOME
} elseif ($env:LOCALAPPDATA) {
    $InstallRoot = Join-Path $env:LOCALAPPDATA 'albumen'
} else {
    $InstallRoot = Join-Path $env:USERPROFILE 'AppData\Local\albumen'
}
$Receive = Join-Path $InstallRoot 'bin\albumen-receive.exe'
if (-not (Test-Path -LiteralPath $Receive)) {
    throw "installer did not place albumen-receive in $InstallRoot\bin"
}

if (-not $env:ALBUMEN_RECEIVE_PASSWORD) {
    $env:ALBUMEN_RECEIVE_PASSWORD = 'a receiver password'
}

# step: receive
$stamp = [guid]::NewGuid().ToString('n')
$Log = Join-Path ([IO.Path]::GetTempPath()) "albumen-quickstart-receive-$stamp.out"
$ErrLog = Join-Path ([IO.Path]::GetTempPath()) "albumen-quickstart-receive-$stamp.err"
$Proc = $null
try {
    $Proc = Start-Process -FilePath $Receive -ArgumentList @(
        '--to', $Library,
        '--user', 'camera',
        '--webdav', '127.0.0.1:8080',
        '--apply'
    ) -RedirectStandardOutput $Log -RedirectStandardError $ErrLog -PassThru -WindowStyle Hidden
    $ready = $false
    foreach ($ignored in 1..40) {
        if ((Test-Path -LiteralPath $Log) -and (Select-String -Path $Log -Pattern 'webdav http://' -Quiet)) {
            $ready = $true
            break
        }
        if ($Proc.HasExited) {
            if (Test-Path -LiteralPath $Log) { Get-Content -Path $Log }
            if (Test-Path -LiteralPath $ErrLog) { Get-Content -Path $ErrLog }
            throw 'receiver exited before it listened'
        }
        Start-Sleep -Milliseconds 250
    }
    if (-not $ready) {
        if (Test-Path -LiteralPath $Log) { Get-Content -Path $Log }
        if (Test-Path -LiteralPath $ErrLog) { Get-Content -Path $ErrLog }
        throw 'receiver did not listen on 127.0.0.1:8080'
    }

    # step: upload
    $Mtime = Get-UnixMtime $Source
    $Headers = Join-Path ([IO.Path]::GetTempPath()) "albumen-quickstart-headers-$stamp.txt"
    $Body = Join-Path ([IO.Path]::GetTempPath()) "albumen-quickstart-body-$stamp.txt"
    & curl.exe -sS -D $Headers -o $Body -T $Source `
        -u "camera:$($env:ALBUMEN_RECEIVE_PASSWORD)" `
        -H 'Content-Type: application/octet-stream' `
        -H "X-Albumen-Mtime: $Mtime" `
        'http://127.0.0.1:8080/IMG_QUICKSTART.jpg'
    if ($LASTEXITCODE -ne 0) { throw "phone PUT failed: $LASTEXITCODE" }

    # step: verify
    $Status = 0
    $Server = ''
    foreach ($line in Get-Content -Path $Headers) {
        if ($line -match '^HTTP/\S+\s+(\d+)') { $Status = [int]$Matches[1] }
        if ($line -match '(?i)^X-Albumen-Sha256:\s*([0-9a-fA-F]+)') {
            $Server = $Matches[1].ToLowerInvariant()
        }
    }
    $Stored = Join-Path $Library 'IMG_QUICKSTART.jpg'
    if (-not (Test-Path -LiteralPath $Stored)) {
        throw 'receiver did not store IMG_QUICKSTART.jpg'
    }
    $Client = Get-Sha256Hex $Source
    $StoredHash = Get-Sha256Hex $Stored
    $MtimeAfter = Get-UnixMtime $Source
    if ($Status -ne 201 -or -not $Server -or $Server -ne $Client -or $StoredHash -ne $Client -or $Client -ne $HashBefore) {
        throw "SHA-256 verification failed`n  status $Status`n  source $Client`n  header $Server`n  stored $StoredHash"
    }
    if ($MtimeAfter -ne $MtimeBefore) {
        throw 'source mtime changed'
    }
    Write-Output "verified: $Client"
    Write-Output 'source unchanged'
} finally {
    if ($Proc -and -not $Proc.HasExited) {
        Stop-Process -Id $Proc.Id -Force
        $Proc.WaitForExit()
    }
}
