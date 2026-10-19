#Requires -Version 7.0
# Notification integration in the shape a user runs it: the UI-test build packaged as an MSIX with
# the release manifest, installed, and launched by Explorer in the interactive session that owns
# the taskbar. Proves package identity, shell registration of the notification-area icon, the
# Windows alert setting and that alerts reach notification history. Disposable runners only.
param(
    [Parameter(Mandatory)][string]$UiTestDirectory,
    [Parameter(Mandatory)][string]$ReleaseArtifacts,
    [Parameter(Mandatory)][string]$OutputDirectory
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'This helper is only for disposable GitHub Windows runners.' }
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing, System.Windows.Forms
$OutputDirectory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$work = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString('N'))
$stage = Join-Path $work 'stage'
$signals = 'C:\PigeonpostUiTest'
$cert = $null; $installed = $null
function Capture-Screen([string]$Name) {
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bitmap = [System.Drawing.Bitmap]::new($bounds.Width, $bounds.Height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try { $graphics.CopyFromScreen($bounds.Left, $bounds.Top, 0, 0, $bitmap.Size); $bitmap.Save((Join-Path $OutputDirectory $Name), [System.Drawing.Imaging.ImageFormat]::Png) }
    finally { $graphics.Dispose(); $bitmap.Dispose() }
}
function Find-TrayIcon {
    $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
    @([System.Windows.Automation.AutomationElement]::RootElement.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition) |
        Where-Object { $_.Current.Name -like 'Pigeonpost*' -and $_.Current.ClassName -notlike '*Window*' })
}
try {
    New-Item -ItemType Directory -Force $stage | Out-Null
    Copy-Item -Path (Join-Path $UiTestDirectory '*') -Destination $stage -Recurse
    $logos = Get-ChildItem $ReleaseArtifacts -Recurse -Filter 'Square150x150Logo.png' | Select-Object -First 1
    if (-not $logos) { throw 'Release package logos are missing.' }
    foreach ($logo in 'Square44x44Logo.png', 'Square150x150Logo.png', 'StoreLogo.png') { Copy-Item (Join-Path $logos.DirectoryName $logo) (Join-Path $stage "Assets/$logo") }
    $manifest = (Get-Content (Join-Path $PSScriptRoot 'Packaging/Package.appxmanifest') -Raw).Replace('__ARCH__', 'x64')
    [IO.File]::WriteAllText((Join-Path $stage 'AppxManifest.xml'), $manifest, [Text.UTF8Encoding]::new($false))
    $sdk = Get-ChildItem "${env:ProgramFiles(x86)}/Windows Kits/10/bin" -Directory | Where-Object Name -Match '^10\.0\.\d+\.\d+$' |
        Sort-Object { [Version]$_.Name } -Descending | Select-Object -First 1
    $package = Join-Path $work 'ui-tests.msix'
    & (Join-Path $sdk.FullName 'x64/makeappx.exe') pack /d $stage /p $package /o | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Packaging the UI-test build failed.' }
    $cert = New-SelfSignedCertificate -Type Custom -Subject 'CN=D850EB9E-D10B-4265-83A3-AC88170D8C6D' -KeyUsage DigitalSignature -CertStoreLocation 'Cert:\CurrentUser\My' -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3')
    $certificateFile = Join-Path $work 'test.cer'
    Export-Certificate -Cert $cert -FilePath $certificateFile | Out-Null
    Import-Certificate -FilePath $certificateFile -CertStoreLocation 'Cert:\LocalMachine\TrustedPeople' | Out-Null
    & (Join-Path $sdk.FullName 'x64/signtool.exe') sign /fd SHA256 /sha1 $cert.Thumbprint /s My $package | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Temporary test signing failed.' }
    Add-AppxPackage -Path $package
    $installed = Get-AppxPackage -Name WodoTeknolojiA.PigeonpostDesktop
    if (-not $installed) { throw 'The UI-test MSIX did not register.' }

    if (Test-Path $signals) { Remove-Item $signals -Recurse -Force }
    New-Item -ItemType Directory $signals | Out-Null
    Set-Content (Join-Path $signals 'packaged-notifications.request') 'run'
    $principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
    $owner = (Get-CimInstance Win32_Process -Filter "Name='explorer.exe'" | Select-Object -First 1 | Invoke-CimMethod -MethodName GetOwner)
    Write-Host "Harness user $env:USERNAME (administrator: $($principal.IsInRole('Administrators'))); Explorer runs as $($owner.User)"
    Start-Process explorer.exe -ArgumentList "shell:AppsFolder\$($installed.PackageFamilyName)!App"

    $ready = Join-Path $signals 'alerts.ready'
    $result = Join-Path $signals 'packaged-notifications.json'
    for ($n = 0; $n -lt 180 -and -not (Test-Path $ready) -and -not (Test-Path $result); $n++) { Start-Sleep -Milliseconds 500 }
    if (Test-Path $ready) {
        Start-Sleep -Milliseconds 700
        Capture-Screen 'packaged-alerts.png'
        $icons = Find-TrayIcon
        Write-Host "Notification-area buttons named Pigeonpost: $(@($icons | ForEach-Object { $_.Current.Name }) -join ' | ')"
        Set-Content (Join-Path $signals 'alerts.captured') 'done'
    }
    for ($n = 0; $n -lt 120 -and -not (Test-Path $result); $n++) { Start-Sleep -Milliseconds 500 }
    if (-not (Test-Path $result)) { Capture-Screen 'packaged-notifications-timeout.png'; throw 'The packaged app did not report its notification checks.' }
    Copy-Item $result (Join-Path $OutputDirectory 'packaged-notifications.json')
    $report = Get-Content $result -Raw | ConvertFrom-Json
    Write-Host (Get-Content $result -Raw)
    if (-not $report.passed) { throw 'Packaged notification integration failed.' }
    Write-Host 'Packaged notification integration passed.'
} finally {
    if ($installed) {
        foreach ($log in @("$env:LOCALAPPDATA\Packages\$($installed.PackageFamilyName)\LocalCache\Local\Pigeonpost\logs\desktop.log", "$env:LOCALAPPDATA\Pigeonpost\logs\desktop.log")) {
            if (Test-Path $log) { Copy-Item $log (Join-Path $OutputDirectory 'packaged-desktop.log') -Force; Get-Content $log | Write-Host; break }
        }
        Get-Process Pigeonpost -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        Remove-AppxPackage -Package $installed.PackageFullName
    }
    if ($cert) {
        Remove-Item "Cert:\LocalMachine\TrustedPeople\$($cert.Thumbprint)" -ErrorAction SilentlyContinue
        Remove-Item "Cert:\CurrentUser\My\$($cert.Thumbprint)" -ErrorAction SilentlyContinue
    }
    if (Test-Path $signals) { Remove-Item $signals -Recurse -Force -ErrorAction SilentlyContinue }
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
