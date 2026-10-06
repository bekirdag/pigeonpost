#Requires -Version 7.0
param([string]$Executable, [string]$PackageFamilyName, [Parameter(Mandatory)][string]$OutputDirectory, [switch]$Fixture)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing, System.Windows.Forms
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class NativeWindowBounds {
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint x, uint y, uint data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int width, int height, uint flags);
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr window, int attribute, out Rect value, int size);
}
'@
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
if ($Fixture) {
    $attachmentPath = Join-Path (Resolve-Path $OutputDirectory).Path 'attachment-only.txt'
    [IO.File]::WriteAllText($attachmentPath, ('Windows file bytes ' * 8192))
    $env:PIGEONPOST_UI_ATTACHMENT_FILE = $attachmentPath
    $downloadDirectory = Join-Path (Resolve-Path $OutputDirectory).Path 'downloads'
    New-Item -ItemType Directory -Force $downloadDirectory | Out-Null
    $env:PIGEONPOST_UI_DOWNLOAD_DIRECTORY = $downloadDirectory
    $env:PIGEONPOST_UI_ARRIVAL_FILE = Join-Path (Resolve-Path $OutputDirectory).Path 'arrival.signal'
    $secondAttachment = Join-Path (Resolve-Path $OutputDirectory).Path 'second-file.txt'
    [IO.File]::WriteAllText($secondAttachment, 'Second dropped file bytes')
}
if ($PackageFamilyName) {
    $before = @(Get-Process Pigeonpost -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id)
    Start-Process explorer.exe -ArgumentList "shell:AppsFolder\$PackageFamilyName!App"
    $process = $null
    for ($i = 0; $i -lt 40; $i++) {
        $process = Get-Process Pigeonpost -ErrorAction SilentlyContinue | Where-Object { $_.Id -notin $before } | Select-Object -First 1
        if ($process) { break }
        Start-Sleep -Milliseconds 500
    }
    if (-not $process) { throw 'Installed MSIX failed to activate.' }
} else {
    if (-not $Executable) { throw 'Executable or package family is required.' }
    Write-Host "Launching native test executable."
    $process = Start-Process -FilePath $Executable -PassThru
}
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
    Write-Host 'Native window is available.'
    $screen = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
    $null = [NativeWindowBounds]::SetWindowPos($process.MainWindowHandle, [IntPtr]::Zero, 12, 12, [Math]::Min(1440, $screen.Width - 24), [Math]::Min(900, $screen.Height - 24), 4)
    Start-Sleep -Milliseconds 500
    function Element([string]$Name) {
        $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
        $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    }
    function Wait-Element([string]$Name) {
        for ($n = 0; $n -lt 40; $n++) { $e = Element $Name; if ($e) { return $e }; Start-Sleep -Milliseconds 250 }
        throw "Native control missing: $Name"
    }
    function Wait-List([string]$Name) {
        $condition = [System.Windows.Automation.AndCondition]::new(
            [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name),
            [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::List))
        for ($n = 0; $n -lt 40; $n++) {
            $element = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
            if ($element) { return $element }
            Start-Sleep -Milliseconds 250
        }
        throw "Native list missing: $Name"
    }
    function Capture([string]$Name) {
        Start-Sleep -Milliseconds 500
        $rect = [NativeWindowBounds+Rect]::new()
        if ([NativeWindowBounds]::DwmGetWindowAttribute($process.MainWindowHandle, 9, [ref]$rect, 16) -ne 0) { throw 'Could not obtain visible native window bounds.' }
        $bitmap = [System.Drawing.Bitmap]::new($rect.Right - $rect.Left, $rect.Bottom - $rect.Top)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
            $bitmap.Save((Join-Path $OutputDirectory $Name), [System.Drawing.Imaging.ImageFormat]::Png)
        } finally { $graphics.Dispose(); $bitmap.Dispose() }
    }
    function Invoke-Control([string]$Name) {
        (Wait-Element $Name).GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 250
    }
    function Set-Text([string]$Name, [string]$Value) {
        (Wait-Element $Name).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value)
        Start-Sleep -Milliseconds 150
    }
    function Dialog-Button([string]$Id) {
        $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
        for ($n = 0; $n -lt 40; $n++) {
            $control = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
            if ($control -and $control.Current.IsEnabled) { return $control }
            Start-Sleep -Milliseconds 250
        }
        throw "Dialog button unavailable: $Id"
    }
    function Commit-Dialog([string]$ExpectedName) {
        $button = Dialog-Button 'PrimaryButton'
        if ($button.Current.Name -ne $ExpectedName) { throw "Expected dialog action $ExpectedName, got $($button.Current.Name)" }
        $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 500
    }
    function Open-Conversation([string]$Address) {
        Invoke-Control 'New conversation'
        $null = Wait-Element 'First message'
        Set-Text 'Pigeonpost address' $Address
        Commit-Dialog 'Open'
    }
    function Copy-Message([string]$Text) {
        $messages = Wait-List 'Messages'
        $bodyCondition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Text)
        $body = $messages.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $bodyCondition)
        if (-not $body) { throw "Message body missing for copy: $Text" }
        $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
        $row = $body
        while ($row -and $row.Current.ControlType -ne [System.Windows.Automation.ControlType]::ListItem) { $row = $walker.GetParent($row) }
        if (-not $row) { throw "Could not find message row for copy: $Text" }
        $condition = [System.Windows.Automation.AndCondition]::new(
            [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, 'Copy message'),
            [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button))
        $button = $row.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
        if (-not $button) { throw "Copy icon missing for message: $Text" }
        $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 200
        if ((Get-Clipboard -Raw).Trim() -ne $Text) { throw 'Copy icon did not copy the displayed message.' }
    }
    function Check-Download([string]$Name, [string]$Original) {
        $destination = Join-Path $downloadDirectory $Name
        Remove-Item $destination -ErrorAction SilentlyContinue
        Invoke-Control "Download $Name"
        $null = Wait-Element "Saved $Name."
        if ((Get-FileHash $destination).Hash -ne (Get-FileHash $Original).Hash) { throw "Attachment download changed bytes: $Name" }
    }
    function Drop-Files([string[]]$Files) {
        # Exercise OLE file drag-and-drop from another Windows app, rather than call the handler.
        $sourceScript = Join-Path $OutputDirectory 'drag-source.ps1'
        $sourceFiles = Join-Path $OutputDirectory 'drag-files.json'
        ConvertTo-Json -InputObject @($Files) | Set-Content $sourceFiles
        @'
param([string]$Files)
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$references = @((Get-ChildItem (Join-Path $PSHOME 'ref/*.dll')).FullName) + @(
    [System.Windows.Forms.Form].Assembly.Location, [System.Drawing.Bitmap].Assembly.Location,
    [Reflection.Assembly]::Load('System.Windows.Forms.Primitives').Location,
    [Reflection.Assembly]::Load('System.Private.Windows.Core').Location)
Add-Type -ReferencedAssemblies $references @"
using System;
using System.Drawing;
using System.Windows.Forms;
public static class NativeFileDragSource {
    public static void Run(string[] paths) {
        using (var form = new Form()) {
            form.Text = "Attachment drag source"; form.StartPosition = FormStartPosition.Manual;
            form.Location = new Point(1500, 50); form.Size = new Size(300, 150); form.TopMost = true;
            var label = new Label { Text = "Drag these files", Dock = DockStyle.Fill, TextAlign = ContentAlignment.MiddleCenter };
            label.MouseDown += (sender, args) => {
                Console.WriteLine("Drag source mouse down: " + paths.Length + " files");
                var data = new DataObject(DataFormats.FileDrop, paths);
                Console.WriteLine("Drag result: " + label.DoDragDrop(data, DragDropEffects.Copy));
            };
            form.Controls.Add(label);
            Application.Run(form);
        }
    }
}
"@
[NativeFileDragSource]::Run([string[]](Get-Content $Files -Raw | ConvertFrom-Json))
'@ | Set-Content $sourceScript
        $sourceLog = Join-Path $OutputDirectory 'drag-source.log'
        $sourceError = Join-Path $OutputDirectory 'drag-source-error.log'
        $sourceProcess = Start-Process pwsh -NoNewWindow -RedirectStandardOutput $sourceLog -RedirectStandardError $sourceError -ArgumentList @('-NoProfile', '-STA', '-File', "`"$sourceScript`"", '-Files', "`"$sourceFiles`"") -PassThru
        try {
            $sourceRoot = $null
            $sourceCondition = [System.Windows.Automation.AndCondition]::new(
                [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, 'Attachment drag source'),
                [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ProcessIdProperty, $sourceProcess.Id))
            for ($n = 0; $n -lt 50; $n++) {
                Start-Sleep -Milliseconds 200
                $sourceRoot = [System.Windows.Automation.AutomationElement]::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Children, $sourceCondition)
                if ($sourceRoot) { break }
            }
            if (-not $sourceRoot) { throw 'Drag source did not open.' }
            $rect = $sourceRoot.Current.BoundingRectangle
            $x = [int]($rect.Left + $rect.Width / 2); $y = [int]($rect.Top + $rect.Height / 2)
            $target = (Wait-List 'Messages').Current.BoundingRectangle
            $tx = [int]($target.Left + $target.Width / 2); $ty = [int]($target.Top + $target.Height / 2)
            $targetPoint = [NativeWindowBounds+Point]::new(); $targetPoint.X = $tx; $targetPoint.Y = $ty
            [uint32]$targetProcess = 0
            $null = [NativeWindowBounds]::GetWindowThreadProcessId([NativeWindowBounds]::WindowFromPoint($targetPoint), [ref]$targetProcess)
            Capture 'drag-ready.png'
            if ($targetProcess -ne $process.Id) { throw "Another window covers the drop target (process $targetProcess)." }
            Write-Host "Drag from ($x,$y) to ($tx,$ty), source window $($sourceRoot.Current.Name)"
            $null = [NativeWindowBounds]::SetCursorPos($x, $y)
            [NativeWindowBounds]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 200
            for ($step = 1; $step -le 20; $step++) {
                $null = [NativeWindowBounds]::SetCursorPos([int]($x + ($tx - $x) * $step / 20), [int]($y + ($ty - $y) * $step / 20))
                Start-Sleep -Milliseconds 50
            }
            Start-Sleep -Milliseconds 400
            [NativeWindowBounds]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
            $null = Wait-Element ('Remove ' + [IO.Path]::GetFileName($Files[0]))
        } finally {
            [NativeWindowBounds]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
            if (-not $sourceProcess.HasExited) { $null = $sourceProcess.CloseMainWindow(); if (-not $sourceProcess.WaitForExit(3000)) { $sourceProcess.Kill() } }
            foreach ($log in @($sourceLog, $sourceError)) { if (Test-Path $log) { Get-Content $log | Write-Host } }
        }
    }
    if ($Fixture) {
        Write-Host 'Checking composer attachments and downloads.'
        $null = Wait-Element 'New conversation'
        Set-Text 'Message' 'Please review the release checklist.'
        Capture 'inbox.png'
        Invoke-Control 'Send'
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne '') { throw 'Successful send did not clear composer.' }
        Invoke-Control 'Attach files…'
        $null = Wait-Element 'Remove attachment-only.txt'
        if (Element 'Download attachment-only.txt') { throw 'Picking a file sent it before Send.' }
        Capture 'attachment-without-message.png'
        Invoke-Control 'Send'
        $null = Wait-Element 'Download attachment-only.txt'
        if (Element 'Remove attachment-only.txt') { throw 'Successful send retained the attached draft file.' }
        Check-Download 'attachment-only.txt' $attachmentPath
        Capture 'attachment-delivered.png'
        Set-Text 'Message' 'Optional file caption'
        Invoke-Control 'Attach files…'
        $null = Wait-Element 'Remove attachment-only.txt'
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Optional file caption') { throw 'Attaching a file lost the text draft.' }
        Invoke-Control 'Send'
        $null = Wait-Element 'Optional file caption'
        Copy-Message 'Optional file caption'
        Invoke-Control 'Copy address'
        if ((Get-Clipboard -Raw).Trim() -ne '/preview/design') { throw 'Peer copy did not preserve the routing address.' }
        Invoke-Control 'Copy my address'
        if ((Get-Clipboard -Raw).Trim() -ne '/preview/main') { throw 'Own copy did not preserve the routing address.' }
        Invoke-Control 'Sender details'
        $null = Wait-Element 'Known sender'
        $null = Wait-Element 'Full permissions'
        $null = Wait-Element 'run_tests'
        Capture 'sender-details.png'
        Invoke-Control 'Cancel'
        Invoke-Control 'Sender address details'
        $null = Wait-Element 'Known sender'
        Invoke-Control 'Cancel'
        Set-Text 'Find in this subject' 'History message 1:'
        $null = Wait-Element '1 of 1'
        Copy-Message 'History message 1: desktop parity check.'
        Capture 'history-find.png'
        Set-Text 'Find in this subject' ''

        Write-Host 'Checking background refresh and scrolling.'
        Set-Text 'Find in this subject' 'History message 15:'
        $null = Wait-Element '1 of 1'
        Set-Text 'Find in this subject' ''
        $anchor = Wait-Element 'History message 15: desktop parity check.'
        $anchorTop = $anchor.Current.BoundingRectangle.Top
        $conversationSelection = (Wait-List 'Conversations').GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()[0]
        $conversationId = $conversationSelection.GetRuntimeId() -join ','
        $subjectSelection = (Wait-List 'Subjects').GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()[0]
        $subjectId = $subjectSelection.GetRuntimeId() -join ','
        Set-Text 'Message' 'Draft survives background checks'
        Start-Sleep -Seconds 3
        if ([Math]::Abs((Wait-Element 'History message 15: desktop parity check.').Current.BoundingRectangle.Top - $anchorTop) -gt 2) { throw 'An unchanged poll moved the history viewport.' }
        if ((((Wait-List 'Conversations').GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()[0].GetRuntimeId() -join ',') -ne $conversationId) -or (((Wait-List 'Subjects').GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()[0].GetRuntimeId() -join ',') -ne $subjectId)) { throw 'Polling recreated a selected menu row.' }
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Draft survives background checks') { throw 'Polling lost the draft.' }
        [IO.File]::WriteAllText($env:PIGEONPOST_UI_ARRIVAL_FILE, 'arrive')
        Start-Sleep -Seconds 2
        if ([Math]::Abs((Wait-Element 'History message 15: desktop parity check.').Current.BoundingRectangle.Top - $anchorTop) -gt 2) { throw 'New mail pulled the reader out of history.' }
        Capture 'refresh-preserves-history.png'
        $scroll = (Wait-List 'Messages').GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern)
        # WinUI estimates the height of unrealized, variable-height rows. A single UIA
        # percentage jump may stop short as more rows are measured; establish the actual end.
        for ($attempt = 0; $attempt -lt 10; $attempt++) {
            $scroll.SetScrollPercent(-1, 100)
            Start-Sleep -Milliseconds 250
            if ($scroll.Current.VerticalScrollPercent -ge 99.9) { break }
        }
        if ($scroll.Current.VerticalScrollPercent -lt 99.9) { throw 'Could not establish the end-of-history precondition.' }
        Write-Host "Before arrival: scroll $($scroll.Current.VerticalScrollPercent)%"
        [IO.File]::WriteAllText($env:PIGEONPOST_UI_ARRIVAL_FILE, 'arrive again')
        Start-Sleep -Seconds 2
        Write-Host "After arrival: scroll $($scroll.Current.VerticalScrollPercent)%"
        if ($scroll.Current.VerticalScrollPercent -lt 99) { throw 'A reader at the end did not follow the new message.' }
        Set-Text 'Message' ''

        Open-Conversation '/preview/team'
        Set-Text 'Message' 'Optional file caption'
        Write-Host 'Checking native file drag-and-drop.'
        Drop-Files @($attachmentPath, $secondAttachment)
        $null = Wait-Element 'Remove second-file.txt'
        if (Element 'Download second-file.txt') { throw 'Dropping files sent them before Send.' }
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Optional file caption') { throw 'Dropping files lost the existing message.' }
        Invoke-Control 'Remove attachment-only.txt'
        if (Element 'Remove attachment-only.txt') { throw 'Removing a staged file failed.' }
        Invoke-Control 'Attach files…'
        $null = Wait-Element 'Remove attachment-only.txt'
        Capture 'message-with-staged-files.png'
        # Files and text stay in this draft across conversation switches and background checks.
        Open-Conversation '/preview/design'
        if (Element 'Remove second-file.txt') { throw 'Staged files leaked to another conversation.' }
        Open-Conversation '/preview/team'
        $null = Wait-Element 'Remove second-file.txt'
        $null = Wait-Element 'Remove attachment-only.txt'
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Optional file caption') { throw 'Switching lost the file-message draft.' }
        Invoke-Control 'Send'
        $null = Wait-Element 'Download second-file.txt'
        if (Element 'Remove second-file.txt') { throw 'Successful send did not clear staged files.' }
        Check-Download 'attachment-only.txt' $attachmentPath
        Check-Download 'second-file.txt' $secondAttachment
        Capture 'sender-attachment-downloads.png'
        Invoke-Control 'Sender details'
        Invoke-Control 'Open this mailbox'
        Open-Conversation '/preview/main'
        Check-Download 'attachment-only.txt' $attachmentPath
        Check-Download 'second-file.txt' $secondAttachment
        Capture 'recipient-attachment-downloads.png'
        Invoke-Control 'Sender details'
        Invoke-Control 'Open this mailbox'
        Open-Conversation '/preview/design'

        Invoke-Control 'New conversation'
        Set-Text 'Pigeonpost address' '/preview/new-agent'
        Set-Text 'First message' 'First message from the native dialog.'
        Capture 'new-conversation.png'
        Commit-Dialog 'Send'
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne '') { throw 'First message was not sent.' }
        $null = Wait-Element 'First message from the native dialog.'

        # The details shortcut must close its native dialog before changing the mailbox.
        for ($cycle = 0; $cycle -lt 3; $cycle++) {
            Open-Conversation '/preview/team'
            Invoke-Control 'Sender details'
            $null = Wait-Element 'This is one of your mailboxes.'
            Invoke-Control 'Open this mailbox'
            $null = Wait-Element 'Operations'
            Set-Text 'Message' 'Draft in team mailbox'
            Open-Conversation '/preview/main'
            Invoke-Control 'Sender details'
            Invoke-Control 'Open this mailbox'
            Open-Conversation '/preview/design'
            Set-Text 'Message' 'Draft in main mailbox'
        }
        Capture 'repeated-mailbox-switch.png'
        Invoke-Control 'New subject'
        Set-Text 'Subject title' 'Native parity subject'
        Capture 'new-subject.png'
        Commit-Dialog 'Create'
        $null = Wait-Element 'Native parity subject'
        Invoke-Control 'Delete subject…'
        $null = Wait-Element 'Cancel'
        Capture 'delete-subject.png'
        Invoke-Control 'Cancel'
        $null = Wait-Element 'Native parity subject'
        Invoke-Control 'Delete subject…'
        Commit-Dialog 'Delete subject'
        if (Element 'Native parity subject') { throw 'Deleted subject remains visible.' }
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Draft in main mailbox') { throw 'Deleting a different subject lost the main draft.' }
    } else {
        $button = Wait-Element 'Sign in or create account'
        $null = Wait-Element 'Privacy policy'
        if (-not $button.Current.IsEnabled) { throw 'Sign-in button is not enabled.' }
        if (Element 'Could not access your saved account. Please try again.') { throw 'Windows credential vault could not be read.' }
        Capture 'welcome.png'
    }
    Write-Host 'Native Windows UI smoke passed.'
} catch {
    if ($root -and -not $process.HasExited) { Capture 'failure.png' }
    throw
} finally {
    if ($Fixture) { Remove-Item Env:PIGEONPOST_UI_ATTACHMENT_FILE, Env:PIGEONPOST_UI_DOWNLOAD_DIRECTORY, Env:PIGEONPOST_UI_ARRIVAL_FILE -ErrorAction SilentlyContinue }
    if (-not $process.HasExited) { $null = $process.CloseMainWindow(); if (-not $process.WaitForExit(5000)) { $process.Kill() } }
}
