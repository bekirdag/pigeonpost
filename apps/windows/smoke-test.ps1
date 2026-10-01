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
    function Invoke-Control([string]$Name) {
        (Wait-Element $Name).GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 250
    }
    function Set-Text([string]$Name, [string]$Value) {
        (Wait-Element $Name).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value)
        Start-Sleep -Milliseconds 150
    }
    function Dialog-Button([string]$Id) {
        $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
        for ($n = 0; $n -lt 40; $n++) {
            $control = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
            if ($control -and $control.Current.IsEnabled) { return $control }
            Start-Sleep -Milliseconds 250
        }
        throw "Dialog button unavailable: $Id"
    }
    function Commit-Dialog([string]$ExpectedName) {
        $button = Dialog-Button 'PrimaryButton'
        if ($button.Current.Name -ne $ExpectedName) { throw "Expected dialog action $ExpectedName, got $($button.Current.Name)" }
        $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 500
    }
    function Open-Conversation([string]$Address) {
        Invoke-Control 'New conversation'
        $null = Wait-Element 'First message'
        Set-Text 'Pigeonpost address' $Address
        Commit-Dialog 'Open'
    }
    if ($Fixture) {
        $null = Wait-Element 'New conversation'
        Set-Text 'Message' 'Please review the release checklist.'
        Capture 'inbox.png'
        Invoke-Control 'Send'
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne '') { throw 'Successful send did not clear composer.' }
        Invoke-Control 'Copy address'
        if ((Get-Clipboard -Raw).Trim() -ne '/preview/design') { throw 'Peer copy did not preserve the routing address.' }
        Invoke-Control 'Copy my address'
        if ((Get-Clipboard -Raw).Trim() -ne '/preview/main') { throw 'Own copy did not preserve the routing address.' }
        Invoke-Control 'Sender details'
        $null = Wait-Element 'Known sender'
        $null = Wait-Element 'Full permissions'
        $null = Wait-Element 'run_tests'
        Capture 'sender-details.png'
        Invoke-Control 'Cancel'
        Invoke-Control 'Sender address details'
        $null = Wait-Element 'Known sender'
        Invoke-Control 'Cancel'
        Set-Text 'Find in this subject' 'History message 1:'
        $null = Wait-Element '1 of 1'
        Capture 'history-find.png'
        Set-Text 'Find in this subject' ''

        Invoke-Control 'New conversation'
        Set-Text 'Pigeonpost address' '/preview/new-agent'
        Set-Text 'First message' 'First message from the native dialog.'
        Capture 'new-conversation.png'
        Commit-Dialog 'Send'
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne '') { throw 'First message was not sent.' }
        $null = Wait-Element 'First message from the native dialog.'

        # The details shortcut must close its native dialog before changing the mailbox.
        for ($cycle = 0; $cycle -lt 3; $cycle++) {
            Open-Conversation '/preview/team'
            Invoke-Control 'Sender details'
            $null = Wait-Element 'This is one of your mailboxes.'
            Invoke-Control 'Open this mailbox'
            $null = Wait-Element 'Operations'
            Set-Text 'Message' 'Draft in team mailbox'
            Open-Conversation '/preview/main'
            Invoke-Control 'Sender details'
            Invoke-Control 'Open this mailbox'
            Open-Conversation '/preview/design'
            Set-Text 'Message' 'Draft in main mailbox'
        }
        Capture 'repeated-mailbox-switch.png'
        Invoke-Control 'New subject'
        Set-Text 'Subject title' 'Native parity subject'
        Capture 'new-subject.png'
        Commit-Dialog 'Create'
        $null = Wait-Element 'Native parity subject'
        Invoke-Control 'Delete subject…'
        $null = Wait-Element 'Cancel'
        Capture 'delete-subject.png'
        Invoke-Control 'Cancel'
        $null = Wait-Element 'Native parity subject'
        Invoke-Control 'Delete subject…'
        Commit-Dialog 'Delete subject'
        if (Element 'Native parity subject') { throw 'Deleted subject remains visible.' }
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Draft in main mailbox') { throw 'Deleting a different subject lost the main draft.' }
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
