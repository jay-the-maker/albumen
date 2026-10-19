# SPDX-License-Identifier: Apache-2.0
# Prove default-config scan, index, and search make no network connection.
#
# Index is the process-local metadata cache. There is no on-disk catalog and
# no index subcommand. The index run is albumen-mcp, with export and upload
# left off, calling search with a metadata filter so ExifTool reads the files.
# The search run is a second default-config process calling search by name.
#
# The observer is pktmon, filtered to albumen.exe, albumen-mcp.exe, and
# perl.exe (ExifTool). A canary must show an external connect, a DNS sendto,
# and a connect from a loopback bind toward an external address before those
# commands are trusted. An outbound firewall block with a failure audit is
# recorded for the same programs. Loopback bypasses Windows Firewall, so the
# loopback proof is pktmon, not the firewall event.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repo = Split-Path -Parent $PSScriptRoot
Set-Location $Repo

function Invoke-Native {
    param([scriptblock]$Command)
    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw "command failed with exit $LASTEXITCODE"
    }
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'cargo is not on PATH'
}
if (-not (Get-Command pktmon -ErrorAction SilentlyContinue)) {
    throw 'pktmon is not on PATH; this Windows image cannot run the packet observer'
}

$albumen = Join-Path $Repo 'target\debug\albumen.exe'
$mcp = Join-Path $Repo 'target\debug\albumen-mcp.exe'
if (-not (Test-Path -LiteralPath $albumen) -or -not (Test-Path -LiteralPath $mcp)) {
    Invoke-Native { cargo build --locked -p albumen -p albumen-mcp }
}

$work = Join-Path ([IO.Path]::GetTempPath()) ("albumen-zero-network-" + [guid]::NewGuid().ToString('n'))
New-Item -ItemType Directory -Force -Path $work | Out-Null
$library = Join-Path $work 'library'
$outDir = Join-Path $work 'out'
$tmp = Join-Path $work 'tmp'
New-Item -ItemType Directory -Force -Path $library, $outDir, $tmp | Out-Null
$source = Join-Path $library 'IMG_0001.jpg'
$payload = [Text.Encoding]::ASCII.GetBytes("albumen synthetic original. Not a photograph.`n")
[IO.File]::WriteAllBytes($source, $payload)
$xmp = @"
<?xpacket begin="" id="albumen-synthetic"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/">
   <dc:description><rdf:Alt><rdf:li xml:lang="x-default">synthetic caption</rdf:li></rdf:Alt></dc:description>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
"@
[IO.File]::WriteAllText((Join-Path $library 'IMG_0001.jpg.xmp'), $xmp)
$hashBefore = (Get-FileHash -Algorithm SHA256 -Path $source).Hash
$mtimeBefore = (Get-Item -LiteralPath $source).LastWriteTimeUtc.Ticks

function New-Frame([object]$Message) {
    $json = $Message | ConvertTo-Json -Compress -Depth 8
    $body = [Text.Encoding]::UTF8.GetBytes($json)
    $header = [Text.Encoding]::ASCII.GetBytes("Content-Length: $($body.Length)`r`n`r`n")
    $frame = New-Object byte[] ($header.Length + $body.Length)
    [Buffer]::BlockCopy($header, 0, $frame, 0, $header.Length)
    [Buffer]::BlockCopy($body, 0, $frame, $header.Length, $body.Length)
    return $frame
}

function Write-Frames([string]$Path, [object[]]$Messages) {
    $bytes = New-Object System.Collections.Generic.List[byte]
    foreach ($message in $Messages) {
        foreach ($byte in (New-Frame $message)) { $bytes.Add($byte) }
    }
    [IO.File]::WriteAllBytes($Path, $bytes.ToArray())
}

$indexIn = Join-Path $work 'index.bin'
$searchIn = Join-Path $work 'search.bin'
Write-Frames $indexIn @(
    @{ jsonrpc = '2.0'; id = 1; method = 'initialize'; params = @{ protocolVersion = '2024-11-05' } },
    @{ jsonrpc = '2.0'; id = 2; method = 'tools/call'; params = @{ name = 'search'; arguments = @{ camera = 'synthetic-index' } } }
)
Write-Frames $searchIn @(
    @{ jsonrpc = '2.0'; id = 1; method = 'initialize'; params = @{ protocolVersion = '2024-11-05' } },
    @{ jsonrpc = '2.0'; id = 2; method = 'tools/call'; params = @{ name = 'search'; arguments = @{ query = 'IMG_0001' } } }
)

foreach ($name in @(
    'ALBUMEN_S3_ENDPOINT', 'ALBUMEN_S3_BUCKET', 'ALBUMEN_S3_ACCESS_KEY', 'ALBUMEN_S3_SECRET_KEY',
    'HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy'
)) {
    Remove-Item -Path "Env:$name" -ErrorAction SilentlyContinue
}
$env:TEMP = $tmp
$env:TMP = $tmp

