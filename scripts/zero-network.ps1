# SPDX-License-Identifier: Apache-2.0
# Prove default-config scan, index, and search make no network connection.
#
# Index is the process-local metadata cache. There is no on-disk catalog and
# no index subcommand. The index run is albumen-mcp, with export and upload
# left off, calling search with a metadata filter so ExifTool reads the files.
# The search run is a second default-config process calling search by name.
#
# windows-2025 pktmon has no process filter. A machine-wide capture would
# include the runner. When --process-name is accepted, that filter is the
# observer. Otherwise the per-process observer is an ETW session on
# Microsoft-Windows-Kernel-Network (NT Kernel Logger if that provider records
# nothing). TCP connect/accept and UDP send/recv carry a payload PID, including
# sends to 127.0.0.1. The canary PID must show an external connect, a DNS send,
# and a loopback send. Scan, index, and search PIDs, including perl.exe, must
# show none. An outbound firewall block with a failure audit is a second layer.
# Windows Firewall still does not classify a loopback destination.

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
$script:AuditsEnabled = @()

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
        # 0x10 is a string: Windows PowerShell 5.1 refuses a native exe when any argument is an int.
        & pktmon @startArgs --flags '0x10'
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
    $notes = New-Object System.Collections.Generic.List[string]
    $udp = New-Object System.Net.Sockets.UdpClient
    try {
        [void]$udp.Send([byte[]](0), 1, '203.0.113.5', 53)
        [void]$udp.Send([byte[]](0), 1, '127.0.0.1', 53)
    } catch {
        $notes.Add("dns: $($_.Exception.Message)")
    } finally {
        $udp.Close()
    }
    $external = New-Object System.Net.Sockets.TcpClient
    try {
        $task = $external.ConnectAsync('203.0.113.5', 9)
        if (-not $task.Wait(300)) {
            $notes.Add('external: timed out')
        } elseif ($task.IsFaulted) {
            $notes.Add("external: $($task.Exception.GetBaseException().Message)")
        }
    } catch {
        $notes.Add("external: $($_.Exception.Message)")
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
        if (-not $task.Wait(300)) {
            $notes.Add('loopback-external: timed out')
        } elseif ($task.IsFaulted) {
            $notes.Add("loopback-external: $($task.Exception.GetBaseException().Message)")
        }
    } catch {
        $notes.Add("loopback-external: $($_.Exception.Message)")
    } finally {
        $bound.Close()
    }
    return $notes
}

function Enable-FailureAudit([string]$Subcategory) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $before = (& auditpol /get /subcategory:"$Subcategory" 2>&1 | Out-String)
        if ($LASTEXITCODE -ne 0) {
            throw "auditpol could not read $Subcategory`n$before"
        }
        if ($before -notmatch 'Failure') {
            $set = (& auditpol /set /subcategory:"$Subcategory" /failure:enable 2>&1 | Out-String)
            if ($LASTEXITCODE -ne 0) {
                throw "auditpol could not enable failure auditing for $Subcategory`n$before`n$set"
            }
            $script:AuditsEnabled += $Subcategory
        }
    } finally {
        $ErrorActionPreference = $previous
    }
}

function Get-EventText($Event) {
    $parts = New-Object System.Collections.Generic.List[string]
    if ($Event.Message) { $parts.Add($Event.Message) }
    foreach ($prop in $Event.Properties) { $parts.Add([string]$prop.Value) }
    return ($parts -join "`n")
}

function Get-FilterEvents([datetime]$Since) {
    $found = @()
    foreach ($id in 5152, 5157) {
        try {
            $found += @(Get-WinEvent -FilterHashtable @{
                LogName = 'Security'
                Id = $id
                StartTime = $Since
            } -ErrorAction Stop)
        } catch {
            if ($_.Exception.Message -notmatch 'No events were found') {
                throw "could not read the Filtering Platform audit log: $($_.Exception.Message)"
            }
        }
    }
    return @($found)
}

