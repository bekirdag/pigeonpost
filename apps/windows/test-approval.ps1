#Requires -Version 7.0
param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Machine approval UI fixtures require a disposable GitHub Windows runner.' }
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing, System.Windows.Forms
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class ApprovalInput {
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint x, uint y, uint data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
}
'@
$directory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$stateFile = Join-Path $directory 'approval-state.json'
$env:PIGEONPOST_UI_APPROVAL_STATE = $stateFile
$process = $null; $root = $null
$checks = [Collections.Generic.List[string]]::new()
function Check([bool]$Condition, [string]$Detail) { if (-not $Condition) { throw $Detail }; $checks.Add($Detail) }
try {
    $process = Start-Process -FilePath $Executable -PassThru
    for ($n = 0; $n -lt 60; $n++) {
        Start-Sleep -Milliseconds 500; $process.Refresh()
        if ($process.HasExited) { throw 'Approval UI fixture exited during startup.' }
        if ($process.MainWindowHandle -ne 0) { $root = [System.Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle); if ($root) { break } }
    }
    if (-not $root) { throw 'Approval UI fixture window missing.' }
    function Control([string]$Name) {
        $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
        $found = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
        if (-not $found) {
            # Content dialogs are hosted in a separate popup root of the same window.
            $desktop = [System.Windows.Automation.AutomationElement]::RootElement
            $process.Refresh()
            $owned = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ProcessIdProperty, $process.Id)
            foreach ($window in $desktop.FindAll([System.Windows.Automation.TreeScope]::Children, $owned)) {
                $found = $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition); if ($found) { break }
            }
        }
        $found
    }
    function Wait-Control([string]$Name) {
        for ($n = 0; $n -lt 75; $n++) { $element = Control $Name; if ($element -and $element.Current.IsEnabled) { return $element }; Start-Sleep -Milliseconds 200 }
        throw "Approval UI control unavailable: $Name"
    }
    function Invoke-Control([string]$Name) { (Wait-Control $Name).GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() }
    function Set-Text([string]$Name, [string]$Value) { (Wait-Control $Name).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value) }
    function Text-Value([string]$Name) { (Wait-Control $Name).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value }
    function State { try { if (Test-Path $stateFile) { return Get-Content $stateFile -Raw | ConvertFrom-Json } } catch { }; return $null }
    function Wait-Check([scriptblock]$Predicate, [string]$Detail) {
        for ($n = 0; $n -lt 75; $n++) { if (& $Predicate) { $checks.Add($Detail); return }; Start-Sleep -Milliseconds 200 }; throw $Detail
    }
    function Capture([string]$Name) {
        Start-Sleep -Milliseconds 300; $rect = $root.Current.BoundingRectangle
        $bitmap = [Drawing.Bitmap]::new([int]$rect.Width, [int]$rect.Height); $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try { $graphics.CopyFromScreen([int]$rect.Left, [int]$rect.Top, 0, 0, $bitmap.Size); $bitmap.Save((Join-Path $directory $Name), [Drawing.Imaging.ImageFormat]::Png) }
        finally { $graphics.Dispose(); $bitmap.Dispose() }
    }
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    function Hold-Pointer([string]$Name, [int]$Milliseconds) {
        $null = [ApprovalInput]::SetForegroundWindow($process.MainWindowHandle)
        $rect = (Wait-Control $Name).Current.BoundingRectangle
        $x = [int]($rect.Left + $rect.Width / 2); $y = [int]($rect.Top + $rect.Height / 2)
        [ApprovalInput]::mouse_event(0x8001, [uint32][Math]::Round($x * 65535 / ($bounds.Width - 1)), [uint32][Math]::Round($y * 65535 / ($bounds.Height - 1)), 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 150
        [ApprovalInput]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds $Milliseconds
        [ApprovalInput]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
    }

    # Unsent work in the inbox must survive the verified grant replacement later in this test.
    Set-Text 'Message' 'Unsent words before verification'
    Invoke-Control 'Machines'
    $null = Wait-Control 'Machine code'
    Set-Text 'Machine code' 'not a code'; Invoke-Control 'Review request'
    Wait-Check { $null -ne (Control "Enter the eight-character code shown on the machine.") -and (State) -eq $null } 'A malformed code is refused locally without a request'
    Set-Text 'Machine code' 'https://inbox.pigeonpost.dev/pair.html#abcd-efgh'; Invoke-Control 'Review request'
    $null = Wait-Control 'Hold to approve'
    Check ($null -ne (Control '/example/build')) 'The review names the agent handle prominently'
    Check ($null -ne (Control 'Approving creates this new agent in your organization.')) 'New-agent creation is stated explicitly'
    Check ($null -ne (Control 'Machine: Build server')) 'The machine label is shown'
    Check ($null -ne (Control "Requested access: Read and send this agent's messages")) 'Requested access is described'
    Check ($null -ne (Control 'Network location: TR. This comes from the network and is not proof of anything.')) 'The network hint states its limits'
    Check ($null -ne (Control 'Confirmation number 123 456')) 'The confirmation number is exposed to Narrator'
    Wait-Check { @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) | Where-Object { $_.Current.Name -like 'Expires in *' }).Count -eq 1 } 'An expiry countdown is shown'
    Check ((State).resolves -eq 1 -and (State).foreignActors -eq 0) 'One resolve ran inside the signed-in account actor'
    Capture 'approval-review.png'

    Hold-Pointer 'Hold to approve' 300
    Wait-Check { $null -ne (Control 'Keep holding for about 1.5 seconds to approve, or choose Approve with confirmation.') } 'A short press only explains the hold'
    Check ((State).approves -eq 0) 'A short press never approves'
    $button = Wait-Control 'Approve with confirmation…'
    $null = [ApprovalInput]::SetForegroundWindow($process.MainWindowHandle); $button.SetFocus(); Start-Sleep -Milliseconds 200
    [System.Windows.Forms.SendKeys]::SendWait(' ')
    $null = Wait-Control 'Approve this machine?'
    Capture 'approval-confirm-dialog.png'
    [System.Windows.Forms.SendKeys]::SendWait('{ESC}')
    Wait-Check { $null -eq (Control 'Approve this machine?') } 'The keyboard confirmation dialog closes with Escape'
    Start-Sleep -Milliseconds 600
    Check ((State).approves -eq 0) 'Cancelling the confirmation never approves'

    Invoke-Control 'Approve with confirmation…'; $null = Wait-Control 'Approve this machine?'; Invoke-Control 'Approve'
    $null = Wait-Control 'Verify sign-in in your browser'
    Check ((State).approves -eq 1 -and $null -ne (Control 'Verify your sign-in, then review and decide this request again.')) 'A strong-authentication refusal asks for verification instead of retrying'
    Check ($null -eq (Control 'Hold to approve') -or -not (Control 'Hold to approve').Current.IsEnabled -or (Control 'Hold to approve').Current.IsOffscreen) 'No approval control is offered while verification is required'
    Capture 'approval-verify.png'
    Invoke-Control 'Verify sign-in in your browser'
    Wait-Check { $s = State; $s -and $s.verifications -eq 1 -and $s.rotations -eq 1 -and $s.resolves -eq 2 } 'Verification replaces the grant and rereads the same code'
    $null = Wait-Control 'Hold to approve'
    Start-Sleep -Milliseconds 800
    Check ((State).approves -eq 1 -and (State).foreignActors -eq 0) 'Verification never approves by itself'
    Hold-Pointer 'Hold to approve' 2000
    Wait-Check { $s = State; $s -and $s.approves -eq 2 -and $s.states.ABCDEFGH -eq 'approved' } 'A deliberate pointer hold approves once under the new grant'
    Wait-Check { $null -ne (Control "The decision is not confirmed. Check the request's status before deciding again.") } 'A lost approval reply is reported as unconfirmed'
    Start-Sleep -Milliseconds 1500
    Check ((State).approves -eq 2) 'A lost reply is never resent automatically'
    Capture 'approval-unconfirmed.png'
    Invoke-Control 'Check status'
    Wait-Check { $null -ne (Control 'This machine is approved. It is signing in now.') } 'A fresh status check shows the committed approval'
    Check ((State).approves -eq 2) 'Checking status sends no decision'

    Invoke-Control 'Enter a different code'; Set-Text 'Machine code' 'WXYZ-2345'; Invoke-Control 'Review request'
    $null = Wait-Control 'Deny'
    Check ($null -ne (Control 'Existing agent · /k/rywv63mb6h0hhrpfjewrrttspw')) 'An existing agent shows its canonical inbox'
    Check ($null -ne (Control 'Confirmation number 654 321')) 'The second request shows its own confirmation'
    Capture 'approval-existing.png'
    Invoke-Control 'Deny'
    Wait-Check { $null -ne (Control 'This request was denied. The machine cannot use it.') } 'Denial is confirmed by a fresh resolve'
    Check ((State).denies -eq 1 -and (State).states.WXYZ2345 -eq 'denied' -and (State).approves -eq 2) 'Denial sends one decision and approves nothing'

    Invoke-Control 'Chats'
    Wait-Check { (Text-Value 'Message') -eq 'Unsent words before verification' } 'The unsent inbox draft survives the verified grant replacement'
    Capture 'approval-draft-kept.png'
    @{ passed = $true; checks = $checks } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'approval-ui.json')
} catch {
    try { if ($root) { Capture 'approval-failure.png' } } catch { }
    @{ passed = $false; checks = $checks; error = $_.Exception.Message } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'approval-ui.json')
    throw
} finally {
    Remove-Item Env:PIGEONPOST_UI_APPROVAL_STATE -ErrorAction SilentlyContinue
    if ($process -and -not $process.HasExited) { $null = $process.CloseMainWindow(); if (-not $process.WaitForExit(5000)) { $process.Kill() } }
}
