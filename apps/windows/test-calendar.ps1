#Requires -Version 7.0
param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Calendar UI fixtures require a disposable GitHub Windows runner.' }
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
$directory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$stateFile = Join-Path $directory 'calendar-state.json'
$env:PIGEONPOST_UI_CALENDAR_STATE = $stateFile
$env:PIGEONPOST_UI_CALENDAR_FOLDER = (New-Item -ItemType Directory -Force (Join-Path $directory 'calendar-protected')).FullName
$env:PIGEONPOST_UI_CALENDAR_LOSSES = '2'
$process = $null; $root = $null
$checks = [Collections.Generic.List[string]]::new()
function Check([bool]$Condition, [string]$Detail) { if (-not $Condition) { throw $Detail }; $checks.Add($Detail) }
try {
    $process = Start-Process -FilePath $Executable -PassThru
    for ($n = 0; $n -lt 60; $n++) {
        Start-Sleep -Milliseconds 500; $process.Refresh()
        if ($process.HasExited) { throw 'Calendar UI fixture exited during startup.' }
        if ($process.MainWindowHandle -ne 0) { $root = [System.Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle); if ($root) { break } }
    }
    if (-not $root) { throw 'Calendar UI fixture window missing.' }
    function Control([string]$Name) {
        $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
        $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    }
    function Wait-Control([string]$Name) {
        for ($n = 0; $n -lt 60; $n++) { $element = Control $Name; if ($element -and $element.Current.IsEnabled) { return $element }; Start-Sleep -Milliseconds 200 }
        throw "Calendar UI control unavailable: $Name"
    }
    function Reveal($Element) {
        if (-not $Element.Current.IsOffscreen) { return }
        $pattern = $null
        if ($Element.TryGetCurrentPattern([System.Windows.Automation.ScrollItemPattern]::Pattern, [ref]$pattern)) { $pattern.ScrollIntoView(); return }
        $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::IsScrollPatternAvailableProperty, $true)
        foreach ($container in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)) {
            $scroll = $container.GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern)
            if (-not $scroll.Current.VerticallyScrollable) { continue }
            $scroll.SetScrollPercent(-1, 0)
            for ($n = 0; $n -lt 35 -and $Element.Current.IsOffscreen; $n++) { $scroll.Scroll([System.Windows.Automation.ScrollAmount]::NoAmount, [System.Windows.Automation.ScrollAmount]::SmallIncrement); Start-Sleep -Milliseconds 30 }
            if (-not $Element.Current.IsOffscreen) { return }
        }
    }
    function Invoke-Control([string]$Name) {
        $element = Wait-Control $Name; Reveal $element
        # Scrolling and a background refresh can replace a visual element. Act on the current
        # accessible control after revealing it, with the same name/enabled assertions.
        (Wait-Control $Name).GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    }
    function Set-Text([string]$Name, [string]$Value) { (Wait-Control $Name).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value) }
    function State { try { if (Test-Path $stateFile) { return Get-Content $stateFile -Raw | ConvertFrom-Json } } catch { }; return $null }
    function Wait-Check([scriptblock]$Predicate, [string]$Detail) {
        for ($n = 0; $n -lt 60; $n++) { if (& $Predicate) { $checks.Add($Detail); return }; Start-Sleep -Milliseconds 200 }; throw $Detail
    }
    function Capture([string]$Name) {
        Start-Sleep -Milliseconds 250; $rect = $root.Current.BoundingRectangle
        $bitmap = [Drawing.Bitmap]::new([int]$rect.Width, [int]$rect.Height); $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try { $graphics.CopyFromScreen([int]$rect.Left, [int]$rect.Top, 0, 0, $bitmap.Size); $bitmap.Save((Join-Path $directory $Name), [Drawing.Imaging.ImageFormat]::Png) }
        finally { $graphics.Dispose(); $bitmap.Dispose() }
    }
    Invoke-Control 'Calendar'; $null = Wait-Control 'New event'
    Check ($null -ne (Control 'Today')) 'Calendar navigation opens the month and day controls'
    Capture 'calendar-month.png'
    (Wait-Control 'Calendars').GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    Invoke-Control 'No calendars'
    Wait-Check { $null -eq (Control 'Open event Design review, 09:00–10:00') } 'Selecting no calendars hides the agenda instead of showing every calendar'
    Invoke-Control 'All calendars'; Invoke-Control 'Open event Design review, 09:00–10:00'; Invoke-Control 'Edit event'
    Set-Text 'Event title' 'Native calendar edited title'; Capture 'calendar-editor.png'; Invoke-Control 'Save changes'
    Wait-Check { $s = State; $s -and $s.edits -eq 1 -and @($s.events | Where-Object title -eq 'Native calendar edited title').Count -eq 1 } 'The actual editor submits one revisioned title change'
    Invoke-Control 'Open event Native calendar edited title, 09:00–10:00'; Invoke-Control 'Reminders'
    Set-Text 'Minutes before (separate with commas)' '5,15'; Invoke-Control 'Save'
    Wait-Check { $s = State; $s -and $s.reminderSaves -eq 1 } 'The reminders form saves personal reminder choices'
    Invoke-Control 'Open invitation Team invitation'; Invoke-Control 'Accept'
    Wait-Check { $s = State; $s -and $s.answers -eq 1 -and @($s.events | Where-Object { $_.id -eq 'evt_invitation' -and $_.rsvp -eq 'accepted' }).Count -eq 1 } 'An invitation accepts through the native detail controls without creating a contact'
    Invoke-Control 'Calendar settings'
    $previews = Wait-Control 'Show event details in reminder notifications'; $previews.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle()
    Invoke-Control 'Save'
    Wait-Check { $s = State; $s -and $s.settingsSaves -eq 1 -and -not $s.settings.previews } 'Calendar settings save the reminder-preview preference'
    Invoke-Control 'New event'; Set-Text 'Event title' 'Native pending event'
    Set-Text 'Start date (YYYY-MM-DD)' '2026-03-08'; Set-Text 'Last date (YYYY-MM-DD)' '2026-03-08'
    Set-Text 'Start time (HH:mm)' '02:30'; Set-Text 'End time (HH:mm)' '03:30'; Set-Text 'Time zone' 'America/New_York'; Invoke-Control 'Create event'
    Wait-Check { $null -ne (Control 'That local time does not exist because the clocks change. Choose another time.') -and (State).posts -eq 0 } 'The native event form refuses a nonexistent local time before posting'
    $day = (State).today
    Set-Text 'Start date (YYYY-MM-DD)' $day; Set-Text 'Last date (YYYY-MM-DD)' $day
    Set-Text 'Start time (HH:mm)' '09:00'; Set-Text 'End time (HH:mm)' '10:00'; Set-Text 'Time zone' 'Europe/Istanbul'; Invoke-Control 'Create event'
    Wait-Check { $s = State; $s -and $s.posts -eq 1 -and @($s.events | Where-Object title -eq 'Native pending event').Count -eq 1 } 'An unknown create response commits one fixture event and keeps the form open'
    Check ($null -ne (Control 'Event title')) 'An uncertain response preserves the visible event form'
    $protected = @(Get-ChildItem $env:PIGEONPOST_UI_CALENDAR_FOLDER -Recurse -File | Where-Object Length -gt 100)
    Check ($protected.Count -gt 0 -and -not (@($protected | Where-Object { [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($_.FullName)).Contains('Native pending event') }).Count)) 'Windows protection keeps the pending event title out of the saved file'
    Capture 'calendar-unknown-create.png'; Invoke-Control 'Cancel'; Invoke-Control 'Review pending event'
    $null = Wait-Control 'Retry saved request'; Check ($null -ne (Control 'Native pending event')) 'Recovery displays the exact original saved event for review'
    Capture 'calendar-recovery.png'; Invoke-Control 'Keep pending'; Start-Sleep -Milliseconds 1200
    Check ((State).posts -eq 1) 'Keeping an event pending never replays the request'
    Invoke-Control 'Review pending event'; Invoke-Control 'Retry saved request'
    Wait-Check { $s = State; $s -and $s.posts -eq 2 -and @($s.requests).Count -eq 1 -and @($s.events | Where-Object title -eq 'Native pending event').Count -eq 1 } 'Explicit recovery reuses the original request and confirms one event'
    Wait-Check { $null -eq (Control 'Review pending event') } 'Confirmed recovery removes its pending review entry'
    Invoke-Control 'New event'; Set-Text 'Event title' 'Native discarded event'; Invoke-Control 'Create event'
    Wait-Check { (State).posts -eq 3 } 'A second uncertain create remains a separate pending intent'
    Invoke-Control 'Cancel'; Invoke-Control 'Review pending event'; Invoke-Control 'Discard locally'; $null = Wait-Control 'Discard this saved request?'; Invoke-Control 'Discard locally'
    Wait-Check { $null -eq (Control 'Review pending event') } 'Explicit local discard removes only the saved request'
    Check ((State).posts -eq 3 -and @((State).events | Where-Object title -eq 'Native discarded event').Count -eq 1) 'Discard does not resend or delete the already created event'
    Invoke-Control 'Open event Native calendar edited title, 09:00–10:00'; Invoke-Control 'Cancel event'; $null = Wait-Control 'Cancel this event?'; Invoke-Control 'Cancel event'
    Wait-Check { $s = State; $s -and $s.cancels -eq 1 -and @($s.events | Where-Object { $_.id -eq 'evt_design' -and $_.status -eq 'cancelled' }).Count -eq 1 } 'Event cancellation requires its visible confirmation and reviewed revision'
    Capture 'calendar-complete.png'
    @{ passed = $true; checks = $checks } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'calendar-ui.json')
} catch {
    try { if ($root) { Capture 'calendar-failure.png' } } catch { }
    @{ passed = $false; checks = $checks; error = $_.Exception.Message } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'calendar-ui.json')
    throw
} finally {
    Remove-Item Env:PIGEONPOST_UI_CALENDAR_STATE, Env:PIGEONPOST_UI_CALENDAR_FOLDER, Env:PIGEONPOST_UI_CALENDAR_LOSSES -ErrorAction SilentlyContinue
    if ($process -and -not $process.HasExited) { $null = $process.CloseMainWindow(); if (-not $process.WaitForExit(5000)) { $process.Kill() } }
}
