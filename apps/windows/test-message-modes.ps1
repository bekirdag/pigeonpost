#Requires -Version 7.0
param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'Message UI fixtures require a disposable GitHub Windows runner.' }
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class MotionPreference {
    [DllImport("user32.dll", SetLastError=true)] static extern bool SystemParametersInfo(uint action, uint param, ref bool value, uint update);
    [DllImport("user32.dll", SetLastError=true)] static extern bool SystemParametersInfo(uint action, uint param, IntPtr value, uint update);
    public static bool Get() { bool on = true; if (!SystemParametersInfo(0x1042, 0, ref on, 0)) throw new System.ComponentModel.Win32Exception(); return on; }
    public static void Set(bool on) { if (!SystemParametersInfo(0x1043, 0, (IntPtr)(on ? 1 : 0), 3)) throw new System.ComponentModel.Win32Exception(); }
}
'@
$directory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$stateFile = Join-Path $directory 'message-modes-state.json'
$env:PIGEONPOST_UI_MESSAGE_MODES_STATE = $stateFile
$env:PIGEONPOST_UI_MESSAGE_MODES_FOLDER = (New-Item -ItemType Directory -Force (Join-Path $directory 'message-modes-protected')).FullName
$process = $null; $root = $null; $motion = $null
$checks = [Collections.Generic.List[string]]::new()
function Check([bool]$Condition, [string]$Detail) { if (-not $Condition) { throw $Detail }; $checks.Add($Detail) }
try {
    # The gear turns only while Windows animation effects are on; start from on and restore afterwards.
    $motion = [MotionPreference]::Get(); [MotionPreference]::Set($true)
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
    function Wait-Prefix([string]$Prefix, [string]$Id) {
        $identity = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
        for ($n = 0; $n -lt 40; $n++) {
            foreach ($element in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $identity)) {
                if ($element.Current.Name.StartsWith($Prefix, [StringComparison]::Ordinal) -and -not $element.Current.IsOffscreen) { return $element }
            }
            Start-Sleep -Milliseconds 250
        }
        throw "Message UI control unavailable: $Prefix… $Id"
    }
    function Pixels($Element) {
        $rect = $Element.Current.BoundingRectangle
        $bitmap = [Drawing.Bitmap]::new([Math]::Max(1, [int]$rect.Width), [Math]::Max(1, [int]$rect.Height)); $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CopyFromScreen([int]$rect.Left, [int]$rect.Top, 0, 0, $bitmap.Size)
            $out = [Collections.Generic.List[Drawing.Color]]::new()
            for ($x = 0; $x -lt $bitmap.Width; $x++) { for ($y = 0; $y -lt $bitmap.Height; $y++) { $out.Add($bitmap.GetPixel($x, $y)) } }
            return , $out
        } finally { $graphics.Dispose(); $bitmap.Dispose() }
    }
    function Green-Count($Element) { @((Pixels $Element) | Where-Object { $_.G -gt $_.R + 40 -and $_.G -gt $_.B + 15 }).Count }
    function Red-Count($Element) { @((Pixels $Element) | Where-Object { $_.R -gt $_.G + 50 -and $_.R -gt $_.B + 20 }).Count }
    function Signature($Element) { ((Pixels $Element) | ForEach-Object { $_.ToArgb() }) -join ',' }
    function Select-Conversation([string]$Prefix) {
        $list = Wait-Control 'Conversations'
        $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
        for ($n = 0; $n -lt 40; $n++) {
            foreach ($element in $list.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)) {
                if (-not $element.Current.Name.StartsWith($Prefix, [StringComparison]::Ordinal)) { continue }
                $item = $element
                while ($item -and $item.Current.ControlType -ne [System.Windows.Automation.ControlType]::ListItem) { $item = $walker.GetParent($item) }
                if ($item) { $item.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select(); Start-Sleep -Milliseconds 600; return }
            }
            Start-Sleep -Milliseconds 250
        }
        throw "Conversation missing: $Prefix"
    }
    $null = Wait-Control 'Message'
    # §14C.7: the row sits under and outside the bubble, on the bubble's side, in the reference order.
    Check ($null -eq (Control 'Message actions')) 'No action menu button is drawn inside the bubbles'
    $sentEdit = (Wait-Control 'Edit message' 'design-2').Current.BoundingRectangle
    $sentCopy = (Wait-Control 'Copy message' 'design-2').Current.BoundingRectangle
    $sentMark = (Wait-Prefix 'Seen' 'design-2').Current.BoundingRectangle
    Check ($sentEdit.Right -le $sentCopy.Left + 1 -and [Math]::Abs($sentEdit.Top - $sentCopy.Top) -le 2) 'Sent messages offer edit then copy in one row'
    Check ($sentCopy.Top -ge $sentMark.Bottom + 4) 'The sent action row sits below the bubble footer, outside the bubble'
    Check ([Math]::Abs($sentCopy.Right - $sentMark.Right) -le 40) 'The sent action row is aligned to the right edge of its bubble'
    $receivedCopy = (Wait-Control 'Copy message' 'design-3').Current.BoundingRectangle
    $receivedReply = (Wait-Control 'Reply' 'design-3').Current.BoundingRectangle
    $receivedBody = (Wait-Control "Thanks. Let's keep the conversation easy to follow and make files simple to share.").Current.BoundingRectangle
    Check ($receivedCopy.Right -le $receivedReply.Left + 1 -and [Math]::Abs($receivedCopy.Top - $receivedReply.Top) -le 2) 'Received messages offer copy then reply in one row'
    Check ($receivedCopy.Top -ge $receivedBody.Bottom + 12) 'The received action row sits below the bubble, outside it'
    Check ([Math]::Abs($receivedCopy.Left - $receivedBody.Left) -le 40) 'The received action row is aligned to the left edge of its bubble'
    Check ($null -eq (Control 'Edit message' 'design-3') -and $null -eq (Control 'Reply' 'design-2')) 'Received messages have no edit and sent messages no reply in the row'
    Check ((Green-Count (Wait-Prefix 'Seen' 'design-2')) -gt 3) 'Seen ticks are green'
    Capture 'message-actions-row.png'
    Set-Text 'Ordinary native draft'
    Invoke-Control 'Reply' 'design-3'
    Check ((Read-Text) -eq 'Ordinary native draft' -and $null -ne (Control 'Replying to message')) 'Reply shows its original quoted context and preserves the ordinary draft'
    Invoke-Control 'Cancel message action'
    Check ((Read-Text) -eq 'Ordinary native draft') 'Cancelling reply preserves the existing text'
    Invoke-Control 'Edit message' 'design-2'
    Wait-Check { $null -ne (Control 'Editing message') -and (Read-Text) -eq "I'll check the layout, keyboard shortcuts, and how it works in a smaller window." } 'Edit is offered on the owned recent message and prefills its displayed text'
    Check (-not (Control 'Attach files…').Current.IsEnabled) 'Edit disables file attachment controls'
    Set-Text 'Cancelled native edit'; Invoke-Control 'Cancel message action'
    Check ((Read-Text) -eq 'Ordinary native draft') 'Cancelling edit restores the previous ordinary draft'
    Invoke-Control 'Edit message' 'design-2'
    Set-Text 'Edited via native message controls'
    Capture 'message-edit.png'
    Invoke-Control 'Save edit'
    Wait-Check { $s = State; $s -and $s.edits -eq 1 -and $s.originalEditParent -and (Read-Text) -eq 'Ordinary native draft' } 'Save edit uses the original owned copy, text only, and restores the ordinary draft'
    (Wait-Prefix 'Seen' 'design-2').GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    $content = Wait-Control 'Message content'
    Check ($content.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.IsReadOnly) 'Message info opens from the receipt marks and its content is read-only'
    Check ($content.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -eq 'Edited via native message controls') 'Message info displays the folded current text'
    $version = Wait-Control 'Message version'
    Check ($version.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.IsReadOnly) 'Version history remains read-only data'
    Capture 'message-info.png'; Invoke-Control 'Done'
    Wait-Check { $null -eq (Control 'Message content') } 'Message info closes'
    # A row action pressed while the dialog is still closing is ignored; press until reply mode shows.
    $replying = $false
    for ($n = 0; $n -lt 10 -and -not $replying; $n++) {
        Invoke-Control 'Reply' 'design-3'
        for ($m = 0; $m -lt 10 -and -not $replying; $m++) { Start-Sleep -Milliseconds 200; $replying = $null -ne (Control 'Replying to message') }
    }
    Check $replying 'Reply from the row enters reply mode after message info closes'
    Set-Text 'Reply via native message controls'; Invoke-Control 'Send'
    Wait-Check { $s = State; $s -and $s.replies -eq 1 -and $s.originalReplyParent -and (Read-Text) -eq '' } 'Native reply sends the original parent and bounded quote and clears the confirmed reply draft'
    $state = State
    Check ($state.posts -eq 2 -and $state.committed -eq 2 -and $state.infoReads -eq 1) 'Cancelled modes never send and message info only reads'
    # §14C.6: every receipt state, with accessible names, colours and the turning gear.
    function Reveal([string]$Prefix, [string]$Id) {
        # Bring the message row into view, then return its receipt marks.
        $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
        $identity = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
        for ($n = 0; $n -lt 40; $n++) {
            foreach ($element in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $identity)) {
                if (-not $element.Current.Name.StartsWith($Prefix, [StringComparison]::Ordinal)) { continue }
                $item = $element
                while ($item -and $item.Current.ControlType -ne [System.Windows.Automation.ControlType]::ListItem) { $item = $walker.GetParent($item) }
                if ($item) { try { $item.GetCurrentPattern([System.Windows.Automation.ScrollItemPattern]::Pattern).ScrollIntoView() } catch { } }
                Start-Sleep -Milliseconds 300
                return Wait-Prefix $Prefix $Id
            }
            Start-Sleep -Milliseconds 250
        }
        throw "Receipt marks missing: $Prefix $Id"
    }
    Select-Conversation 'Build agent'
    $sent = Reveal 'Sent' 'agent-1'; $delivered = Reveal 'Delivered' 'agent-2'
    Check ($sent.Current.Name -eq 'Sent' -and $delivered.Current.Name -eq 'Delivered') 'Sent and delivered marks carry their accessible names'
    Check ((Green-Count $sent) -eq 0 -and (Green-Count $delivered) -eq 0) 'Sent and delivered ticks are gray'
    Check ($delivered.Current.BoundingRectangle.Width -gt $sent.Current.BoundingRectangle.Width + 2) 'Delivered shows two ticks where sent shows one'
    Capture 'message-receipts-sent-delivered.png'
    $seen = Reveal 'Seen ' 'agent-3'; $working = Reveal 'Agent working on this' 'agent-4'; $failed = Reveal 'Agent run failed' 'agent-5'
    Check ((Green-Count $seen) -gt 3) 'Seen ticks are green and labelled with the seen time'
    Check ((Green-Count $working) -gt 3) 'The working gear is green'
    Check ((Red-Count $failed) -gt 3) 'A failed agent run shows a red warning'
    $turning = $false
    for ($n = 0; $n -lt 6 -and -not $turning; $n++) { $a = Signature $working; Start-Sleep -Milliseconds 350; $turning = (Signature $working) -ne $a }
    Check $turning 'The gear turns while the agent works'
    $gearWidth = $working.Current.BoundingRectangle.Width
    Capture 'message-receipts.png'
    [MotionPreference]::Set($false)
    Select-Conversation 'Design'; Select-Conversation 'Build agent'
    $working = Reveal 'Agent working on this' 'agent-4'
    Wait-Check { $working.Current.BoundingRectangle.Width -gt $gearWidth + 20 } 'With animation effects off, the gear is joined by the word Working…'
    $still = $true
    for ($n = 0; $n -lt 4; $n++) { $a = Signature $working; Start-Sleep -Milliseconds 350; if ((Signature $working) -ne $a) { $still = $false } }
    Check $still 'With animation effects off, the gear stands still'
    Capture 'message-receipts-reduced-motion.png'
    [MotionPreference]::Set($true)
    Capture 'message-modes-complete.png'
    @{ passed = $true; checks = $checks } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'message-modes-ui.json')
} catch {
    try { if ($root) { Capture 'message-modes-failure.png' } } catch { }
    @{ passed = $false; checks = $checks; error = $_.Exception.Message } | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $directory 'message-modes-ui.json')
    throw
} finally {
    if ($null -ne $motion) { try { [MotionPreference]::Set($motion) } catch { } }
    Remove-Item Env:PIGEONPOST_UI_MESSAGE_MODES_STATE, Env:PIGEONPOST_UI_MESSAGE_MODES_FOLDER -ErrorAction SilentlyContinue
    if ($process -and -not $process.HasExited) { $null = $process.CloseMainWindow(); if (-not $process.WaitForExit(5000)) { $process.Kill() } }
}