$script:TraceSession = 'albumen-zero-net'
$script:KernelLoggerStarted = $false
$script:AnalyticEnabled = $false
$script:ObservedPids = $null
$script:ObservedImages = @{}
$script:WatchProcesses = $false

function Invoke-Quiet([scriptblock]$Command) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        return (& $Command 2>&1 | Out-String)
    } finally {
        $ErrorActionPreference = $previous
    }
}

function Enable-KernelAnalytic {
    $status = Invoke-Quiet { wevtutil gl 'Microsoft-Windows-Kernel-Network/Analytic' }
    if ($status -match 'enabled:\s*true') { return }
    $set = Invoke-Quiet { wevtutil sl 'Microsoft-Windows-Kernel-Network/Analytic' /e:true /q:true }
    if ($LASTEXITCODE -eq 0) { $script:AnalyticEnabled = $true }
}

function Start-KernelTrace([string]$Etl, [string]$Keywords) {
    if (Test-Path -LiteralPath $Etl) { Remove-Item -LiteralPath $Etl -Force }
    [void](Invoke-Quiet { logman stop $script:TraceSession -ets })
    Enable-KernelAnalytic
    # Windows PowerShell 5.1 throws "Argument types do not match" and never
    # starts the process when a native argument is an int. Every logman number is a string.
    $text = Invoke-Quiet {
        logman start $script:TraceSession -ets -o $Etl -max '128' -nb '32' '32' -bs '128' `
            -p Microsoft-Windows-Kernel-Network $Keywords '0xff' `
            -p Microsoft-Windows-Kernel-Process '0x10' '0xff'
    }
    if ($LASTEXITCODE -eq 0) { return }
    $text = Invoke-Quiet {
        logman start $script:TraceSession -ets -o $Etl -max '128' -nb '32' '32' -bs '128' `
            -p Microsoft-Windows-Kernel-Network $Keywords '0xff'
    }
    if ($LASTEXITCODE -ne 0) {
        throw "logman could not start Microsoft-Windows-Kernel-Network (keywords $Keywords)`n$text"
    }
}

function Stop-KernelTrace {
    [void](Invoke-Quiet { logman stop $script:TraceSession -ets })
}

function Start-NtKernelLogger([string]$Etl) {
    if (Test-Path -LiteralPath $Etl) { Remove-Item -LiteralPath $Etl -Force }
    $existing = Invoke-Quiet { logman query 'NT Kernel Logger' -ets }
    if ($LASTEXITCODE -eq 0 -and $existing -match 'Running') {
        throw @"
NT Kernel Logger is already running on this runner, so a second kernel network
session cannot start.
$existing
"@
    }
    # 0x10000 is EVENT_TRACE_FLAG_NETWORK_TCPIP.
    $text = Invoke-Quiet {
        logman start 'NT Kernel Logger' -ets -o $Etl -max '128' -p 'Windows Kernel Trace' '0x10000'
    }
    if ($LASTEXITCODE -ne 0) {
        throw "logman could not start the NT Kernel Logger network provider`n$text"
    }
    $script:KernelLoggerStarted = $true
}

function Stop-NtKernelLogger {
    if (-not $script:KernelLoggerStarted) { return }
    [void](Invoke-Quiet { logman stop 'NT Kernel Logger' -ets })
    $script:KernelLoggerStarted = $false
}

function Convert-TraceXml([string]$Etl, [string]$Xml) {
    if (Test-Path -LiteralPath $Xml) { Remove-Item -LiteralPath $Xml -Force }
    $text = Invoke-Quiet { tracerpt $Etl -o $Xml -of XML -y }
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $Xml)) {
        $help = Invoke-Quiet { tracerpt -? }
        throw "tracerpt could not decode the kernel network trace`n$text`n$help"
    }
}

function Get-AddressForms([string]$Value) {
    $forms = New-Object System.Collections.Generic.List[string]
    $trimmed = $Value.Trim()
    if (-not $trimmed) { return @() }
    $forms.Add($trimmed)
    if ($trimmed -match '^\d+$') {
        $bytes = [BitConverter]::GetBytes([uint32]$trimmed)
        $forms.Add("$($bytes[0]).$($bytes[1]).$($bytes[2]).$($bytes[3])")
        $forms.Add("$($bytes[3]).$($bytes[2]).$($bytes[1]).$($bytes[0])")
    }
    return @($forms)
}

