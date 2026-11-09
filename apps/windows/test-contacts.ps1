#Requires -Version 7.0
# docs/contacts-and-phone.md §9 (Windows): Phone number, Find people I know from a contacts file, and one confirmed
# SMS invitation, against the stand-in postbox. Every number and name here is fictional.
param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Contacts UI fixtures require a disposable GitHub Windows runner.' }
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
$directory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$stateFile = Join-Path $directory 'contacts-state.json'
$contactsFile = Join-Path $directory 'contacts-fixture.vcf'
Remove-Item $stateFile -ErrorAction SilentlyContinue
@(
    'BEGIN:VCARD', 'VERSION:3.0', 'FN:Design Friend', 'TEL;TYPE=cell:+44 7700 900123', 'END:VCARD',
    'BEGIN:VCARD', 'VERSION:3.0', 'FN:Stranger', 'TEL;TYPE=cell:+1 555 555 0100', 'TEL;TYPE=work:0800-FLOWERS', 'END:VCARD'
) -join "`r`n" | Set-Content -NoNewline -Encoding utf8 $contactsFile
$env:PIGEONPOST_UI_CONTACTS_FILE = $contactsFile
$env:PIGEONPOST_UI_CONTACTS_STATE = $stateFile
$script:process = $null; $script:root = $null
$checks = [Collections.Generic.List[string]]::new()
function Check([bool]$Condition, [string]$Detail) { if (-not $Condition) { throw $Detail }; $checks.Add($Detail) }
function Start-App {
    $script:process = Start-Process -FilePath $Executable -PassThru; $script:root = $null
    for ($n = 0; $n -lt 60; $n++) {
        Start-Sleep -Milliseconds 500; $script:process.Refresh()
        if ($script:process.HasExited) { throw 'Contacts UI fixture exited during startup.' }
        if ($script:process.MainWindowHandle -ne 0) { $script:root = [System.Windows.Automation.AutomationElement]::FromHandle($script:process.MainWindowHandle); if ($script:root) { return } }
    }
    throw 'Contacts UI fixture window missing.'
}
function Stop-App { if ($script:process -and -not $script:process.HasExited) { $null = $script:process.CloseMainWindow(); if (-not $script:process.WaitForExit(5000)) { $script:process.Kill() } } }
function Find([System.Windows.Automation.Condition]$Condition) {
    $found = $script:root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $Condition)
    if (-not $found) {
        $owned = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ProcessIdProperty, $script:process.Id)
        foreach ($window in [System.Windows.Automation.AutomationElement]::RootElement.FindAll([System.Windows.Automation.TreeScope]::Children, $owned)) {
            $found = $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $Condition); if ($found) { break }
        }
    }
    $found
}
function Control([string]$Name) { Find ([System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name)) }
function Like([string]$Pattern) {
    $all = $script:root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    foreach ($e in $all) { if ($e.Current.Name -like $Pattern) { return $e } }
    $null
}
function Wait-Control([string]$Name) {
    for ($n = 0; $n -lt 75; $n++) { $element = Control $Name; if ($element -and $element.Current.IsEnabled) { return $element }; Start-Sleep -Milliseconds 200 }
    throw "Contacts UI control unavailable: $Name"
}
function Wait-Like([string]$Pattern, [string]$Detail) {
    for ($n = 0; $n -lt 75; $n++) { if (Like $Pattern) { $checks.Add($Detail); return }; Start-Sleep -Milliseconds 200 }
    throw $Detail
}
function Invoke-Control([string]$Name) { (Wait-Control $Name).GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 250 }
function Set-Text([string]$Name, [string]$Value) {
    # A dialog can share its title with its field, so only an editable control counts here.
    $field = [System.Windows.Automation.AndCondition]::new(
        [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name),
        [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Edit))
    for ($n = 0; $n -lt 75; $n++) {
        $element = Find $field
        if ($element -and $element.Current.IsEnabled) { $element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value); return }
        Start-Sleep -Milliseconds 200
    }
    throw "Contacts UI field unavailable: $Name"
}
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
    $null = Wait-Control 'Account'

    # Phone number: a wrong code is explained, the right one verifies, and discoverability starts off.
    Invoke-Control 'Account'
    Invoke-Control 'Phone number…'
    Set-Text 'Phone number' '+90 532 123 45 67'
    Invoke-Control 'Send code'
    Set-Text 'Verification code' '000000'
    Invoke-Control 'Verify'
    Wait-Like 'That code is not right. 4 tries left.' 'A wrong code says how many tries are left'
    Set-Text 'Verification code' '123456'
    Invoke-Control 'Verify'
    Wait-Like 'Your number is verified.' 'The right code verifies the number'
    $toggle = Wait-Control 'Let people who have my number find me'
    Check ($toggle.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Current.ToggleState -eq [System.Windows.Automation.ToggleState]::Off) 'Being findable by number stays off unless the person turns it on'
    Wait-Check { $s = State; $s -and $s.numbers -eq 2 -and $s.discoverable -eq 1 } 'Only the seeded contact is discoverable after verifying'
    Capture 'contacts-phone-verified.png'
    Invoke-Control 'Done'

    # Find people I know: one match, one "No match found", one unreadable number; only hashes are sent.
    Invoke-Control 'Account'
    Invoke-Control 'Find people I know…'
    Invoke-Control 'Choose a contacts file'
    $null = Wait-Control 'Start chat with Design Friend'
    Wait-Like '1 on Pigeonpost. 1 with no match found. Couldn*t read 1.' 'The summary counts matches, misses and unreadable numbers'
    Wait-Check { $s = State; $s -and (@($s.batches) -join ',') -eq '2' -and -not $s.raw_number_discovered } 'Discovery sent two hashes and no raw number'
    Check ($null -eq (Control 'Stranger') -or $null -ne (Control 'Invite Stranger by SMS')) 'An unmatched contact is offered an invitation'
    Capture 'contacts-found.png'

    # One invitation: the exact text is shown and nothing is sent until "Send invitation".
    Invoke-Control 'Invite Stranger by SMS'
    Wait-Like 'Pigeonpost: * invited you to chat on Pigeonpost. Join: https://pigeonpost.dev/invite?c=* Reply STOP to opt out.' 'The confirmation shows the exact message'
    Start-Sleep -Milliseconds 500
    Check (@((State).invites).Count -eq 0) 'Opening the confirmation sends nothing'
    Capture 'contacts-invite-confirm.png'
    Invoke-Control 'Send invitation to Stranger'
    Wait-Like 'Invitation to Stranger: Invitation sent.' 'The invitation result is shown on the row'
    Check (@((State).invites).Count -eq 1 -and (State).invites[0] -eq '+15555550100') 'Exactly one invitation was sent, for the one confirmed number'

    Invoke-Control 'Start chat with Design Friend'
    Wait-Check { $null -eq (Control 'Choose a contacts file') } 'Start chat closes the sheet'
    Capture 'contacts-started.png'
    @{ passed = $true; checks = $checks } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'contacts-ui.json')
} catch {
    try { if ($script:root) { Capture 'contacts-failure.png' } } catch { }
    @{ passed = $false; checks = $checks; error = $_.Exception.Message } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'contacts-ui.json')
    throw
} finally {
    Remove-Item Env:PIGEONPOST_UI_CONTACTS_FILE, Env:PIGEONPOST_UI_CONTACTS_STATE -ErrorAction SilentlyContinue
    Stop-App
}
