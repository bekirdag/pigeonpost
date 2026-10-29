#Requires -Version 7.0
param([Parameter(Mandatory)][string]$Package, [Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# Only the disposable GitHub Windows test machine trusts this temporary certificate.
$work = Join-Path ([System.IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $work | Out-Null
$cert = $null
$installed = $null
try {
    $cert = New-SelfSignedCertificate -Type Custom -Subject 'CN=D850EB9E-D10B-4265-83A3-AC88170D8C6D' -KeyUsage DigitalSignature -CertStoreLocation 'Cert:\CurrentUser\My' -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3')
    $certificateFile = Join-Path $work 'test.cer'
    Export-Certificate -Cert $cert -FilePath $certificateFile | Out-Null
    Import-Certificate -FilePath $certificateFile -CertStoreLocation 'Cert:\LocalMachine\TrustedPeople' | Out-Null
    $signedPackage = Join-Path $work 'test.msix'
    Copy-Item -LiteralPath $Package -Destination $signedPackage
    $signTool = Get-ChildItem "${env:ProgramFiles(x86)}/Windows Kits/10/bin" -Directory |
        Where-Object Name -Match '^10\.0\.\d+\.\d+$' | Sort-Object { [Version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName 'x64/signtool.exe' } | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $signTool) { throw 'Windows SDK SignTool is missing.' }
    & $signTool sign /fd SHA256 /sha1 $cert.Thumbprint /s My $signedPackage
    if ($LASTEXITCODE -ne 0) { throw 'Temporary test signing failed.' }
    Add-AppxPackage -Path $signedPackage
    $installed = Get-AppxPackage -Name WodoTeknolojiA.PigeonpostDesktop
    if (-not $installed -or $installed.Status -ne 'Ok') { throw 'MSIX did not register correctly.' }
    & "$PSScriptRoot/smoke-test.ps1" -PackageFamilyName $installed.PackageFamilyName -OutputDirectory $OutputDirectory
    # Windows App SDK push (WindowsPush.cs): the installed package must register its push server.
    # The channel result is evidence only: it needs Microsoft's mapping of this package family to
    # the Entra app, which this test-signed package shares with the Store package.
    # The same two places smoke-test.ps1 looks: a full-trust package may or may not redirect LocalAppData.
    $logs = @("$env:LOCALAPPDATA\Pigeonpost\logs\desktop.log", "$env:LOCALAPPDATA\Packages\$($installed.PackageFamilyName)\LocalCache\Local\Pigeonpost\logs\desktop.log")
    $pushLines = @()
    for ($i = 0; $i -lt 60; $i++) {
        $log = $logs | Where-Object { Test-Path $_ } | Select-Object -First 1
        if ($log) { $pushLines = @(Get-Content $log | Where-Object { $_ -match 'push' }) }
        if ($pushLines -match 'Push channel (ready|unavailable)|Push channel request failed') { break }
        Start-Sleep -Milliseconds 500
    }
    $pushLines | Set-Content (Join-Path $OutputDirectory 'push.log')
    $pushLines | ForEach-Object { Write-Host "push: $_" }
    if (-not ($pushLines -match 'Windows push registered')) { throw 'The installed package did not register Windows App SDK push.' }
    Write-Host "Installed package passed: $($installed.PackageFullName)"
} finally {
    if ($installed) { Remove-AppxPackage -Package $installed.PackageFullName }
    if ($cert) {
        Remove-Item "Cert:\LocalMachine\TrustedPeople\$($cert.Thumbprint)" -ErrorAction SilentlyContinue
        Remove-Item "Cert:\CurrentUser\My\$($cert.Thumbprint)" -ErrorAction SilentlyContinue
    }
    Remove-Item -LiteralPath $work -Recurse -Force
}
