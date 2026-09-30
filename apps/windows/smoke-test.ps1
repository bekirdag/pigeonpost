#Requires -Version 7.0
param([string]$Executable, [string]$PackageFamilyName, [Parameter(Mandatory)][string]$OutputDirectory, [switch]$Fixture)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing, System.Windows.Forms
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class NativeWindowBounds {
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int width, int height, uint flags);
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr window, int attribute, out Rect value, int size);
}
'@
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
if ($PackageFamilyName) {
    $before = @(Get-Process Pigeonpost -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
    Start-Process explorer.exe -ArgumentList "shell:AppsFolder\$PackageFamilyName!App"
    $process = $null
    for ($i = 0; $i -lt 40; $i++) {
        $process = Get-Process Pigeonpost -ErrorAction SilentlyContinue | Where-Object { $_.Id -notin $before } | Select-Object -First 1
        if ($process) { break }
        Start-Sleep -Milliseconds 500
    }
    if (-not $process) { throw 'Installed MSIX failed to activate.' }
} else {
    if (-not $Executable) { throw 'Executable or package family is required.' }
    $process = Start-Process -FilePath $Executable -PassThru
}
try {
    $root = $null
    for ($i = 0; $i -lt 60; $i++) {
        Start-Sleep -Milliseconds 500
        $process.Refresh()
        if ($process.HasExited) { throw "Desktop exited during launch ($($process.ExitCode))." }
        if ($process.MainWindowHandle -ne 0) {
            $root = [System.Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
            if ($root) { break }
        }
    }
    if (-not $root) { throw 'No native desktop window appeared.' }
    $screen = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
    $null = [NativeWindowBounds]::SetWindowPos($process.MainWindowHandle, [IntPtr]::Zero, 12, 12, [Math]::Min(1440, $screen.Width - 24), [Math]::Min(900, $screen.Height - 24), 4)
    Start-Sleep -Milliseconds 500
    function Element([string]$Name) {
        $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
        $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    }
    function Wait-Element([string]$Name) {
        for ($n = 0; $n -lt 40; $n++) { $e = Element $Name; if ($e) { return $e }; Start-Sleep -Milliseconds 250 }
        throw "Native control missing: $Name"
    }
    function Capture([string]$Name) {
        Start-Sleep -Milliseconds 500
        $rect = [NativeWindowBounds+Rect]::new()
        if ([NativeWindowBounds]::DwmGetWindowAttribute($process.MainWindowHandle, 9, [ref]$rect, 16) -ne 0) { throw 'Could not obtain visible native window bounds.' }
        $bitmap = [System.Drawing.Bitmap]::new($rect.Right - $rect.Left, $rect.Bottom - $rect.Top)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
            $bitmap.Save((Join-Path $OutputDirectory $Name), [System.Drawing.Imaging.ImageFormat]::Png)
        } finally { $graphics.Dispose(); $bitmap.Dispose() }
    }
    if ($Fixture) {
        $null = Wait-Element 'New conversation'
        $composer = Wait-Element 'Message'
        $composer.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('Please review the release checklist.')
        Capture 'inbox.png'
        $send = Wait-Element 'Send'
        $send.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 500
        if ($composer.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne '') { throw 'Successful send did not clear composer.' }
        $new = Wait-Element 'New subject'
        $new.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        $null = Wait-Element 'Cancel'
        Capture 'new-subject.png'
        (Element 'Cancel').GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    } else {
        $button = Wait-Element 'Sign in or create account'
        $null = Wait-Element 'Privacy policy'
        if (-not $button.Current.IsEnabled) { throw 'Sign-in button is not enabled.' }
        if (Element 'Could not access your saved account. Please try again.') { throw 'Windows credential vault could not be read.' }
        Capture 'welcome.png'
    }
    Write-Host 'Native Windows UI smoke passed.'
} finally {
    if (-not $process.HasExited) { $null = $process.CloseMainWindow(); if (-not $process.WaitForExit(5000)) { $process.Kill() } }
}
