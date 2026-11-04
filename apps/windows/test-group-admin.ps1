#Requires -Version 7.0
# Group administration on Windows (audit 2026-11-03 P1-14, P3-6): join by /g/ link, who can join,
# invite link create/reset/off, members changing the timer, ban and unban, ownership transfer and
# stepping down. Two synthetic members key in with real MLS devices; the stand-in postbox writes its
# own view of the groups to the state file, which is what these checks read.
param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Group admin UI fixtures require a disposable GitHub Windows runner.' }
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
$directory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$stateFile = Join-Path $directory 'group-admin-state.json'
Remove-Item $stateFile -ErrorAction SilentlyContinue
$env:PIGEONPOST_UI_GROUP_ADMIN_STATE = $stateFile
$script:process = $null; $script:root = $null
$checks = [Collections.Generic.List[string]]::new()
function Check([bool]$Condition, [string]$Detail) { if (-not $Condition) { throw $Detail }; $checks.Add($Detail) }
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
    for ($n = 0; $n -lt 100; $n++) { $element = Control $Name; if ($element -and $element.Current.IsEnabled) { return $element }; Start-Sleep -Milliseconds 200 }
    throw "Group admin UI control unavailable: $Name"
}
function Invoke-Control([string]$Name) { (Wait-Control $Name).GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() }
function Set-Text([string]$Name, [string]$Value) { (Wait-Control $Name).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value) }
function Read-Text([string]$Name) { (Wait-Control $Name).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value }
function State { try { if (Test-Path $stateFile) { return Get-Content $stateFile -Raw | ConvertFrom-Json } } catch { }; return $null }
function Role([string]$Address) { $s = State; if ($s -and $s.group -and $s.group.roles.PSObject.Properties[$Address]) { return $s.group.roles.$Address }; return $null }
function Wait-Check([scriptblock]$Predicate, [string]$Detail) {
    for ($n = 0; $n -lt 150; $n++) { if (& $Predicate) { $checks.Add($Detail); return }; Start-Sleep -Milliseconds 200 }; throw $Detail
}
function Capture([string]$Name) {
    Start-Sleep -Milliseconds 300; $rect = $script:root.Current.BoundingRectangle
    $bitmap = [Drawing.Bitmap]::new([int]$rect.Width, [int]$rect.Height); $graphics = [Drawing.Graphics]::FromImage($bitmap)
    try { $graphics.CopyFromScreen([int]$rect.Left, [int]$rect.Top, 0, 0, $bitmap.Size); $bitmap.Save((Join-Path $directory $Name), [Drawing.Imaging.ImageFormat]::Png) }
    finally { $graphics.Dispose(); $bitmap.Dispose() }
}
# Group info is a sheet: each action closes it, then runs.
function Info-Action([string]$Name) { Invoke-Control 'Group info'; Invoke-Control $Name }
function Member-Action([string]$Member, [string]$Action) { Invoke-Control 'Group info'; Invoke-Control $Member; Invoke-Control $Action }
try {
    $script:process = Start-Process -FilePath $Executable -PassThru
    for ($n = 0; $n -lt 60; $n++) {
        Start-Sleep -Milliseconds 500; $script:process.Refresh()
        if ($script:process.HasExited) { throw 'Group admin UI fixture exited during startup.' }
        if ($script:process.MainWindowHandle -ne 0) { $script:root = [System.Windows.Automation.AutomationElement]::FromHandle($script:process.MainWindowHandle); if ($script:root) { break } }
    }
    if (-not $script:root) { throw 'Group admin UI fixture window missing.' }
    Wait-Check { $null -ne (State) } 'The stand-in postbox reports its groups'
    $helper = (State).helper; $third = (State).third

    # Join somebody else's public group by its /g/ link: the group is shown before joining.
    Invoke-Control 'Groups'
    Invoke-Control 'New group'
    Invoke-Control 'Join with a link…'
    Set-Text 'Group invite link' 'https://evil.example/g/grp_x#abcdefghijklmnopqrstuvwxyz'
    Start-Sleep -Milliseconds 300
    Check (-not (Control 'Open link').Current.IsEnabled) 'A link from anywhere else cannot be opened'
    Set-Text 'Group invite link' (State).studio_link
    Invoke-Control 'Open link'
    $null = Wait-Control 'Join Open studio?'
    Check ($null -ne (Control 'Join')) 'The group is previewed before anything is joined'
    Capture 'group-admin-join-preview.png'
    Invoke-Control 'Join'
    Wait-Check { $s = State; @($s.studio.roles.PSObject.Properties | Where-Object { $_.Value -eq 'member:keying' }).Count -eq 1 } 'Joining by link makes this inbox a member waiting for keys'
    $me = (@((State).studio.roles.PSObject.Properties | Where-Object { $_.Value -eq 'member:keying' }))[0].Name

    # Own group with two members who key in.
    Invoke-Control 'New group'
    Set-Text 'Group members' '/preview/helper, /preview/third'
    Set-Text 'Group name' 'Admin check'
    Invoke-Control 'Create'
    Wait-Check { (Role $helper) -eq 'member:active' -and (Role $third) -eq 'member:active' -and (Role $me) -eq 'owner:active' } 'Both members key in; this inbox owns the group'

    Info-Action 'Let anyone with the link join'
    Wait-Check { (State).group.join_policy -eq 'public' } 'Who can join: an admin makes the group public'
    Info-Action 'Create invite link'
    $first = Read-Text 'Invite link'
    $group = (State).group
    Check ($first -eq "https://inbox.pigeonpost.dev/g/$($group.id)#$($group.link_token)") 'Create link shows the postbox''s /g/ link once'
    Capture 'group-admin-link.png'
    Invoke-Control 'Done'
    Info-Action 'Reset invite link'
    $second = Read-Text 'Invite link'
    Check ($second -ne $first -and $second -like "https://inbox.pigeonpost.dev/g/$($group.id)#*") 'Reset makes a new link'
    Invoke-Control 'Done'
    Info-Action 'Turn off invite link'
    Wait-Check { -not (State).group.has_link } 'Turn off link stops it working'
    Info-Action 'Let members change the timer'
    Wait-Check { (State).group.members_can_change_timer -eq $true } 'Admins let members change the disappearing timer'

    Member-Action 'Actions for Preview Third' 'Remove and ban'
    Invoke-Control 'Ban'
    Wait-Check { (Role $third) -eq 'member:banned' -and @((State).group.bans) -contains $third } 'Ban removes the member and keeps them out'
    Invoke-Control 'Group info'
    $null = Wait-Control 'Banned (1)'
    Capture 'group-admin-banned.png'
    Invoke-Control 'Actions for Preview Third'; Invoke-Control 'Unban'
    Wait-Check { $null -eq (Role $third) -and @((State).group.bans).Count -eq 0 } 'Unban lifts the ban'

    Member-Action 'Actions for Preview Helper' 'Make admin'
    Wait-Check { (Role $helper) -eq 'admin:active' } 'The owner makes a member an admin'
    Info-Action 'Transfer ownership…'
    $null = Wait-Control 'New owner'
    Invoke-Control 'Transfer'
    Wait-Check { (Role $helper) -eq 'owner:active' -and (Role $me) -eq 'admin:active' } 'Ownership goes to the admin; the old owner stays an admin'
    Member-Action 'Your actions' 'Step down as admin'
    Invoke-Control 'Step down'
    Wait-Check { (Role $me) -eq 'member:active' } 'An admin steps down to member'
    Invoke-Control 'Group info'
    $null = Wait-Control 'Leave group'
    Check ($null -eq (Control 'Let anyone with the link join') -and $null -eq (Control 'Make the group invite only')) 'A member no longer sees admin settings'
    Capture 'group-admin-member.png'
    Invoke-Control 'Close'
    @{ passed = $true; checks = $checks } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'group-admin-ui.json')
} catch {
    try { if ($script:root) { Capture 'group-admin-failure.png' } } catch { }
    try { if (Test-Path $stateFile) { Copy-Item $stateFile (Join-Path $directory 'group-admin-state-at-failure.json') -Force } } catch { }
    @{ passed = $false; checks = $checks; error = $_.Exception.Message } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'group-admin-ui.json')
    throw
} finally {
    Remove-Item Env:PIGEONPOST_UI_GROUP_ADMIN_STATE -ErrorAction SilentlyContinue
    if ($script:process -and -not $script:process.HasExited) { $null = $script:process.CloseMainWindow(); if (-not $script:process.WaitForExit(5000)) { $script:process.Kill() } }
}
