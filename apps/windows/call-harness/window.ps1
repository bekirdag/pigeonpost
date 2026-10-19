#Requires -Version 7.0
# Helper for drive.mjs on a Windows runner: capture the app window, or read a button's enabled state.
param([Parameter(Mandatory)][int]$ProcessId, [Parameter(Mandatory)][ValidateSet('capture', 'enabled', 'size')][string]$Action, [string]$Path, [string]$Name)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class CallWindow {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr window, int attribute, out Rect value, int size);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int width, int height, uint flags);
}
'@
$process = Get-Process -Id $ProcessId
if ($process.MainWindowHandle -eq 0) { throw 'The app has no visible window.' }
if ($Action -eq 'size') {
    # The same frame as the UI smoke screenshots (Partner Center accepts 1366x768 and larger).
    $null = [CallWindow]::SetWindowPos($process.MainWindowHandle, [IntPtr]::Zero, 12, 12, 1440, 900, 4)
    Write-Output 'sized'
} elseif ($Action -eq 'capture') {
    $null = [CallWindow]::SetForegroundWindow($process.MainWindowHandle)
    Start-Sleep -Milliseconds 400
    $rect = [CallWindow+Rect]::new()
    if ([CallWindow]::DwmGetWindowAttribute($process.MainWindowHandle, 9, [ref]$rect, 16) -ne 0) { throw 'No window bounds.' }
    $bitmap = [System.Drawing.Bitmap]::new($rect.Right - $rect.Left, $rect.Bottom - $rect.Top)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try { $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size); $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png) }
    finally { $graphics.Dispose(); $bitmap.Dispose() }
    Write-Output 'captured'
} else {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
    # The app's own (XAML) button: the call page inside WebView2 has hidden buttons with the same names.
    $condition = [System.Windows.Automation.AndCondition]::new(
        [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name),
        [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::FrameworkIdProperty, 'XAML'),
        [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button))
    $element = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    if (-not $element) { throw "No control named $Name." }
    Write-Output $element.Current.IsEnabled
}