function Test-Port([int]$Port, [int]$Want) {
    $swapped = (($Want -band 0xff) -shl 8) -bor (($Want -shr 8) -band 0xff)
    return $Port -eq $Want -or $Port -eq $swapped
}

function Get-TraceEvents([string]$XmlPath) {
    $text = [IO.File]::ReadAllText($XmlPath)
    $found = New-Object System.Collections.Generic.List[object]
    foreach ($chunk in [regex]::Matches($text, '(?s)<Event\b[^>]*>.*?</Event>')) {
        $body = $chunk.Value
        $id = 0
        if ($body -match '<EventID[^>]*>\s*(\d+)\s*</EventID>') { $id = [int]$Matches[1] }
        $payloadPid = -1
        if ($body -match '<Data Name="(?:PID|Pid)">\s*(\d+)\s*</Data>') { $payloadPid = [int]$Matches[1] }
        $processId = -1
        if ($body -match '<Data Name="ProcessID">\s*(\d+)\s*</Data>') { $processId = [int]$Matches[1] }
        $parentId = -1
        if ($body -match '<Data Name="ParentProcessID">\s*(\d+)\s*</Data>') { $parentId = [int]$Matches[1] }
        $daddr = ''
        if ($body -match '<Data Name="(?:daddr|DestAddress|RemoteAddress)">\s*([^<]*?)\s*</Data>') { $daddr = $Matches[1] }
        $saddr = ''
        if ($body -match '<Data Name="(?:saddr|SrcAddress|LocalAddress)">\s*([^<]*?)\s*</Data>') { $saddr = $Matches[1] }
        $dport = 0
        if ($body -match '<Data Name="(?:dport|DestPort|RemotePort)">\s*(\d+)\s*</Data>') { $dport = [int]$Matches[1] }
        $image = ''
        if ($body -match '<Data Name="(?:ImageFileName|ImageName|ProcessName)">\s*([^<]*?)\s*</Data>') {
            $image = $Matches[1]
        }
        $provider = ''
        if ($body -match '<Provider[^>]*Name="([^"]+)"') { $provider = $Matches[1] }
        $found.Add([pscustomobject]@{
            Id = $id
            Pid = $payloadPid
            ProcessId = $processId
            ParentId = $parentId
            Daddr = $daddr
            Saddr = $saddr
            Dport = $dport
            Image = $image
            Provider = $provider
            Body = $body
        })
    }
    return @($found)
}

function Merge-ProcessImages($Events) {
    $changed = $true
    while ($changed) {
        $changed = $false
        foreach ($event in $Events) {
            $procId = [int]$event.ProcessId
            if ($procId -lt 0) { continue }
            $leaf = [IO.Path]::GetFileName([string]$event.Image)
            $parentKnown = $event.ParentId -ge 0 -and $script:ObservedPids.Contains([int]$event.ParentId)
            $selfKnown = $script:ObservedPids.Contains($procId)
            $ownImage = $leaf -match '^albumen(\.exe|-mcp\.exe)$'
            if ($parentKnown -or $selfKnown -or $ownImage) {
                if ($script:ObservedPids.Add($procId)) { $changed = $true }
                if ($leaf) { $script:ObservedImages[$procId] = $leaf }
            }
        }
    }
}

