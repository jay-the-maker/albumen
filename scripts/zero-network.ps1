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
# observer. Otherwise the observer is an outbound block for albumen.exe,
# albumen-mcp.exe, and perl.exe (ExifTool), audited as Filtering Platform
# failure events. The canary has to show a blocked connect to 203.0.113.5
# and a blocked DNS send to that address on port 53. Windows Firewall does
# not classify a loopback destination, and the script names that gap.

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
        $canaryNotes = @(Invoke-Canary)
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
        Invoke-ObservedCommands
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
        if ($sawLoopbackExternal.Count -gt 0) {
            Write-Output 'zero-network: firewall audit saw a connect from 127.0.0.1 toward 203.0.113.5'
        } else {
            Write-Output @"
zero-network: a connect from a 127.0.0.1 bind toward 203.0.113.5 produced no Filtering Platform event.
Socket result: $($canaryNotes -join '; ')
pktmon cannot attribute that attempt. pktmon filter add on this image:
$probeText
"@
        }
        if ($sawLoopbackDns.Count -gt 0) {
            Write-Output 'zero-network: firewall audit saw a DNS send to 127.0.0.1 port 53'
        } else {
            $loopbackWhy = 'Windows Firewall does not classify loopback destinations, and pktmon has no process filter to attribute one.'
            if ($loopbackRuleNote) {
                $loopbackWhy = "The firewall refused a loopback remote address ($loopbackRuleNote). $loopbackWhy"
            }
            Write-Output "zero-network: a UDP send to 127.0.0.1 port 53 produced no Filtering Platform event. $loopbackWhy"
        }
        Write-Output 'zero-network: windows firewall failure audit saw no connection from scan, index, or search'
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
    $ErrorActionPreference = $restorePreference
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
