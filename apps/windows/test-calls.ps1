#Requires -Version 7.0
# Voice and video calls, end to end on a disposable Windows runner: the UI-test build's WebView2
# against a local stand-in postbox, the repository's call service, and a headless browser peer.
# Fake media devices only (no real microphone or camera); see call-harness/drive.mjs.
param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'This helper is only for disposable GitHub Windows runners.' }
$OutputDirectory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$repository = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
# The fixture inbox needs these even though the call test never attaches or saves a file.
$fixtures = New-Item -ItemType Directory -Force (Join-Path ([IO.Path]::GetTempPath()) "pp-calls-$([Guid]::NewGuid().ToString('N'))")
$env:PIGEONPOST_UI_ATTACHMENT_FILE = Join-Path $fixtures 'unused.txt'
Set-Content $env:PIGEONPOST_UI_ATTACHMENT_FILE 'fixture'
$env:PIGEONPOST_UI_DOWNLOAD_DIRECTORY = $fixtures.FullName
try {
    & node (Join-Path $PSScriptRoot 'call-harness/drive.mjs') $repository $OutputDirectory (Resolve-Path $Executable).Path
    if ($LASTEXITCODE -ne 0) {
        Get-WinEvent -FilterHashtable @{ LogName = 'Application'; StartTime = (Get-Date).AddMinutes(-10) } -ErrorAction SilentlyContinue |
            Where-Object { $_.Message -like '*Pigeonpost*' -or $_.Message -like '*WebView*' } | Select-Object -First 6 |
            ForEach-Object { Write-Host "Event $($_.Id) $($_.ProviderName): $($_.Message)" }
        Get-Content (Join-Path $OutputDirectory 'calls.json') | Write-Host
        throw 'The Windows call test failed; see calls.json.'
    }
    Write-Host 'Windows calls passed.'
} finally {
    Get-Process Pigeonpost -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Remove-Item Env:PIGEONPOST_UI_ATTACHMENT_FILE, Env:PIGEONPOST_UI_DOWNLOAD_DIRECTORY -ErrorAction SilentlyContinue
    Remove-Item $fixtures -Recurse -Force -ErrorAction SilentlyContinue
}