function Invoke-TracedCanary([string]$Etl) {
    $attempts = @(
        @{ Mode = 'manifest'; Keywords = '0x30' },
        @{ Mode = 'manifest'; Keywords = '0xffffffffffffffff' },
        @{ Mode = 'kernel-logger'; Keywords = '' }
    )
    $errors = New-Object System.Collections.Generic.List[string]
    foreach ($attempt in $attempts) {
        try {
            if ($attempt.Mode -eq 'manifest') {
                Start-KernelTrace $Etl $attempt.Keywords
                try {
                    foreach ($note in @(Invoke-Canary)) {
                        if ($note) { $script:CanaryNotes += [string]$note }
                    }
                    Start-Sleep -Milliseconds 400
                } finally {
                    Stop-KernelTrace
                }
            } else {
                Start-NtKernelLogger $Etl
                try {
                    foreach ($note in @(Invoke-Canary)) {
                        if ($note) { $script:CanaryNotes += [string]$note }
                    }
                    Start-Sleep -Milliseconds 400
                } finally {
                    Stop-NtKernelLogger
                }
            }
            $xml = "$Etl.xml"
            Convert-TraceXml $Etl $xml
            Assert-CanaryTrace @(Get-TraceEvents $xml) $PID
            $script:TraceMode = [string]$attempt.Mode
            $script:TraceKeywords = [string]$attempt.Keywords
            return
        } catch {
            $errors.Add($_.Exception.Message)
            Stop-KernelTrace
            Stop-NtKernelLogger
        }
    }
    throw @"
No kernel ETW session recorded the canary's external connect, DNS send, and loopback send.
$($errors -join "`n----`n")
"@
}

