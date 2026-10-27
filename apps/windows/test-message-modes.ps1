#Requires -Version 7.0
param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Message UI fixtures require a disposable GitHub Windows runner.' }
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
$directory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$stateFile = Join-Path $directory 'message-modes-state.json'
$env:PIGEONPOST_UI_MESSAGE_MODES_STATE = $stateFile
$env:PIGEONPOST_UI_MESSAGE_MODES_FOLDER = (New-Item -ItemType Directory -Force (Join-Path $directory 'message-modes-protected')).FullName
$process = $null; $root = $null
$checks = [Collections.Generic.List[string]]::new()
function Check([bool]$Condition, [string]$Detail) { if (-not $Condition) { throw $Detail }; $checks.Add($Detail) }
try {
    $process = Start-Process -FilePath $Executable -PassThru
    for ($n = 0; $n -lt 60; $n++) {
        Start-Sleep -Milliseconds 500; $process.Refresh()
        if ($process.HasExited) { throw 'Message UI fixture exited during startup.' }
        if ($process.MainWindowHandle -ne 0) { $root = [System.Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle); if ($root) { break } }
    }
    if (-not $root) { throw 'Message UI fixture window missing.' }
    function Control([string]$Name, [string]$Id = '') {
        $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
        if ($Id) {
            $identity = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
            $condition = [System.Windows.Automation.AndCondition]::new($condition, $identity)
        }
        $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    }
    function Wait-Control([string]$Name, [string]$Id = '') {
        for ($n = 0; $n -lt 40; $n++) {
            $element = Control $Name $Id
            if ($element -and -not $element.Current.IsOffscreen -and $element.Current.IsEnabled) { return $element }
            Start-Sleep -Milliseconds 250
        }
        throw "Message UI control unavailable: $Name $Id"
    }
    function Invoke-Control([string]$Name, [string]$Id = '') { (Wait-Control $Name $Id).GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() }
    function Set-Text([string]$Value) { (Wait-Control 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value) }
    function Read-Text { (Wait-Control 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value }
    function State { try { if (Test-Path $stateFile) { return Get-Content $stateFile -Raw | ConvertFrom-Json } } catch { }; return $null }
    function Wait-Check([scriptblock]$Predicate, [string]$Detail) {
        for ($n = 0; $n -lt 60; $n++) { if (& $Predicate) { $checks.Add($Detail); return }; Start-Sleep -Milliseconds 200 }
        throw $Detail
    }
    function Capture([string]$Name) {
        Start-Sleep -Milliseconds 250; $rect = $root.Current.BoundingRectangle
        $bitmap = [Drawing.Bitmap]::new([int]$rect.Width, [int]$rect.Height); $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try { $graphics.CopyFromScreen([int]$rect.Left, [int]$rect.Top, 0, 0, $bitmap.Size); $bitmap.Save((Join-Path $directory $Name), [Drawing.Imaging.ImageFormat]::Png) }
        finally { $graphics.Dispose(); $bitmap.Dispose() }
    }
    $null = Wait-Control 'Message'
    Set-Text 'Ordinary native draft'
    Invoke-Control 'Message actions' 'design-3'; Invoke-Control 'Reply'
    Check ((Read-Text) -eq 'Ordinary native draft' -and $null -ne (Control 'Replying to message')) 'Reply shows its original quoted context and preserves the ordinary draft'
    Invoke-Control 'Cancel message action'
    Check ((Read-Text) -eq 'Ordinary native draft') 'Cancelling reply preserves the existing text'
    Invoke-Control 'Message actions' 'design-2'; Invoke-Control 'Edit message'
    Wait-Check { $null -ne (Control 'Editing message') -and (Read-Text) -eq "I'll check the layout, keyboard shortcuts, and how it works in a smaller window." } 'Edit is offered on the owned recent message and prefills its displayed text'
    Check (-not (Control 'Attach files…').Current.IsEnabled) 'Edit disables file attachment controls'
    Set-Text 'Cancelled native edit'; Invoke-Control 'Cancel message action'
    Check ((Read-Text) -eq 'Ordinary native draft') 'Cancelling edit restores the previous ordinary draft'
    Invoke-Control 'Message actions' 'design-2'; Invoke-Control 'Edit message'
    Set-Text 'Edited via native message controls'
    Capture 'message-edit.png'
    Invoke-Control 'Save edit'
    Wait-Check { $s = State; $s -and $s.edits -eq 1 -and $s.originalEditParent -and (Read-Text) -eq 'Ordinary native draft' } 'Save edit uses the original owned copy, text only, and restores the ordinary draft'
    Invoke-Control 'Message actions' 'design-2'; Invoke-Control 'Message info'
    $content = Wait-Control 'Message content'
    Check ($content.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.IsReadOnly) 'Message info content is read-only'
    Check ($content.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -eq 'Edited via native message controls') 'Message info displays the folded current text'
    $version = Wait-Control 'Message version'
    Check ($version.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.IsReadOnly) 'Version history remains read-only data'
    Capture 'message-info.png'; Invoke-Control 'Done'
    Invoke-Control 'Message actions' 'design-3'; Invoke-Control 'Reply'
    Set-Text 'Reply via native message controls'; Invoke-Control 'Send'
    Wait-Check { $s = State; $s -and $s.replies -eq 1 -and $s.originalReplyParent -and (Read-Text) -eq '' } 'Native reply sends the original parent and bounded quote and clears the confirmed reply draft'
    $state = State
    Check ($state.posts -eq 2 -and $state.committed -eq 2 -and $state.infoReads -eq 1) 'Cancelled modes never send and message info only reads'
    Capture 'message-modes-complete.png'
    @{ passed = $true; checks = $checks } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'message-modes-ui.json')
} catch {
    try { if ($root) { Capture 'message-modes-failure.png' } } catch { }
    @{ passed = $false; checks = $checks; error = $_.Exception.Message } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'message-modes-ui.json')
    throw
} finally {
    Remove-Item Env:PIGEONPOST_UI_MESSAGE_MODES_STATE, Env:PIGEONPOST_UI_MESSAGE_MODES_FOLDER -ErrorAction SilentlyContinue
    if ($process -and -not $process.HasExited) { $null = $process.CloseMainWindow(); if (-not $process.WaitForExit(5000)) { $process.Kill() } }
}
