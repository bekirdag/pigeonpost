#Requires -Version 7.0
# Live streams in WebView2, end to end on a disposable Windows runner (call-harness/streams.mjs):
# local LiveKit, the stream stand-in postbox, the repository's call service, synthetic media only.
param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$OutputDirectory, [Parameter(Mandatory)][string]$LiveKit)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'This helper is only for disposable GitHub Windows runners.' }
$OutputDirectory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$repository = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$fixtures = New-Item -ItemType Directory -Force (Join-Path ([IO.Path]::GetTempPath()) "pp-streams-$([Guid]::NewGuid().ToString('N'))")
$env:PIGEONPOST_UI_ATTACHMENT_FILE = Join-Path $fixtures 'unused.txt'
Set-Content $env:PIGEONPOST_UI_ATTACHMENT_FILE 'fixture'
$env:PIGEONPOST_UI_DOWNLOAD_DIRECTORY = $fixtures.FullName
$env:LIVEKIT_SERVER = (Resolve-Path $LiveKit).Path
try {
    & node (Join-Path $PSScriptRoot 'call-harness/streams.mjs') $repository $OutputDirectory (Resolve-Path $Executable).Path
    if ($LASTEXITCODE -ne 0) { Get-Content (Join-Path $OutputDirectory 'streams.json') | Write-Host; throw 'The Windows live stream test failed; see streams.json.' }
    Write-Host 'Windows live streams passed.'
} finally {
    Get-Process Pigeonpost, livekit-server -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Remove-Item Env:PIGEONPOST_UI_ATTACHMENT_FILE, Env:PIGEONPOST_UI_DOWNLOAD_DIRECTORY, Env:LIVEKIT_SERVER -ErrorAction SilentlyContinue
    Remove-Item $fixtures -Recurse -Force -ErrorAction SilentlyContinue
}
