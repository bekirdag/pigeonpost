#Requires -Version 7.0
param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$OutputDirectory, [switch]$Fixture)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing, System.Windows.Forms
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$process = Start-Process -FilePath $Executable -PassThru
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
        $rect = $root.Current.BoundingRectangle
        $bitmap = [System.Drawing.Bitmap]::new([int]$rect.Width, [int]$rect.Height)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CopyFromScreen([int]$rect.X, [int]$rect.Y, 0, 0, $bitmap.Size)
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
