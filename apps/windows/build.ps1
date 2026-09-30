#Requires -Version 7.0
[CmdletBinding()]
param(
    [ValidateSet('x64', 'ARM64')][string]$Architecture = 'x64',
    [ValidateSet('Debug', 'Release')][string]$Configuration = 'Release',
    [switch]$Package
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'WinUI compilation and MSIX packaging require Windows. Core tests can run on any .NET 10 host.' }

Push-Location $PSScriptRoot
try {
    # Use a fresh directory so stale files from a previous publish cannot enter an MSIX.
    $buildId = [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss') + '-' + [Guid]::NewGuid().ToString('N').Substring(0, 8)
    $buildRoot = Join-Path $PSScriptRoot "artifacts/$Architecture/$buildId"
    $publish = Join-Path $buildRoot 'publish'
    $runtime = if ($Architecture -eq 'ARM64') { 'win-arm64' } else { 'win-x64' }
    New-Item -ItemType Directory -Path $publish -Force | Out-Null

    & dotnet run --project Pigeonpost.Core.Tests/Pigeonpost.Core.Tests.csproj -c $Configuration
    if ($LASTEXITCODE -ne 0) { throw 'Core tests failed.' }

    & dotnet publish Pigeonpost.Desktop/Pigeonpost.Desktop.csproj -c $Configuration -r $runtime `
        "-p:Platform=$Architecture" --self-contained true -o $publish --nologo
    if ($LASTEXITCODE -ne 0) { throw "WinUI publish failed for $Architecture." }
    if (-not (Test-Path -LiteralPath (Join-Path $publish 'Pigeonpost.exe'))) { throw 'Publish did not produce Pigeonpost.exe.' }

    if ($Package) {
        # Derive package sizes from the existing Pigeonpost artwork as part of the Windows build.
        Add-Type -AssemblyName System.Drawing
        $source = [System.Drawing.Image]::FromFile((Join-Path $publish 'Assets/Logo.png'))
        try {
            foreach ($logo in @(@('Square44x44Logo.png', 44), @('Square150x150Logo.png', 150), @('StoreLogo.png', 50))) {
                $size = [int]$logo[1]
                $bitmap = [System.Drawing.Bitmap]::new($size, $size)
                $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
                try {
                    $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
                    $graphics.Clear([System.Drawing.Color]::Transparent)
                    $graphics.DrawImage($source, 0, 0, $size, $size)
                    $bitmap.Save((Join-Path $publish "Assets/$($logo[0])"), [System.Drawing.Imaging.ImageFormat]::Png)
                } finally { $graphics.Dispose(); $bitmap.Dispose() }
            }
        } finally { $source.Dispose() }

        $manifest = Get-Content -LiteralPath 'Packaging/Package.appxmanifest' -Raw
        $manifest = $manifest.Replace('__ARCH__', $Architecture.ToLowerInvariant())
        [System.IO.File]::WriteAllText((Join-Path $publish 'AppxManifest.xml'), $manifest, [System.Text.UTF8Encoding]::new($false))

        $sdkRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10/bin'
        $makeAppx = Get-ChildItem -LiteralPath $sdkRoot -Directory |
            Where-Object { $_.Name -match '^10\.0\.\d+\.\d+$' } |
            Sort-Object { [Version]$_.Name } -Descending |
            ForEach-Object { Join-Path $_.FullName 'x64/makeappx.exe' } |
            Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
        if (-not $makeAppx) { throw 'Install the Windows SDK (including MakeAppx) to create an MSIX.' }
        $packagePath = Join-Path $buildRoot "Pigeonpost-preview-$runtime.msix"
        & $makeAppx pack /d $publish /p $packagePath /o
        if ($LASTEXITCODE -ne 0) { throw 'MSIX validation/packaging failed.' }
        Write-Host "Unsigned development package: $packagePath"
    }

    $zip = Join-Path $buildRoot "Pigeonpost-preview-$runtime.zip"
    Compress-Archive -Path (Join-Path $publish '*') -DestinationPath $zip
    Write-Host "Preview executable: $(Join-Path $publish 'Pigeonpost.exe')"
    Write-Host "Preview archive: $zip"
} finally { Pop-Location }
