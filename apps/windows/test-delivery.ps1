#Requires -Version 7.0
param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Delivery UI fixtures run only on disposable GitHub Windows runners.' }
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
$directory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$stateFile = Join-Path $directory 'delivery-ui-state.json'
$pendingFolder = (New-Item -ItemType Directory -Force (Join-Path $directory 'delivery-ui-protected')).FullName
$env:PIGEONPOST_UI_DELIVERY_STATE = $stateFile
$env:PIGEONPOST_UI_DELIVERY_FOLDER = $pendingFolder
$process = $null
$root = $null
$checks = [Collections.Generic.List[string]]::new()
function Check([bool]$Condition, [string]$Detail) { if (-not $Condition) { throw $Detail }; $checks.Add($Detail) }
try {
    $process = Start-Process -FilePath $Executable -PassThru
    $root = $null
    for ($n = 0; $n -lt 60; $n++) {
        Start-Sleep -Milliseconds 500; $process.Refresh()
        if ($process.HasExited) { throw 'Delivery fixture exited during startup.' }
        if ($process.MainWindowHandle -ne 0) { $root = [System.Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle); if ($root) { break } }
    }
    if (-not $root) { throw 'Delivery fixture window missing.' }
    function Control([string]$Name) {
        $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
        $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    }
    function Wait-Control([string]$Name) {
        for ($n = 0; $n -lt 40; $n++) {
            $element = Control $Name
            if ($element -and -not $element.Current.IsOffscreen -and $element.Current.IsEnabled) { return $element }
            Start-Sleep -Milliseconds 250
        }
        throw "Delivery control unavailable: $Name"
    }
    function Invoke-Control([string]$Name) { (Wait-Control $Name).GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() }
    function Set-ControlText([string]$Name, [string]$Value) { (Wait-Control $Name).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value) }
    function Read-ControlText([string]$Name) { (Wait-Control $Name).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value }
    function State {
        try { if (Test-Path $stateFile) { return Get-Content $stateFile -Raw | ConvertFrom-Json } }
        catch { return $null }
        return $null
    }
    function Wait-Check([scriptblock]$Predicate, [string]$Detail) {
        for ($n = 0; $n -lt 80; $n++) {
            if (& $Predicate) { $checks.Add($Detail); return }
            Start-Sleep -Milliseconds 150
        }
        throw $Detail
    }
    function Capture([string]$Name) {
        Start-Sleep -Milliseconds 300
        $rect = $root.Current.BoundingRectangle
        $bitmap = [Drawing.Bitmap]::new([int]$rect.Width, [int]$rect.Height)
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try { $graphics.CopyFromScreen([int]$rect.Left, [int]$rect.Top, 0, 0, $bitmap.Size); $bitmap.Save((Join-Path $directory $Name), [Drawing.Imaging.ImageFormat]::Png) }
        finally { $graphics.Dispose(); $bitmap.Dispose() }
    }
    $null = Wait-Control 'Message'
    Set-ControlText 'Message' 'Native recovery UI fixture'
    Invoke-Control 'Send'
    $null = Wait-Control 'Review pending deliveries'
    Check ((Read-ControlText 'Message') -eq 'Native recovery UI fixture') 'Unknown delivery preserves the visible composer draft'
    Start-Sleep -Milliseconds 600
    $state = State
    Check ($state -and $state.posts -eq 1 -and $state.committed -eq 1) 'Polling never automatically replays the committed delivery'
    $files = @(Get-ChildItem $pendingFolder -Filter '*.pending' -File -Recurse)
    Check ($files.Count -eq 1 -and -not [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($files[0].FullName)).Contains('Native recovery UI fixture')) 'The actual composer writes a Windows-protected pending file'
    Invoke-Control 'Review pending deliveries'
    $null = Wait-Control 'Saved delivery content'
    Check ((Read-ControlText 'Saved delivery content') -eq 'Native recovery UI fixture') 'Recovery dialog shows the original saved content'
    Check ($null -ne (Control '/preview/design') -and $null -ne (Control '/k/bbbbbbbbbbbbbbbbbbbbbbbbbb')) 'Recovery dialog shows the reviewed handle and frozen canonical recipient'
    Capture 'delivery-review.png'
    Invoke-Control 'Keep pending'
    $null = Wait-Control 'Review pending deliveries'
    Check ((State).posts -eq 1) 'Keeping a delivery pending does not send'
    Invoke-Control 'Review pending deliveries'
    $null = Wait-Control 'Saved delivery content'
    Invoke-Control 'Retry delivery'
    Wait-Check { $s = State; $s -and $s.posts -eq 2 -and $s.committed -eq 1 -and $s.exactRetries } 'Explicit UI retry preserves the original request and confirms one copy'
    Wait-Check { (Read-ControlText 'Message') -eq '' -and @(Get-ChildItem $pendingFolder -Filter '*.pending' -File -Recurse).Count -eq 0 } 'Confirmation clears the original draft and protected pending entry'
    Capture 'delivery-confirmed.png'
    Set-ControlText 'Message' 'Native discard UI fixture'
    Invoke-Control 'Send'
    $null = Wait-Control 'Review pending deliveries'
    Invoke-Control 'Review pending deliveries'
    $null = Wait-Control 'Saved delivery content'
    Check ((Read-ControlText 'Saved delivery content') -eq 'Native discard UI fixture') 'Second recovery dialog remains bound to its own original intent'
    Invoke-Control 'Discard recovery'
    Wait-Check { @(Get-ChildItem $pendingFolder -Filter '*.pending' -File -Recurse).Count -eq 0 } 'Explicit discard removes local recovery'
    Check ((Read-ControlText 'Message') -eq 'Native discard UI fixture') 'Discard preserves the unsent composer draft'
    $state = State
    Check ($state.posts -eq 3 -and $state.committed -eq 2) 'Discard neither sends again nor deletes the already committed fixture copy'
    Capture 'delivery-discarded.png'
    # A first message to a new recipient opens that conversation at once, here with an unconfirmed
    # answer: the thread, its pending row and recovery are in view and the caret is in its box.
    Invoke-Control 'New conversation'
    Set-ControlText 'Pigeonpost address' '/fixture/newpeer'
    Set-ControlText 'First message' 'Native first message fixture'
    $sendCondition = [System.Windows.Automation.AndCondition]::new(
        [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'PrimaryButton'),
        [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, 'Send'))
    $dialogSend = $null
    for ($n = 0; $n -lt 40 -and -not $dialogSend; $n++) { $dialogSend = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $sendCondition); if (-not $dialogSend) { Start-Sleep -Milliseconds 250 } }
    Check ($null -ne $dialogSend) 'New conversation dialog offers Send for a first message'
    $dialogSend.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Wait-Check { $s = State; $s -and $s.posts -eq 4 } 'The first message is sent once from the new conversation'
    Wait-Check {
        $address = Control 'Sender address details'
        $address -and $address.FindFirst([System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, '/fixture/newpeer'))
    } 'The new conversation is selected and open after its first send'
    $null = Wait-Control 'Review pending deliveries'
    Check ((Read-ControlText 'Message') -eq 'Native first message fixture') 'An unconfirmed first message stays in the new conversation with its text kept'
    Wait-Check { (Wait-Control 'Message').Current.HasKeyboardFocus } 'Focus is in the new conversation''s message box'
    Capture 'delivery-new-conversation.png'
    Invoke-Control 'Review pending deliveries'
    $null = Wait-Control 'Saved delivery content'
    Check ((Read-ControlText 'Saved delivery content') -eq 'Native first message fixture') 'The new conversation''s unconfirmed first message has Retry and Discard'
    Invoke-Control 'Discard recovery'
    Wait-Check { @(Get-ChildItem $pendingFolder -Filter '*.pending' -File -Recurse).Count -eq 0 } 'Discarding the first message removes only its recovery'
    @{ passed = $true; checks = $checks } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'delivery-ui.json')
    Write-Host "Native delivery UI: $($checks.Count) checks passed."
} catch {
    try { if ($root) { Capture 'delivery-failed.png' } } catch { }
    @{ passed = $false; checks = $checks; error = $_.ToString() } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'delivery-ui.json')
    throw
} finally {
    if ($process -and -not $process.HasExited) { $process.Kill(); $process.WaitForExit() }
    Remove-Item Env:PIGEONPOST_UI_DELIVERY_STATE -ErrorAction SilentlyContinue
    Remove-Item Env:PIGEONPOST_UI_DELIVERY_FOLDER -ErrorAction SilentlyContinue
}
