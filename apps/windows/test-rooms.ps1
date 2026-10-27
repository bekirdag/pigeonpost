#Requires -Version 7.0
param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Organization room UI fixtures require a disposable GitHub Windows runner.' }
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
$directory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$stateFile = Join-Path $directory 'rooms-state.json'
Remove-Item $stateFile -ErrorAction SilentlyContinue
$env:PIGEONPOST_UI_ROOMS_STATE = $stateFile
$env:PIGEONPOST_UI_ROOMS_FOLDER = (New-Item -ItemType Directory -Force (Join-Path $directory 'rooms-protected')).FullName
$env:PIGEONPOST_UI_ROOMS_LOSSES = '1'
$script:process = $null; $script:root = $null
$checks = [Collections.Generic.List[string]]::new()
function Check([bool]$Condition, [string]$Detail) { if (-not $Condition) { throw $Detail }; $checks.Add($Detail) }
function Start-App {
    $script:process = Start-Process -FilePath $Executable -PassThru; $script:root = $null
    for ($n = 0; $n -lt 60; $n++) {
        Start-Sleep -Milliseconds 500; $script:process.Refresh()
        if ($script:process.HasExited) { throw 'Rooms UI fixture exited during startup.' }
        if ($script:process.MainWindowHandle -ne 0) { $script:root = [System.Windows.Automation.AutomationElement]::FromHandle($script:process.MainWindowHandle); if ($script:root) { return } }
    }
    throw 'Rooms UI fixture window missing.'
}
function Stop-App { if ($script:process -and -not $script:process.HasExited) { $null = $script:process.CloseMainWindow(); if (-not $script:process.WaitForExit(5000)) { $script:process.Kill() } } }
function Control([string]$Name) {
    $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    $found = $script:root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    if (-not $found) {
        $owned = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ProcessIdProperty, $script:process.Id)
        foreach ($window in [System.Windows.Automation.AutomationElement]::RootElement.FindAll([System.Windows.Automation.TreeScope]::Children, $owned)) {
            $found = $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition); if ($found) { break }
        }
    }
    $found
}
function Wait-Control([string]$Name) {
    for ($n = 0; $n -lt 75; $n++) { $element = Control $Name; if ($element -and $element.Current.IsEnabled) { return $element }; Start-Sleep -Milliseconds 200 }
    throw "Rooms UI control unavailable: $Name"
}
function Invoke-Control([string]$Name) { (Wait-Control $Name).GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() }
function Set-Text([string]$Name, [string]$Value) { (Wait-Control $Name).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value) }
function State { try { if (Test-Path $stateFile) { return Get-Content $stateFile -Raw | ConvertFrom-Json } } catch { }; return $null }
function Wait-Check([scriptblock]$Predicate, [string]$Detail) {
    for ($n = 0; $n -lt 75; $n++) { if (& $Predicate) { $checks.Add($Detail); return }; Start-Sleep -Milliseconds 200 }; throw $Detail
}
function Capture([string]$Name) {
    Start-Sleep -Milliseconds 300; $rect = $script:root.Current.BoundingRectangle
    $bitmap = [Drawing.Bitmap]::new([int]$rect.Width, [int]$rect.Height); $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try { $graphics.CopyFromScreen([int]$rect.Left, [int]$rect.Top, 0, 0, $bitmap.Size); $bitmap.Save((Join-Path $directory $Name), [Drawing.Imaging.ImageFormat]::Png) }
    finally { $graphics.Dispose(); $bitmap.Dispose() }
}
try {
    Start-App
    Invoke-Control 'Rooms'
    Wait-Check { $null -ne (Control 'This inbox is not an organization member inbox.') } 'A personal inbox is not treated as an organization member'
    Check ($null -ne (Control "/example/agent — opening an operated agent's rooms isn't available in this app yet.")) 'Delegated agents are listed but explicitly not opened'
    Check ($null -eq (Control 'New room')) 'No creation is offered for a personal inbox'
    Capture 'rooms-personal-inbox.png'
    Invoke-Control 'Switch to /preview/team'
    $null = Wait-Control 'Room Design studio, 1 member, Join'
    Check ($null -ne (Control 'Room Leadership, 1 member, Not available')) 'A private room without an invitation cannot be joined'
    Capture 'rooms-directory.png'
    Invoke-Control 'Room Design studio, 1 member, Join'
    $null = Wait-Control 'Join room'
    Check ($null -ne (Control 'Design studio')) 'Room details are reviewed before joining'
    Invoke-Control 'Join room'
    $null = Wait-Control 'Groups list'
    Wait-Check { $null -ne (Control 'Group messages') -and $null -eq (Control 'Group message') } 'A joined room opens in Groups and waits for keys without a composer'
    Capture 'rooms-joined-keying.png'

    Invoke-Control 'Rooms'
    Invoke-Control 'New room'
    Set-Text 'Room name' 'Launch plans'; Set-Text 'Room description' 'Synthetic launch room'
    Invoke-Control 'Create room'
    Wait-Check { $null -ne (Control 'Creation is not confirmed. Retry the same details or check your rooms.') } 'A lost committed creation reply is reported as unconfirmed'
    Check (@((State).attempts).Count -eq 1) 'The creation was sent once'
    $protected = @(Get-ChildItem $env:PIGEONPOST_UI_ROOMS_FOLDER -Recurse -File)
    Check ($protected.Count -gt 0 -and -not (@($protected | Where-Object { [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($_.FullName)).Contains('Launch plans') }).Count)) 'Windows protection keeps the room name out of the saved creation'
    Capture 'rooms-unconfirmed.png'
    Stop-App

    Start-App
    Invoke-Control 'Rooms'
    Invoke-Control 'Switch to /preview/team'
    Wait-Check { $null -ne (Control 'A room creation from earlier is not confirmed. Retry the same details, or discard it after checking your rooms.') } 'The protected creation is restored after restarting the app'
    Start-Sleep -Milliseconds 800
    Check (@((State).attempts).Count -eq 1) 'Restoring never resends the creation'
    Capture 'rooms-restored.png'
    Invoke-Control 'Retry the same details'
    $null = Wait-Control 'Group message'
    $attempts = @((State).attempts)
    Check ($attempts.Count -eq 2 -and @($attempts | Select-Object -ExpandProperty sha -Unique).Count -eq 1 -and @($attempts | Select-Object -ExpandProperty key -Unique).Count -eq 1) 'The explicit retry repeats the exact saved request'
    Check ($null -ne (Control 'Launch plans')) 'The created room opens as an encrypted group with its own composer'
    Wait-Check { @(Get-ChildItem $env:PIGEONPOST_UI_ROOMS_FOLDER -Recurse -File -Filter 'room.pending').Count -eq 0 } 'Confirmed creation clears protected custody'
    Capture 'rooms-created.png'
    @{ passed = $true; checks = $checks } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'rooms-ui.json')
} catch {
    try { if ($script:root) { Capture 'rooms-failure.png' } } catch { }
    @{ passed = $false; checks = $checks; error = $_.Exception.Message } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'rooms-ui.json')
    throw
} finally {
    Remove-Item Env:PIGEONPOST_UI_ROOMS_STATE, Env:PIGEONPOST_UI_ROOMS_FOLDER, Env:PIGEONPOST_UI_ROOMS_LOSSES -ErrorAction SilentlyContinue
    Stop-App
}
