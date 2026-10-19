# Copy a corpus and write a Windows Sandbox configuration that maps the copy
# read-only with networking disabled. The original path is not mapped.
param(
    [string]$Corpus = "fixtures",
    [string]$Work = "$env:USERPROFILE\albumen-trial"
)

$ErrorActionPreference = "Stop"
$copy = Join-Path $Work "copy"
$out = Join-Path $Work "out"
if (Test-Path $Work) {
    Remove-Item -Recurse -Force $Work
}
New-Item -ItemType Directory -Path $copy | Out-Null
New-Item -ItemType Directory -Path $out | Out-Null
Copy-Item -Path (Join-Path $Corpus "*") -Destination $copy -Recurse

$wsb = @"
<Configuration>
  <Networking>Disable</Networking>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>$copy</HostFolder>
      <SandboxFolder>C:\albumen\library</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>$out</HostFolder>
      <SandboxFolder>C:\albumen\out</SandboxFolder>
      <ReadOnly>false</ReadOnly>
    </MappedFolder>
  </MappedFolders>
</Configuration>
"@
$wsbPath = Join-Path $Work "albumen.wsb"
Set-Content -Path $wsbPath -Value $wsb -Encoding UTF8
Write-Output "rung 5 prepared: $wsbPath"
Write-Output "library copy: $copy"
Write-Output "Start Windows Sandbox with that file, then scan C:\albumen\library into C:\albumen\out."