function Invoke-TracedCommands([string]$Etl) {
    $script:ObservedPids = New-Object 'System.Collections.Generic.HashSet[int]'
    $script:ObservedImages = @{}
    $script:WatchProcesses = $true
    if ($script:TraceMode -eq 'kernel-logger') {
        Start-NtKernelLogger $Etl
        try { Invoke-ObservedCommands } finally { Stop-NtKernelLogger }
    } else {
        Start-KernelTrace $Etl $script:TraceKeywords
        try { Invoke-ObservedCommands } finally { Stop-KernelTrace }
    }
    $xml = "$Etl.xml"
    Convert-TraceXml $Etl $xml
    $events = @(Get-TraceEvents $xml)
    Merge-ProcessImages $events
    $perlSeen = @($script:ObservedImages.GetEnumerator() | Where-Object { [string]$_.Value -match '^perl\.exe$' })
    if ($perlSeen.Count -eq 0) {
        $listed = ($script:ObservedImages.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ', '
        throw "the per-process observer never saw perl.exe, so an ExifTool loopback send would not be attributed. PIDs: $listed"
    }
    Assert-CleanTrace $events
    $script:WatchProcesses = $false
}

function Test-WatchedAddress($Event, [string]$Address) {
    foreach ($form in (Get-AddressForms $Event.Daddr)) {
        if ($form -eq $Address) { return $true }
    }
    return $false
}

function Assert-CanaryTrace($Events, [int]$CanaryPid) {
    $mine = @($Events | Where-Object { $_.Pid -eq $CanaryPid -and $_.Daddr })
    $external = @($mine | Where-Object { (Test-WatchedAddress $_ '203.0.113.5') -and -not (Test-Port $_.Dport 53) })
    $dns = @($mine | Where-Object { (Test-WatchedAddress $_ '203.0.113.5') -and (Test-Port $_.Dport 53) })
    $loopback = @($mine | Where-Object { (Test-WatchedAddress $_ '127.0.0.1') -and (Test-Port $_.Dport 53) })
    if ($external.Count -eq 0 -or $dns.Count -eq 0 -or $loopback.Count -eq 0) {
        $sample = ''
        $shown = @($mine | Select-Object -First 1)
        if ($shown.Count -eq 0) { $shown = @($Events | Where-Object { $_.Daddr } | Select-Object -First 1) }
        if ($shown.Count -gt 0) { $sample = $shown[0].Body }
        throw @"
Kernel network ETW did not record the canary PID $CanaryPid.
External events: $($external.Count). DNS port 53 events: $($dns.Count). Loopback 127.0.0.1:53 events: $($loopback.Count). Parsed events: $($Events.Count).
Sample:
$sample
"@
    }
    Write-Output "zero-network: kernel ETW saw canary PID $CanaryPid external ($($external.Count)), DNS ($($dns.Count)), and loopback 127.0.0.1:53 ($($loopback.Count))"
}

function Assert-CleanTrace($Events) {
    $hits = @($Events | Where-Object { $_.Pid -ge 0 -and $script:ObservedPids.Contains([int]$_.Pid) -and ($_.Daddr -or $_.Saddr) })
    if ($hits.Count -gt 0) {
        $hit = $hits[0]
        throw "kernel ETW recorded a network event for PID $($hit.Pid) ($($script:ObservedImages[[int]$hit.Pid])) event $($hit.Id) $($hit.Saddr) -> $($hit.Daddr):$($hit.Dport)"
    }
    $listed = ($script:ObservedImages.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ', '
    Write-Output "zero-network: kernel ETW saw no TCP or UDP event for scan, index, or search PIDs ($listed)"
}

function Note-Tree([int]$Root) {
    $frontier = @($Root)
    for ($depth = 0; $depth -lt 4; $depth++) {
        $next = @()
        foreach ($parent in $frontier) {
            $kids = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$parent" -ErrorAction SilentlyContinue)
            foreach ($kid in $kids) {
                $id = [int]$kid.ProcessId
                if ($script:ObservedPids.Add($id)) {
                    $script:ObservedImages[$id] = [string]$kid.Name
                    $next += $id
                }
            }
        }
        if ($next.Count -eq 0) { break }
        $frontier = $next
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
        Wait = (-not $script:WatchProcesses)
        NoNewWindow = $true
    }
    if ($Stdin) { $start.RedirectStandardInput = $Stdin }
    $proc = Start-Process @start
    if ($script:WatchProcesses) {
        [void]$script:ObservedPids.Add([int]$proc.Id)
        $script:ObservedImages[[int]$proc.Id] = [IO.Path]::GetFileName($Exe)
        while (-not $proc.HasExited) {
            Note-Tree ([int]$proc.Id)
            Start-Sleep -Milliseconds 20
        }
        $proc.WaitForExit()
        Note-Tree ([int]$proc.Id)
        $proc.Refresh()
    }
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

function Add-BlockRule([string]$Program, [string[]]$RemoteAddress) {
    $rule = "albumen-zero-network-$([guid]::NewGuid().ToString('n'))"
    $params = @{
        DisplayName = $rule
        Direction = 'Outbound'
        Action = 'Block'
        Program = $Program
        Profile = 'Any'
        ErrorAction = 'Stop'
    }
    if ($RemoteAddress) { $params.RemoteAddress = $RemoteAddress }
    New-NetFirewallRule @params | Out-Null
    $script:rules += $rule
    return $rule
}

function Assert-SourceUnchanged {
    $hashAfter = (Get-FileHash -Algorithm SHA256 -Path $source).Hash
    $mtimeAfter = (Get-Item -LiteralPath $source).LastWriteTimeUtc.Ticks
    if ($hashAfter -ne $hashBefore -or $mtimeAfter -ne $mtimeBefore) {
        throw 'source bytes or mtime changed'
    }
}

function Invoke-ObservedCommands {
    $scanOut = Invoke-Albumen 'scan' $albumen @('scan', $library, '--manifest', (Join-Path $outDir 'scan.jsonl')) $null
    $scanText = [IO.File]::ReadAllText($scanOut)
    if ($scanText -notmatch 'scanned ') { throw "scan did not report a count`n$scanText" }
    $indexOut = Invoke-Albumen 'index' $mcp @('--library', $library) $indexIn
    Assert-Mcp 'index' $indexOut ''
    $searchOut = Invoke-Albumen 'search' $mcp @('--library', $library) $searchIn
    Assert-Mcp 'search' $searchOut 'IMG_0001.jpg'
}

try {
    $probePreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $probeText = (& pktmon filter add albumen-probe --process-name 'albumen.exe' 2>&1 | Out-String)
    $probeOk = $LASTEXITCODE -eq 0 -and $probeText -notmatch 'invalid|unknown|error|fail'
    $ErrorActionPreference = $probePreference
    Clear-Pktmon
    if (-not $probeOk) {
        $ErrorActionPreference = 'Continue'
        $usage = (& pktmon filter add 2>&1 | Out-String)
        $ErrorActionPreference = $probePreference
        $probeText = "$probeText`n$usage"
    }

    if ($probeOk) {
        Write-Output 'zero-network: pktmon accepted --process-name'
        $shell = Split-Path -Leaf ([Diagnostics.Process]::GetCurrentProcess().Path)
        Start-Pktmon -ProcessNames $shell
        [void](Invoke-Canary)
        Start-Sleep -Milliseconds 500
        $canaryText = Stop-PktmonText
        Assert-Canary $canaryText

        foreach ($program in @($albumen, $mcp)) {
            [void](Add-BlockRule $program $null)
        }
        $perl = Get-Command perl.exe -ErrorAction SilentlyContinue
        if ($perl) { [void](Add-BlockRule $perl.Source $null) }
        Enable-FailureAudit 'Filtering Platform Connection'
        $started = Get-Date
        Start-Pktmon -ProcessNames albumen.exe,albumen-mcp.exe,perl.exe
        Invoke-ObservedCommands
        Start-Sleep -Milliseconds 500
        $cleanText = Stop-PktmonText
        $hits = [regex]::Matches($cleanText, '\b(?:\d{1,3}\.){3}\d{1,3}\b|::')
        if ($hits.Count -gt 0) {
            throw "pktmon recorded network traffic from scan, index, or search`n$cleanText"
        }
        Start-Sleep -Seconds 2
        $events = @(Get-FilterEvents $started.AddSeconds(-2) | Where-Object {
            $_.Id -eq 5157 -and $_.Message -match 'albumen\.exe|albumen-mcp\.exe|perl\.exe'
        })
        if ($events.Count -gt 0) {
            throw "Windows Firewall audited a blocked connection from scan, index, or search`n$($events[0].Message)"
        }
        Assert-SourceUnchanged
        Write-Output 'zero-network: windows pktmon process filter and firewall failure audit saw no packet and no blocked connection from scan, index, or search'
    } else {
        Write-Output 'zero-network: pktmon filter add has no process-name on this image, so the observer is the firewall audit'
        $powershell = (Get-Process -Id $PID).Path
        $loopbackRuleNote = ''
        try {
            [void](Add-BlockRule $powershell @('203.0.113.5', '127.0.0.1'))
        } catch {
            $loopbackRuleNote = $_.Exception.Message
            [void](Add-BlockRule $powershell @('203.0.113.5'))
        }
        Enable-FailureAudit 'Filtering Platform Connection'
        Enable-FailureAudit 'Filtering Platform Packet Drop'
        $canaryStart = (Get-Date).AddSeconds(-2)
        $script:CanaryNotes = @()
        $script:TraceMode = ''
        $script:TraceKeywords = ''
        Invoke-TracedCanary (Join-Path $work 'kernel-canary.etl')
        $canaryNotes = @($script:CanaryNotes)
        $canaryEvents = @()
        for ($attempt = 0; $attempt -lt 10; $attempt++) {
            Start-Sleep -Seconds 1
            $canaryEvents = @(Get-FilterEvents $canaryStart | Where-Object {
                $text = Get-EventText $_
                ($text -match 'powershell\.exe' -and $text -match "Process ID:\s+$PID\b") -or
                ($_.Properties.Count -gt 1 -and ([string]$_.Properties[0].Value) -eq "$PID" -and ([string]$_.Properties[1].Value) -match 'powershell\.exe')
            })
            $sawExternal = @($canaryEvents | Where-Object {
                $text = Get-EventText $_
                $text -match '203\.0\.113\.5' -and $text -notmatch 'Destination Port:\s+53\b' -and $text -notmatch '(?m)^53$'
            })
            $sawDns = @($canaryEvents | Where-Object {
                $text = Get-EventText $_
                $text -match '203\.0\.113\.5' -and ($text -match 'Destination Port:\s+53\b' -or $text -match '(?m)^53$')
            })
            if ($sawExternal.Count -gt 0 -and $sawDns.Count -gt 0) { break }
        }
        if ($sawExternal.Count -eq 0 -or $sawDns.Count -eq 0) {
            $all = @(Get-FilterEvents $canaryStart)
            $sample = ($all | Select-Object -First 1 | ForEach-Object { Get-EventText $_ }) -join "`n"
            throw @"
Windows Firewall did not audit the canary. External events: $($sawExternal.Count). DNS port 53 events: $($sawDns.Count). Matched powershell events: $($canaryEvents.Count). Events in the window: $($all.Count).
Socket notes: $($canaryNotes -join '; ')
pktmon filter add:
$probeText
Sample:
$sample
"@
        }
        $sawLoopbackExternal = @($canaryEvents | Where-Object {
            $text = Get-EventText $_
            $text -match '127\.0\.0\.1' -and $text -match '203\.0\.113\.5'
        })
        $sawLoopbackDns = @($canaryEvents | Where-Object {
            $text = Get-EventText $_
            $text -match '127\.0\.0\.1' -and $text -notmatch '203\.0\.113\.5' -and ($text -match 'Destination Port:\s+53\b' -or $text -match '(?m)^53$')
        })
        $canaryRule = $script:rules[-1]
        Remove-NetFirewallRule -DisplayName $canaryRule -ErrorAction SilentlyContinue
        $script:rules = @($script:rules | Where-Object { $_ -ne $canaryRule })

        foreach ($program in @($albumen, $mcp)) {
            [void](Add-BlockRule $program $null)
        }
        $perl = Get-Command perl.exe -ErrorAction SilentlyContinue
        if (-not $perl) {
            throw 'perl.exe is not on PATH, so the ExifTool process cannot be named in a firewall rule'
        }
        [void](Add-BlockRule $perl.Source $null)
        Start-Sleep -Seconds 1
        $cleanStart = Get-Date
        Invoke-TracedCommands (Join-Path $work 'kernel-clean.etl')
        $cleanEvents = @()
        for ($attempt = 0; $attempt -lt 5; $attempt++) {
            Start-Sleep -Seconds 1
            $cleanEvents = @(Get-FilterEvents $cleanStart.AddSeconds(-2) | Where-Object {
                (Get-EventText $_) -match 'albumen\.exe|albumen-mcp\.exe|perl\.exe'
            })
            if ($cleanEvents.Count -gt 0) { break }
        }
        if ($cleanEvents.Count -gt 0) {
            throw "Windows Firewall audited a connection from scan, index, or search`n$($cleanEvents[0].Message)"
        }
        Assert-SourceUnchanged
        Write-Output "zero-network: firewall audit saw $($sawExternal.Count) blocked connect(s) to 203.0.113.5 and $($sawDns.Count) blocked DNS send(s) to 203.0.113.5 port 53"
        if ($sawLoopbackDns.Count -eq 0) {
            $loopbackWhy = 'Windows Firewall does not classify a loopback destination.'
            if ($loopbackRuleNote) {
                $loopbackWhy = "The firewall refused a loopback remote address ($loopbackRuleNote). $loopbackWhy"
            }
            Write-Output "zero-network: $loopbackWhy The kernel ETW session is the observer for 127.0.0.1."
        }
        Write-Output 'zero-network: windows kernel ETW and firewall failure audit saw no connection from scan, index, or search'
    }
} finally {
    Clear-Pktmon
    foreach ($rule in $script:rules) {
        Remove-NetFirewallRule -DisplayName $rule -ErrorAction SilentlyContinue
    }
    $restorePreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    foreach ($sub in $script:AuditsEnabled) {
        & auditpol /set /subcategory:"$sub" /failure:disable 2>&1 | Out-Null
    }
    & logman stop $script:TraceSession -ets 2>&1 | Out-Null
    if ($script:KernelLoggerStarted) {
        & logman stop 'NT Kernel Logger' -ets 2>&1 | Out-Null
    }
    if ($script:AnalyticEnabled) {
        & wevtutil sl 'Microsoft-Windows-Kernel-Network/Analytic' /e:false /q:true 2>&1 | Out-Null
    }
    $ErrorActionPreference = $restorePreference
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
