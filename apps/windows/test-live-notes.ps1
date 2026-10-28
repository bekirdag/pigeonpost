#Requires -Version 7.0
# Native round three on the disposable runner's standard user: the LIVE banner, Go live and stage
# invitation in a group, the group composer keeping focus, and the Meeting notes screen decrypting a
# synthetic brief with this device's notes key. UI-test build and its stand-in postbox only.
param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Live stream and meeting notes UI fixtures require a disposable GitHub Windows runner.' }
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing, System.Windows.Forms
$directory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$fixtures = New-Item -ItemType Directory -Force (Join-Path ([IO.Path]::GetTempPath()) "pp-live-notes-$([Guid]::NewGuid().ToString('N'))")
$env:PIGEONPOST_UI_ATTACHMENT_FILE = Join-Path $fixtures 'unused.txt'
Set-Content $env:PIGEONPOST_UI_ATTACHMENT_FILE 'fixture'
$env:PIGEONPOST_UI_DOWNLOAD_DIRECTORY = $fixtures.FullName
$env:PIGEONPOST_UI_LIVE_NOTES = '1'
$script:process = $null; $script:root = $null
$checks = [Collections.Generic.List[string]]::new()
function Check([bool]$Condition, [string]$Detail) { if (-not $Condition) { throw $Detail }; $checks.Add($Detail) }
function Start-App {
    $script:process = Start-Process -FilePath $Executable -PassThru; $script:root = $null
    for ($n = 0; $n -lt 60; $n++) {
        Start-Sleep -Milliseconds 500; $script:process.Refresh()
        if ($script:process.HasExited) { throw 'Live/notes UI fixture exited during startup.' }
        if ($script:process.MainWindowHandle -ne 0) { $script:root = [System.Windows.Automation.AutomationElement]::FromHandle($script:process.MainWindowHandle); if ($script:root) { return } }
    }
    throw 'Live/notes UI fixture window missing.'
}
function Stop-App { if ($script:process -and -not $script:process.HasExited) { $null = $script:process.CloseMainWindow(); if (-not $script:process.WaitForExit(5000)) { $script:process.Kill() } } }
function All { @($script:root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)) }
function Like([string]$Pattern) { All | Where-Object { $_.Current.Name -like $Pattern } | Select-Object -First 1 }
function Wait-Like([string]$Pattern) {
    for ($n = 0; $n -lt 75; $n++) { $element = Like $Pattern; if ($element -and $element.Current.IsEnabled) { return $element }; Start-Sleep -Milliseconds 200 }
    throw "Live/notes UI control unavailable: $Pattern"
}
function Invoke-Like([string]$Pattern) { (Wait-Like $Pattern).GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 300 }
function Set-Text([string]$Name, [string]$Value) { (Wait-Like $Name).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value) }
function Wait-Check([scriptblock]$Predicate, [string]$Detail) {
    for ($n = 0; $n -lt 75; $n++) { if (& $Predicate) { $checks.Add($Detail); return }; Start-Sleep -Milliseconds 200 }; throw $Detail
}
function Dialog-Button([string]$Id) {
    $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
    for ($n = 0; $n -lt 40; $n++) { $control = $script:root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition); if ($control -and $control.Current.IsEnabled) { return $control }; Start-Sleep -Milliseconds 250 }
    throw "Dialog button unavailable: $Id"
}
function Commit-Dialog([string]$Expected) {
    $button = Dialog-Button 'PrimaryButton'
    if ($button.Current.Name -ne $Expected) { throw "Expected dialog action $Expected, got $($button.Current.Name)" }
    $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 500
}
function Focused { $f = [System.Windows.Automation.AutomationElement]::FocusedElement; if ($f) { $f.Current.Name } else { '' } }
function Capture([string]$Name) {
    Start-Sleep -Milliseconds 300; $rect = $script:root.Current.BoundingRectangle
    $bitmap = [Drawing.Bitmap]::new([int]$rect.Width, [int]$rect.Height); $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try { $graphics.CopyFromScreen([int]$rect.Left, [int]$rect.Top, 0, 0, $bitmap.Size); $bitmap.Save((Join-Path $directory $Name), [Drawing.Imaging.ImageFormat]::Png) }
    finally { $graphics.Dispose(); $bitmap.Dispose() }
}
function Create-Group([string]$Name) {
    Invoke-Like 'New group'
    Set-Text 'Group name' $Name
    Commit-Dialog 'Create'
    $null = Wait-Like 'Group messages'
}
try {
    Start-App

    # ---- Meeting notes ----
    Invoke-Like 'Meeting notes'
    $null = Wait-Like 'Meeting notes, *Ready · host Preview · kept*'
    Check ($null -ne (Like 'Meeting notes, *Being prepared*')) 'A meeting still being prepared is listed'
    Check ($null -ne (Like 'Meeting notes, *host Preview Design*')) 'Notes made for another device are listed'
    Capture 'notes-list.png'
    Invoke-Like 'Meeting notes, *Ready · host Preview · kept*'
    $null = Wait-Like '•  Release Windows and Linux together'
    Check ($null -ne (Like 'Launch review')) 'The brief title becomes the heading'
    Check ($null -ne (Like 'The synthetic launch review agreed the release order.')) 'The summary is decrypted on this device'
    Check ($null -ne (Like '•  Preview Design: Update the store notes (due Friday)')) 'Action items are shown with owner and due date'
    Check ($null -ne (Like '•  00:21  Preview Design: Screenshots are ready.')) 'The transcript is shown'
    Capture 'notes-brief.png'
    Invoke-Like 'Copy notes as text'
    $script:copied = ''
    try {
        Wait-Check {
            $script:copied = ("$(Get-Clipboard -Raw)" -replace "`r", '')
            $script:copied.StartsWith('Launch review') -and $script:copied.Contains("Decisions:`n- Release Windows and Linux together") -and $script:copied.Contains("Transcript:`n- [00:04] Preview Main: Let's go through the checklist.`n- [00:21] Preview Design: Screenshots are ready.")
        } 'Copy as text puts the plain brief on the clipboard'
    } finally { Set-Content (Join-Path $directory 'live-notes-clipboard.txt') $script:copied }
    Invoke-Like 'All meeting notes'
    Invoke-Like 'Meeting notes, *host Preview Design*'
    $null = Wait-Like 'These notes were encrypted for your other devices.*'
    $checks.Add('Notes wrapped only for another device are explained, not opened')
    Capture 'notes-other-device.png'
    Invoke-Like 'All meeting notes'
    Invoke-Like 'Meeting notes, *Being prepared*'
    $null = Wait-Like 'The notes are still being prepared. Check back in a few minutes.'
    $checks.Add('Notes in preparation say so')
    Invoke-Like 'Delete my copy of these notes'
    Commit-Dialog 'Delete'
    Wait-Check { $null -eq (Like 'Meeting notes, *Being prepared*') -and $null -ne (Like 'Meeting notes, *Ready · host Preview · kept*') } 'Delete my copy removes only that meeting'

    # ---- Groups: Go live, composer focus ----
    Invoke-Like 'Groups'
    Create-Group 'Studio'
    $null = Wait-Like 'Go live'
    Check ($null -eq (Like 'Watch the live stream')) 'No LIVE banner without a stream'
    Invoke-Like 'Go live'
    $null = Wait-Like 'Stream title'
    Check ($null -ne (Like 'Show my camera') -and $null -ne (Like 'Share my screen')) 'The Go live dialog offers camera and screen'
    Set-Text 'Stream title' 'Sprint demo'
    Capture 'go-live-dialog.png'
    Commit-Dialog 'Go live'
    $null = Wait-Like 'Going live needs the call view, which this preview doesn''t load.'
    $checks.Add('Go live hands the stream to the call host (absent in this preview)')

    $draft = Wait-Like 'Group message'
    Wait-Check { $draft.Current.IsEnabled } 'The group composer is ready once keyed'
    $draft.SetFocus(); Start-Sleep -Milliseconds 300
    [System.Windows.Forms.SendKeys]::SendWait('Typed then Ctrl and Enter')
    [System.Windows.Forms.SendKeys]::SendWait('^{ENTER}')
    $null = Wait-Like 'Typed then Ctrl and Enter'
    Wait-Check { (Focused) -eq 'Group message' } 'The group composer keeps focus after Ctrl+Enter'
    [System.Windows.Forms.SendKeys]::SendWait('Sent with the button')
    Invoke-Like 'Send to group'
    $null = Wait-Like 'Sent with the button'
    Wait-Check { (Focused) -eq 'Group message' } 'The group composer keeps focus after the Send button'

    # ---- Groups: a live stream with a stage invitation ----
    Create-Group 'Live rehearsal'
    $banner = Wait-Like 'Watch the live stream'
    Check ($null -ne (Like 'Preview Design is live: Launch rehearsal')) 'The LIVE banner names the streamer and the title'
    Check ($null -eq (Like 'Go live')) 'Go live is not offered while the group is live'
    Check ($null -ne (Like 'Preview Design invited you onto the stage.*')) 'A stage invitation for this inbox is shown'
    Capture 'group-live-banner.png'
    Invoke-Like 'Watch the live stream'
    $null = Wait-Like 'Watching needs the call view, which this preview doesn''t load.'
    $checks.Add('Watch hands the stream to the call host (absent in this preview)')
    Invoke-Like 'Group info'
    $null = Wait-Like 'Let everyone go live'
    Capture 'group-info-live-policy.png'
    Invoke-Like 'Let everyone go live'
    Start-Sleep -Milliseconds 800
    Invoke-Like 'Group info'
    $null = Wait-Like 'Only admins can go live'
    $checks.Add('An admin can choose who may go live')
    (Dialog-Button 'CloseButton').GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Start-Sleep -Milliseconds 400
    $null = $banner

    @{ passed = $true; checks = $checks } | ConvertTo-Json -Depth 3 | Set-Content (Join-Path $directory 'live-notes.json')
    Write-Host "Live streams and meeting notes UI passed ($($checks.Count) checks)."
} catch {
    try { Capture 'live-notes-failure.png' } catch { }
    @{ passed = $false; checks = $checks; error = "$_" } | ConvertTo-Json -Depth 3 | Set-Content (Join-Path $directory 'live-notes.json')
    throw
} finally {
    Stop-App
    Remove-Item Env:PIGEONPOST_UI_LIVE_NOTES, Env:PIGEONPOST_UI_ATTACHMENT_FILE, Env:PIGEONPOST_UI_DOWNLOAD_DIRECTORY -ErrorAction SilentlyContinue
    Remove-Item $fixtures -Recurse -Force -ErrorAction SilentlyContinue
}