$etl = Join-Path $work 'capture.etl'
$txt = Join-Path $work 'capture.txt'
$rules = @()
$auditBefore = $null

function Clear-Pktmon {
    # Windows PowerShell treats pktmon's "not running" stderr as a terminating
    # error when ErrorAction is Stop, even with 2>$null.
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & pktmon stop 2>&1 | Out-Null
        & pktmon filter remove 2>&1 | Out-Null
    } finally {
        $ErrorActionPreference = $previous
    }
}

function Start-Pktmon([string[]]$ProcessNames) {
    Clear-Pktmon
    if (Test-Path -LiteralPath $etl) { Remove-Item -LiteralPath $etl -Force }
    if (Test-Path -LiteralPath $txt) { Remove-Item -LiteralPath $txt -Force }
    $index = 0
    foreach ($processName in $ProcessNames) {
        $index += 1
        & pktmon filter add "albumen-$index" --process-name $processName
        if ($LASTEXITCODE -ne 0) {
            $help = & pktmon filter add --help 2>&1 | Out-String
            throw "pktmon filter add --process-name failed for $processName`n$help"
        }
    }
    $startHelp = & pktmon start --help 2>&1 | Out-String
    $startArgs = @('start', '--capture', '--pkt-size', '128', '--file-name', $etl, '--file-size', '32')
    if ($startHelp -match 'loopback') {
        & pktmon @startArgs --flags 0x10
        if ($LASTEXITCODE -eq 0) { return }
        # A failed start may have left the monitor running. Stopping it writes
        # "not running" to stderr when it did not, and Windows PowerShell turns
        # that stderr into a terminating error.
        $previous = $ErrorActionPreference
        $ErrorActionPreference = 'Continue'
        try {
            & pktmon stop 2>&1 | Out-Null
        } finally {
            $ErrorActionPreference = $previous
        }
    }
    & pktmon @startArgs
    if ($LASTEXITCODE -ne 0) {
        throw "pktmon start failed`n$startHelp"
    }
}

function Stop-PktmonText {
    & pktmon stop | Out-Null
    if (-not (Test-Path -LiteralPath $etl)) {
        throw 'pktmon wrote no capture file'
    }
    & pktmon etl2txt $etl --out $txt
    if ($LASTEXITCODE -ne 0) {
        & pktmon etl2txt $etl -o $txt
    }
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $txt)) {
        $help = & pktmon etl2txt --help 2>&1 | Out-String
        throw "pktmon etl2txt failed`n$help"
    }
    return [IO.File]::ReadAllText($txt)
}

function Assert-Canary([string]$Text) {
    foreach ($needle in @('203.0.113.5', '127.0.0.1')) {
        if ($Text -notlike "*$needle*") {
            $comp = & pktmon comp list 2>&1 | Out-String
            $help = & pktmon start --help 2>&1 | Out-String
            throw @"
pktmon did not record $needle from the canary.
This is the connection attempt the observer missed.
pktmon comp list:
$comp
pktmon start --help:
$help
"@
        }
    }
    if ($Text -notmatch ':53\b' -and $Text -notmatch '\.53\b') {
        throw "pktmon did not record a DNS port 53 datagram from the canary`n$Text"
    }
    Write-Output 'zero-network: pktmon saw the external connect, the DNS send, and loopback'
}

function Invoke-Canary {
    $udp = New-Object System.Net.Sockets.UdpClient
    try {
        [void]$udp.Send([byte[]](0), 1, '127.0.0.1', 53)
        [void]$udp.Send([byte[]](0), 1, '203.0.113.5', 53)
    } finally {
        $udp.Close()
    }
    $external = New-Object System.Net.Sockets.TcpClient
    try {
        $task = $external.ConnectAsync('203.0.113.5', 9)
        [void]$task.Wait(300)
    } catch {
    } finally {
        $external.Close()
    }
    $bound = New-Object System.Net.Sockets.Socket(
        [Net.Sockets.AddressFamily]::InterNetwork,
        [Net.Sockets.SocketType]::Stream,
        [Net.Sockets.ProtocolType]::Tcp)
    try {
        $bound.Bind((New-Object System.Net.IPEndPoint ([Net.IPAddress]::Loopback, 0)))
        $task = $bound.ConnectAsync('203.0.113.5', 9)
        try { [void]$task.Wait(300) } catch { }
    } finally {
        $bound.Close()
    }
}

function Invoke-Albumen([string]$Name, [string]$Exe, [string[]]$ArgumentList, [string]$Stdin) {
    $stdout = Join-Path $work "$Name.stdout"
    $stderr = Join-Path $work "$Name.stderr"
    $start = @{
        FilePath = $Exe
        ArgumentList = $ArgumentList
        WorkingDirectory = $work
        RedirectStandardOutput = $stdout
        RedirectStandardError = $stderr
        PassThru = $true
        Wait = $true
        NoNewWindow = $true
    }
    if ($Stdin) { $start.RedirectStandardInput = $Stdin }
    $proc = Start-Process @start
    if ($proc.ExitCode -ne 0) {
        $err = ''
        if (Test-Path -LiteralPath $stderr) { $err = [IO.File]::ReadAllText($stderr) }
        throw "$Name exited $($proc.ExitCode)`n$err"
    }
    return $stdout
}

