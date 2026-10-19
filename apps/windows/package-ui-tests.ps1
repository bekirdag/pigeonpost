#Requires -Version 7.0
# Packages the UI-test build with the release manifest and a temporary, machine-trusted test
# certificate, so the notification checks run with real package identity. Disposable runners only.
param([Parameter(Mandatory)][string]$UiTestDirectory, [Parameter(Mandatory)][string]$ReleaseArtifacts, [Parameter(Mandatory)][string]$Output)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'This helper is only for disposable GitHub Windows runners.' }
$work = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString('N'))
$stage = Join-Path $work 'stage'
New-Item -ItemType Directory -Force $stage | Out-Null
try {
    Copy-Item -Path (Join-Path $UiTestDirectory '*') -Destination $stage -Recurse
    $logos = Get-ChildItem $ReleaseArtifacts -Recurse -Filter 'Square150x150Logo.png' | Select-Object -First 1
    if (-not $logos) { throw 'Release package logos are missing.' }
    foreach ($logo in 'Square44x44Logo.png', 'Square150x150Logo.png', 'StoreLogo.png') { Copy-Item (Join-Path $logos.DirectoryName $logo) (Join-Path $stage "Assets/$logo") }
    $manifest = (Get-Content (Join-Path $PSScriptRoot 'Packaging/Package.appxmanifest') -Raw).Replace('__ARCH__', 'x64')
    [IO.File]::WriteAllText((Join-Path $stage 'AppxManifest.xml'), $manifest, [Text.UTF8Encoding]::new($false))
    $sdk = Get-ChildItem "${env:ProgramFiles(x86)}/Windows Kits/10/bin" -Directory | Where-Object Name -Match '^10\.0\.\d+\.\d+$' |
        Sort-Object { [Version]$_.Name } -Descending | Select-Object -First 1
    New-Item -ItemType Directory -Force (Split-Path $Output) | Out-Null
    & (Join-Path $sdk.FullName 'x64/makeappx.exe') pack /d $stage /p $Output /o | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Packaging the UI-test build failed.' }
    $cert = New-SelfSignedCertificate -Type Custom -Subject 'CN=D850EB9E-D10B-4265-83A3-AC88170D8C6D' -KeyUsage DigitalSignature -CertStoreLocation 'Cert:\CurrentUser\My' -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3')
    Export-Certificate -Cert $cert -FilePath (Join-Path $work 'test.cer') | Out-Null
    Import-Certificate -FilePath (Join-Path $work 'test.cer') -CertStoreLocation 'Cert:\LocalMachine\TrustedPeople' | Out-Null
    & (Join-Path $sdk.FullName 'x64/signtool.exe') sign /fd SHA256 /sha1 $cert.Thumbprint /s My $Output | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Temporary test signing failed.' }
    # Exchange folder the packaged app reads (Explorer launches do not pass our environment).
    $signals = 'C:\PigeonpostUiTest'
    New-Item -ItemType Directory -Force $signals | Out-Null
    & icacls $signals /grant '*S-1-5-32-545:(OI)(CI)M' /Q | Out-Null
    Write-Host "Signed UI-test package: $Output"
} finally { Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue }
