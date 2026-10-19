#Requires -Version 7.0
# Installs the signed UI-test MSIX for the current user and runs the notification checks with
# package identity. Mode "shell" runs in the runner's interactive session (the taskbar owner) and
# proves the notification-area icon; mode "alerts" runs as a standard user, where Windows supports
# app alerts, and proves they reach notification history. Disposable runners only.
param([Parameter(Mandatory)][string]$Package, [Parameter(Mandatory)][ValidateSet('shell', 'alerts')][string]$Mode, [Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing, System.Windows.Forms
Add-Type @'
using System;
using System.Runtime.InteropServices;
[ComImport, Guid("2e941141-7f97-4756-ba1d-9decde894a3d"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IApplicationActivationManager {
    [PreserveSig] int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string appUserModelId, [MarshalAs(UnmanagedType.LPWStr)] string arguments, int options, out uint processId);
}
[ComImport, Guid("45BA127D-10A8-46EA-8AB7-56EA9078943C")] class ApplicationActivationManager { }
public static class PackagedLauncher {
    public static uint Activate(string aumid) {
        uint id; int hr = ((IApplicationActivationManager)new ApplicationActivationManager()).ActivateApplication(aumid, "", 0, out id);
        if (hr != 0) Marshal.ThrowExceptionForHR(hr);
        return id;
    }
}
'@
$signals = 'C:\PigeonpostUiTest'
$OutputDirectory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$installed = $null
function Capture-Screen([string]$Name) {
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bitmap = [System.Drawing.Bitmap]::new($bounds.Width, $bounds.Height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try { $graphics.CopyFromScreen($bounds.Left, $bounds.Top, 0, 0, $bitmap.Size); $bitmap.Save((Join-Path $OutputDirectory $Name), [System.Drawing.Imaging.ImageFormat]::Png) }
    catch { Write-Host "Screen capture unavailable: $($_.Exception.Message)" }
    finally { $graphics.Dispose(); $bitmap.Dispose() }
}
function Named([string]$Pattern) {
    $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
    @([System.Windows.Automation.AutomationElement]::RootElement.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition) | Where-Object { $_.Current.Name -like $Pattern })
}
try {
    Add-AppxPackage -Path $Package
    $installed = Get-AppxPackage -Name WodoTeknolojiA.PigeonpostDesktop
    if (-not $installed) { throw 'The UI-test MSIX did not register.' }
    foreach ($file in Get-ChildItem $signals -File -ErrorAction SilentlyContinue) { Remove-Item $file.FullName -Force }
    Set-Content (Join-Path $signals 'packaged-notifications.request') $Mode
    $principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
    Write-Host "Mode $Mode as $env:USERNAME (administrator: $($principal.IsInRole('Administrators')))"
    $aumid = "$($installed.PackageFamilyName)!App"
    if ($Mode -eq 'shell') { Start-Process explorer.exe -ArgumentList "shell:AppsFolder\$aumid" } # As a user starts it.
    else { Write-Host "Activated process $([PackagedLauncher]::Activate($aumid))" }

    $ready = Join-Path $signals 'alerts.ready'
    $result = Join-Path $signals "packaged-notifications-$Mode.json"
    for ($n = 0; $n -lt 180 -and -not (Test-Path $ready) -and -not (Test-Path $result); $n++) { Start-Sleep -Milliseconds 500 }
    if (Test-Path $ready) {
        Start-Sleep -Milliseconds 700
        Capture-Screen "packaged-$Mode.png"
        if ($Mode -eq 'shell') {
            Write-Host "Visible notification-area buttons: $((Named 'Pigeonpost*' | ForEach-Object { $_.Current.Name }) -join ' | ')"
            $chevron = Named '*hidden icons*' | Select-Object -First 1
            if ($chevron) {
                $chevron.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
                Start-Sleep -Milliseconds 800
                Write-Host "Overflow buttons: $((Named 'Pigeonpost*' | ForEach-Object { $_.Current.Name }) -join ' | ')"
                Capture-Screen 'packaged-shell-overflow.png'
                [System.Windows.Forms.SendKeys]::SendWait('{ESC}')
            }
        }
        Set-Content (Join-Path $signals 'alerts.captured') 'done'
    }
    for ($n = 0; $n -lt 120 -and -not (Test-Path $result); $n++) { Start-Sleep -Milliseconds 500 }
    if (-not (Test-Path $result)) { Capture-Screen "packaged-$Mode-timeout.png"; throw "The packaged app did not report its $Mode checks." }
    Copy-Item $result (Join-Path $OutputDirectory "packaged-notifications-$Mode.json")
    Write-Host (Get-Content $result -Raw)
    if (-not (Get-Content $result -Raw | ConvertFrom-Json).passed) { throw "Packaged $Mode notification checks failed." }
    Write-Host "Packaged $Mode notification checks passed."
} finally {
    if ($installed) {
        $log = "$env:LOCALAPPDATA\Packages\$($installed.PackageFamilyName)\LocalCache\Local\Pigeonpost\logs\desktop.log"
        if (Test-Path $log) { Copy-Item $log (Join-Path $OutputDirectory "packaged-$Mode-desktop.log") -Force; Get-Content $log | Write-Host }
        Get-Process Pigeonpost -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
        Remove-AppxPackage -Package $installed.PackageFullName
    }
    Remove-Item (Join-Path $signals 'packaged-notifications.request') -ErrorAction SilentlyContinue
}