function Assert-Mcp([string]$Label, [string]$Path, [string]$Expect) {
    $text = [IO.File]::ReadAllText($Path)
    if ($text -notmatch '"isError"\s*:\s*false') {
        throw "$Label did not return a successful tool result`n$text"
    }
    if ($Expect -and $text.IndexOf($Expect) -lt 0) {
        throw "$Label result did not contain $Expect`n$text"
    }
    Write-Output "zero-network: $Label completed"
}

try {
    $filterHelp = & pktmon filter add --help 2>&1 | Out-String
    if ($filterHelp -notmatch 'process-name') {
        throw @"
pktmon on this runner has no --process-name filter, so a capture cannot be
limited to albumen.exe. A machine-wide capture would include the GitHub
runner's own connections. pktmon filter add --help:
$filterHelp
"@
    }

    $shell = Split-Path -Leaf ([Diagnostics.Process]::GetCurrentProcess().Path)
    Start-Pktmon -ProcessNames $shell
    Invoke-Canary
    Start-Sleep -Milliseconds 500
    $canaryText = Stop-PktmonText
    Assert-Canary $canaryText

    $programs = @($albumen, $mcp)
    $perl = Get-Command perl.exe -ErrorAction SilentlyContinue
    if ($perl) { $programs += $perl.Source }
    foreach ($program in $programs) {
        $rule = "albumen-zero-network-$([guid]::NewGuid().ToString('n'))"
        New-NetFirewallRule -DisplayName $rule -Direction Outbound -Action Block -Program $program -Profile Any -Enabled True | Out-Null
        $rules += $rule
    }
    $auditBefore = (& auditpol /get /subcategory:"Filtering Platform Connection" | Out-String)
    $script:AuditChanged = $false
    if ($auditBefore -notmatch 'Failure') {
        & auditpol /set /subcategory:"Filtering Platform Connection" /failure:enable | Out-Null
        $script:AuditChanged = $true
    }
    $started = Get-Date

    Start-Pktmon -ProcessNames albumen.exe,albumen-mcp.exe,perl.exe
    $scanOut = Invoke-Albumen 'scan' $albumen @('scan', $library, '--manifest', (Join-Path $outDir 'scan.jsonl')) $null
    $scanText = [IO.File]::ReadAllText($scanOut)
    if ($scanText -notmatch 'scanned ') { throw "scan did not report a count`n$scanText" }
    $indexOut = Invoke-Albumen 'index' $mcp @('--library', $library) $indexIn
    Assert-Mcp 'index' $indexOut ''
    $searchOut = Invoke-Albumen 'search' $mcp @('--library', $library) $searchIn
    Assert-Mcp 'search' $searchOut 'IMG_0001.jpg'
    Start-Sleep -Milliseconds 500
    $cleanText = Stop-PktmonText
    $hits = [regex]::Matches($cleanText, '\b(?:\d{1,3}\.){3}\d{1,3}\b|::')
    if ($hits.Count -gt 0) {
        throw "pktmon recorded network traffic from scan, index, or search`n$cleanText"
    }

    Start-Sleep -Seconds 2
    $events = @()
    try {
        $events = @(Get-WinEvent -FilterHashtable @{
            LogName = 'Security'
            Id = 5157
            StartTime = $started.AddSeconds(-2)
        } -ErrorAction Stop)
    } catch {
        if ($_.Exception.Message -notmatch 'No events were found') {
            throw "could not read the Filtering Platform Connection audit log: $($_.Exception.Message)"
        }
    }
    $blocked = @($events | Where-Object {
        $_.Message -match 'albumen\.exe|albumen-mcp\.exe|perl\.exe'
    })
    if ($blocked.Count -gt 0) {
        throw "Windows Firewall audited a blocked connection from scan, index, or search`n$($blocked[0].Message)"
    }

    $hashAfter = (Get-FileHash -Algorithm SHA256 -Path $source).Hash
    $mtimeAfter = (Get-Item -LiteralPath $source).LastWriteTimeUtc.Ticks
    if ($hashAfter -ne $hashBefore -or $mtimeAfter -ne $mtimeBefore) {
        throw 'source bytes or mtime changed'
    }
    Write-Output 'zero-network: windows pktmon process filter and firewall failure audit saw no packet and no blocked connection from scan, index, or search'
} finally {
    Clear-Pktmon
    foreach ($rule in $rules) {
        Remove-NetFirewallRule -DisplayName $rule -ErrorAction SilentlyContinue
    }
    if ($script:AuditChanged) {
        & auditpol /set /subcategory:"Filtering Platform Connection" /failure:disable | Out-Null
    }
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
