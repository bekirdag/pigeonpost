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
    $env:PIGEONPOST_UI_THEME_FILE = Join-Path (Resolve-Path $OutputDirectory).Path 'theme.signal'
    $env:PIGEONPOST_UI_SCROLL_LOG = Join-Path (Resolve-Path $OutputDirectory).Path 'scroll.log'
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
    function Assert-DesktopColumns {
        $conversations = Wait-List 'Conversations'
        $subjects = Wait-List 'Subjects'
        $messages = Wait-List 'Messages'
        if ($conversations.Current.IsOffscreen -or $subjects.Current.IsOffscreen -or $messages.Current.IsOffscreen) {
            throw 'All three desktop columns must be visible.'
        }
        $left = $conversations.Current.BoundingRectangle
        $middle = $subjects.Current.BoundingRectangle
        $right = $messages.Current.BoundingRectangle
        if ($left.Right -gt $middle.Left -or $middle.Right -gt $right.Left) {
            throw 'Conversation, subject and message columns overlap or appear out of order.'
        }
        if ($middle.Width -lt 120 -or $right.Width -lt 280 -or $right.Right -gt $root.Current.BoundingRectangle.Right) {
            throw 'The subjects or messages column is clipped or too narrow to use.'
        }
    }
    function Capture([string]$Name) {
        $null = [NativeWindowBounds]::SetCursorPos($screen.Right - 2, $screen.Bottom - 2)
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
    function Select-PartialText([string]$Text) {
        # Drag across the start of a message body with the real mouse, as a reader would,
        # then copy with the keyboard. The row must not swallow the drag or the focus.
        $messages = Wait-List 'Messages'
        $bodyCondition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Text)
        $body = $messages.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $bodyCondition)
        if (-not $body) { throw "Message body missing for selection: $Text" }
        $rect = $body.Current.BoundingRectangle
        $y = [int]($rect.Top + [Math]::Min(10, $rect.Height / 2)); $x = [int]$rect.Left + 1; $end = [int]$rect.Left + 45
        Set-Clipboard 'selection-sentinel'
        # Absolute moves produce real pointer updates; zero-delta relative moves can be dropped.
        $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
        function Move-Mouse([int]$X, [int]$Y) {
            [NativeWindowBounds]::mouse_event(0x8001, [uint32][Math]::Round($X * 65535 / ($bounds.Width - 1)), [uint32][Math]::Round($Y * 65535 / ($bounds.Height - 1)), 0, [UIntPtr]::Zero)
        }
        Move-Mouse $x $y
        Start-Sleep -Milliseconds 150
        [NativeWindowBounds]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 150
        for ($step = 1; $step -le 15; $step++) {
            Move-Mouse ([int]($x + ($end - $x) * $step / 15)) $y
            Start-Sleep -Milliseconds 30
        }
        Start-Sleep -Milliseconds 100
        [NativeWindowBounds]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 300
        try {
            $selection = $body.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).GetSelection()
            Write-Host "Selection after drag: '$(if ($selection.Length) { $selection[0].GetText(200) })'"
        } catch { Write-Host "Text pattern unavailable: $($_.Exception.Message)" }
        $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
        if ($focused) { Write-Host "Focused after drag: $($focused.Current.ControlType.ProgrammaticName) '$($focused.Current.Name)'" }
        Write-Host "Drag from ($x,$y) to ($end,$y) over body $rect"
        [System.Windows.Forms.SendKeys]::SendWait('^c')
        Start-Sleep -Milliseconds 300
        $copied = Get-Clipboard -Raw
        Write-Host "Partial selection copied: '$copied'"
        if (-not $copied -or $copied -eq 'selection-sentinel' -or $copied.Length -ge $Text.Length -or -not $Text.StartsWith($copied)) {
            Capture 'partial-selection-failed.png'
            throw "Dragging over a message did not copy part of its text (got '$copied')."
        }
        Capture 'partial-selection.png'
        # Double-clicking a word selects just that word, again without the row taking over.
        Set-Clipboard 'selection-sentinel'
        Move-Mouse ($x + 20) $y
        Start-Sleep -Milliseconds 150
        foreach ($click in 1..2) {
            [NativeWindowBounds]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
            [NativeWindowBounds]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 60
        }
        Start-Sleep -Milliseconds 400
        [System.Windows.Forms.SendKeys]::SendWait('^c')
        Start-Sleep -Milliseconds 300
        $word = Get-Clipboard -Raw
        Write-Host "Double-click copied: '$word'"
        if (-not $word -or $word.Trim() -ne $Text.Split(' ')[0]) {
            Capture 'word-selection-failed.png'
            throw "Double-clicking a word did not select it (got '$word')."
        }
        # Right-click on selected text offers the text menu; without a selection, the message menu.
        [NativeWindowBounds]::mouse_event(8, 0, 0, 0, [UIntPtr]::Zero)
        [NativeWindowBounds]::mouse_event(0x10, 0, 0, 0, [UIntPtr]::Zero)
        $null = Wait-Element 'Copy'
        if (Element 'Copy original message') { throw 'Right-clicking selected text showed the message menu instead of Copy.' }
        [System.Windows.Forms.SendKeys]::SendWait('{ESC}')
        Start-Sleep -Milliseconds 300
        Move-Mouse ($x + 80) $y
        [NativeWindowBounds]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
        [NativeWindowBounds]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 400
        [NativeWindowBounds]::mouse_event(8, 0, 0, 0, [UIntPtr]::Zero)
        [NativeWindowBounds]::mouse_event(0x10, 0, 0, 0, [UIntPtr]::Zero)
        $null = Wait-Element 'Copy original message'
        Capture 'message-menu-over-text.png'
        [System.Windows.Forms.SendKeys]::SendWait('{ESC}')
        Start-Sleep -Milliseconds 300
    }
    function Sample-Luminance([double]$XFraction, [int]$FromTop, [int]$FromBottom) {
        $null = [NativeWindowBounds]::SetCursorPos($screen.Right - 2, $screen.Bottom - 2)
        Start-Sleep -Milliseconds 300
        $rect = [NativeWindowBounds+Rect]::new()
        if ([NativeWindowBounds]::DwmGetWindowAttribute($process.MainWindowHandle, 9, [ref]$rect, 16) -ne 0) { throw 'Could not obtain window bounds.' }
        $x = [int]($rect.Left + ($rect.Right - $rect.Left) * $XFraction)
        $y = if ($FromTop -ge 0) { $rect.Top + $FromTop } else { $rect.Bottom - $FromBottom }
        $bitmap = [System.Drawing.Bitmap]::new(1, 1)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try { $graphics.CopyFromScreen($x, $y, 0, 0, $bitmap.Size); $c = $bitmap.GetPixel(0, 0) }
        finally { $graphics.Dispose(); $bitmap.Dispose() }
        return [int](0.2126 * $c.R + 0.7152 * $c.G + 0.0722 * $c.B)
    }
    function Choose-Theme([string]$Name) {
        Invoke-Control 'Account'
        $menu = Wait-Element 'Theme'
        try { $menu.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand() }
        catch { $menu.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() }
        Start-Sleep -Milliseconds 400
        $item = Wait-Element $Name
        try { $item.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() }
        catch {
            try { $item.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select() }
            catch { $item.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle() }
        }
        Start-Sleep -Milliseconds 700
        if (Element 'Copy my address') { [System.Windows.Forms.SendKeys]::SendWait('{ESC}'); Start-Sleep -Milliseconds 300 }
        if (Element 'Copy my address') { [System.Windows.Forms.SendKeys]::SendWait('{ESC}'); Start-Sleep -Milliseconds 300 }
    }
    function Assert-Theme([string]$Expected, [string]$Stored) {
        $body = Sample-Luminance 0.5 -1 22
        $caption = Sample-Luminance 0.5 15 0
        Write-Host "Theme $Expected`: account bar luminance $body, title bar luminance $caption"
        $dark = $Expected -eq 'Dark'
        if (($dark -and ($body -gt 60 -or $caption -gt 60)) -or (-not $dark -and ($body -lt 190 -or $caption -lt 190))) {
            Capture "theme-$($Expected.ToLowerInvariant())-failed.png"
            throw "The window or its title bar did not switch to the $Expected theme."
        }
        $file = Join-Path $env:LOCALAPPDATA 'Pigeonpost\preferences.json'
        if (-not (Test-Path $file) -or -not (Get-Content $file -Raw).Contains("`"theme`":`"$Stored`"")) { throw "The $Stored theme preference was not saved." }
    }
    function Check-Download([string]$Name, [string]$Original) {
        $destination = Join-Path $downloadDirectory $Name
        Remove-Item $destination -ErrorAction SilentlyContinue
        Invoke-Control "Download $Name"
        $null = Wait-Element "Saved $Name."
        if ((Get-FileHash $destination).Hash -ne (Get-FileHash $Original).Hash) { throw "Attachment download changed bytes: $Name" }
    }
    function Wait-Like([string]$Pattern) {
        for ($n = 0; $n -lt 40; $n++) {
            $match = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) | Where-Object { $_.Current.Name -like $Pattern })
            if ($match.Count -gt 0) { return $match[0] }
            Start-Sleep -Milliseconds 250
        }
        throw "Native control missing: $Pattern"
    }
    function Assert-ComposerFocused([string]$How) {
        for ($n = 0; $n -lt 20; $n++) {
            $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
            if ($focused -and $focused.Current.Name -eq 'Message') { return }
            Start-Sleep -Milliseconds 150
        }
        throw "The composer lost focus after sending with $How (focused: $(if ($focused) { $focused.Current.Name }))."
    }
    function Wait-ComposerEmpty {
        for ($n = 0; $n -lt 40; $n++) { if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -eq '') { return }; Start-Sleep -Milliseconds 150 }
        $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
        Capture 'composer-send-failure.png'
        throw "The message was not sent (composer: '$((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value)', focused: $(if ($focused) { $focused.Current.Name }))."
    }
    function Open-Viewer([string]$Name, [string]$Content, [string]$Shot) {
        Invoke-Control "Open $Name"
        $null = Wait-Element 'Close viewer'
        $null = Wait-Element $Content
        Start-Sleep -Milliseconds 600
        Capture $Shot
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
            label.QueryContinueDrag += (sender, args) => Console.WriteLine("Continue drag: " + args.KeyState + " " + args.Action + " at " + Control.MousePosition);
            label.GiveFeedback += (sender, args) => Console.WriteLine("Drag feedback: " + args.Effect + " at " + Control.MousePosition);
            label.MouseDown += (sender, args) => {
                Console.WriteLine("Drag source mouse down: " + paths.Length + " files");
                var data = new DataObject(DataFormats.FileDrop, paths);
                Console.WriteLine("Drag result: " + label.DoDragDrop(data, DragDropEffects.Copy));
            };
            form.Controls.Add(label);
            using (var probe = new Form { Text = "Native drag probe", StartPosition = FormStartPosition.Manual,
                Location = new Point(1500, 300), Size = new Size(300, 150), TopMost = true, AllowDrop = true }) {
                probe.DragEnter += (sender, args) => args.Effect = args.Data.GetDataPresent(DataFormats.FileDrop) ? DragDropEffects.Copy : DragDropEffects.None;
                probe.DragDrop += (sender, args) => Console.WriteLine("Probe received: " + ((string[])args.Data.GetData(DataFormats.FileDrop)).Length);
                form.Shown += (sender, args) => probe.Show(form);
                Application.Run(form);
            }
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
            # Confirm that the runner can deliver a native file drag before testing WinUI.
            $probeCondition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, 'Native drag probe')
            $probe = $null
            for ($n = 0; $n -lt 30; $n++) {
                $probe = [System.Windows.Automation.AutomationElement]::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $probeCondition)
                if ($probe) { break }
                Start-Sleep -Milliseconds 200
            }
            if (-not $probe) { throw 'Native drag probe did not open.' }
            $probeRect = $probe.Current.BoundingRectangle
            $destinations = @(@([int]($probeRect.Left + $probeRect.Width / 2), [int]($probeRect.Top + $probeRect.Height / 2)), @($tx, $ty))
            foreach ($destination in $destinations) {
            $tx = $destination[0]; $ty = $destination[1]
            Write-Host "Drag from ($x,$y) to ($tx,$ty), source window $($sourceRoot.Current.Name)"
            $null = [NativeWindowBounds]::SetCursorPos($x, $y)
            [NativeWindowBounds]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 200
            for ($step = 1; $step -le 20; $step++) {
                $null = [NativeWindowBounds]::SetCursorPos([int]($x + ($tx - $x) * $step / 20), [int]($y + ($ty - $y) * $step / 20))
                [NativeWindowBounds]::mouse_event(1, 0, 0, 0, [UIntPtr]::Zero)
                Start-Sleep -Milliseconds 50
            }
            Start-Sleep -Milliseconds 400
            [NativeWindowBounds]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 500
            if (-not (Get-Content $sourceLog -Raw).Contains("Probe received: $($Files.Count)")) { throw 'The runner failed to deliver a native drag to the control probe.' }
            }
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
        if (Element 'Find in this subject') { throw 'Search should be closed initially.' }
        Assert-DesktopColumns
        foreach ($call in @('Voice call', 'Video call')) { if (-not (Wait-Element $call).Current.IsEnabled) { throw "$call is unavailable in an open conversation." } }
        $pin = (Wait-Element 'Pin subjects').GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
        if ($pin.Current.ToggleState -ne [System.Windows.Automation.ToggleState]::On) { throw 'Subjects must be pinned by default.' }
        $initialScroll = (Wait-List 'Messages').GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern)
        for ($n = 0; $n -lt 40 -and $initialScroll.Current.VerticalScrollPercent -lt 95; $n++) { Start-Sleep -Milliseconds 100 }
        if ($initialScroll.Current.VerticalScrollPercent -lt 95) { throw 'Initial conversation did not open at its newest messages.' }
        Set-Text 'Message' 'Please review the release checklist.'
        Capture 'inbox.png'
        $null = Wait-Element 'Seen'
        $quotedName = 'Open quoted message: I''ve organized the feedback for the next release. Can you review the desktop layout?'
        Invoke-Control $quotedName
        $parentSelected = $false
        for ($n = 0; $n -lt 40; $n++) {
            $selectedRows = (Wait-List 'Messages').GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern).GetCurrentSelection()
            if ($selectedRows.Count -eq 1) {
                $parentCopy = $selectedRows[0].FindFirst([System.Windows.Automation.TreeScope]::Descendants,
                    [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'design-1'))
                if ($parentCopy) { $parentSelected = $true; break }
            }
            Start-Sleep -Milliseconds 100
        }
        if (-not $parentSelected) { throw 'The quote did not navigate to its original message.' }
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Please review the release checklist.') {
            throw 'Quoted-parent navigation changed the draft.'
        }
        Capture 'quoted-parent.png'
        (Wait-List 'Messages').GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern).SetScrollPercent(-1, 100)
        Start-Sleep -Milliseconds 500
        $pin.Toggle()
        Start-Sleep -Milliseconds 500
        if (Element 'Subjects') { throw 'Unpinning did not hide the subjects pane.' }
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Please review the release checklist.') { throw 'Hiding subjects lost the draft.' }
        (Wait-Element 'Pin subjects').GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle()
        Assert-DesktopColumns
        Capture 'pinned-subjects.png'
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

        Write-Host 'Checking the in-app attachment viewer.'
        Open-Viewer 'attachment-only.txt' 'Contents of attachment-only.txt' 'viewer-text.png'
        $saved = Join-Path $downloadDirectory 'attachment-only.txt'
        Remove-Item $saved -ErrorAction SilentlyContinue
        Invoke-Control 'Save attachment'
        $null = Wait-Like '*Saved as attachment-only.txt'
        if ((Get-FileHash $saved).Hash -ne (Get-FileHash $attachmentPath).Hash) { throw 'Saving from the viewer changed bytes.' }
        [System.Windows.Forms.SendKeys]::SendWait('{ESC}')
        Start-Sleep -Milliseconds 500
        if (Element 'Close viewer') { throw 'Esc did not close the attachment viewer.' }

        Write-Host 'Checking that the composer keeps focus after sending.'
        (Wait-Element 'Message').SetFocus()
        [System.Windows.Forms.SendKeys]::SendWait('Sent with the keyboard')
        [System.Windows.Forms.SendKeys]::SendWait('^{ENTER}')
        Wait-ComposerEmpty
        $null = Wait-Element 'Sent with the keyboard'
        Assert-ComposerFocused 'Ctrl+Enter'
        Set-Text 'Message' 'Sent with the Send button'
        $sendRect = (Wait-Element 'Send').Current.BoundingRectangle
        $null = [NativeWindowBounds]::SetCursorPos([int]($sendRect.Left + $sendRect.Width / 2), [int]($sendRect.Top + $sendRect.Height / 2))
        [NativeWindowBounds]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 60; [NativeWindowBounds]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
        Wait-ComposerEmpty
        $null = Wait-Element 'Sent with the Send button'
        Assert-ComposerFocused 'the Send button'
        Set-Text 'Message' 'Optional file caption'
        Invoke-Control 'Attach files…'
        $null = Wait-Element 'Remove attachment-only.txt'
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Optional file caption') { throw 'Attaching a file lost the text draft.' }
        Invoke-Control 'Send'
        $null = Wait-Element 'Optional file caption'
        Copy-Message 'Optional file caption'
        Select-PartialText 'Optional file caption'
        Invoke-Control 'Copy address'
        if ((Get-Clipboard -Raw).Trim() -ne '/preview/design') { throw 'Peer copy did not preserve the routing address.' }
        Invoke-Control 'Account'
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
        (Wait-Element 'Message').SetFocus()
        [System.Windows.Forms.SendKeys]::SendWait('^f')
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
        Write-Host 'Checking native clipboard attachments and text paste.'
        $composer = Wait-Element 'Message'
        $composer.SetFocus()
        Set-Clipboard 'Optional file caption'
        [System.Windows.Forms.SendKeys]::SendWait('^v')
        Start-Sleep -Milliseconds 300
        if ($composer.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Optional file caption') { throw 'Ordinary text paste failed.' }
        $bitmap = [System.Drawing.Bitmap]::new(2, 2)
        try {
            for ($x = 0; $x -lt 2; $x++) { for ($y = 0; $y -lt 2; $y++) { $bitmap.SetPixel($x, $y, [System.Drawing.Color]::HotPink) } }
            [System.Windows.Forms.Clipboard]::SetImage($bitmap)
            (Wait-Element 'Find in this subject').SetFocus()
            [System.Windows.Forms.SendKeys]::SendWait('^v')
            Start-Sleep -Milliseconds 300
            if (Element 'Image attached.') { throw 'Pasting into search attached an image to the message.' }
            $composer.SetFocus()
            [System.Windows.Forms.SendKeys]::SendWait('^v')
            $null = Wait-Element 'Image attached.'
        } finally { $bitmap.Dispose() }
        $buttons = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button))
        $imageName = @($buttons | Where-Object { $_.Current.Name -like 'Remove image-*.png' })[0].Current.Name.Substring(7)
        if ($composer.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Optional file caption') { throw 'Image paste changed the caption.' }
        if (Element "Download $imageName") { throw 'Pasting sent an image before Send.' }
        Capture 'pasted-image-draft.png'
        Set-Text 'Message' ''
        Invoke-Control 'Send'
        $null = Wait-Element "Download $imageName"
        Check-Download $imageName (Join-Path (Resolve-Path $OutputDirectory).Path $imageName)
        $downloadedImage = [System.Drawing.Bitmap]::new((Join-Path $downloadDirectory $imageName))
        try { if ($downloadedImage.GetPixel(0, 0).ToArgb() -ne [System.Drawing.Color]::HotPink.ToArgb()) { throw 'Clipboard image pixels changed.' } }
        finally { $downloadedImage.Dispose() }
        Open-Viewer $imageName "Picture $imageName" 'viewer-image.png'
        Invoke-Control 'Close viewer'
        Open-Conversation '/preview/guide'
        Open-Viewer 'desktop-checklist.pdf' 'Page 1 of 1' 'viewer-pdf.png'
        Invoke-Control 'Close viewer'
        if (Element 'Close viewer') { throw 'Closing the viewer failed.' }
        Open-Conversation '/preview/team'
        $copiedFiles = [System.Collections.Specialized.StringCollection]::new()
        $copiedFiles.AddRange([string[]]@($attachmentPath, $secondAttachment))
        [System.Windows.Forms.Clipboard]::SetFileDropList($copiedFiles)
        (Wait-Element 'Message').SetFocus()
        [System.Windows.Forms.SendKeys]::SendWait('^v')
        $null = Wait-Element 'Remove attachment-only.txt'
        $null = Wait-Element 'Remove second-file.txt'
        Invoke-Control 'Remove attachment-only.txt'
        Invoke-Control 'Remove second-file.txt'
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
        Invoke-Control 'Subject actions'
        Invoke-Control 'Delete subject…'
        $null = Wait-Element 'Cancel'
        Capture 'delete-subject.png'
        Invoke-Control 'Cancel'
        $null = Wait-Element 'Native parity subject'
        Invoke-Control 'Subject actions'
        Invoke-Control 'Delete subject…'
        Commit-Dialog 'Delete subject'
        if (Element 'Native parity subject') { throw 'Deleted subject remains visible.' }
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Draft in main mailbox') { throw 'Deleting a different subject lost the main draft.' }

        Write-Host 'Checking subject switching and adaptive layouts.'
        Invoke-Control 'Close search'
        $subject = Wait-Element 'Current subject'
        $subject.SetFocus()
        [System.Windows.Forms.SendKeys]::SendWait('%{DOWN}{HOME}{DOWN}{ENTER}')
        Start-Sleep -Milliseconds 500
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne '') { throw 'Draft leaked to another subject.' }
        Capture 'subject-conversation.png'
        Set-Text 'Message' 'Subject-specific draft'
        $subject.SetFocus()
        [System.Windows.Forms.SendKeys]::SendWait('%{DOWN}{HOME}{ENTER}')
        Start-Sleep -Milliseconds 500
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Draft in main mailbox') { throw 'Subject switcher lost the original draft.' }

        foreach ($width in @(1100, 800, 900)) {
            $null = [NativeWindowBounds]::SetWindowPos($process.MainWindowHandle, [IntPtr]::Zero, 12, 12, $width, 720, 4)
            Start-Sleep -Milliseconds 500
            Assert-DesktopColumns
            $null = Wait-Element 'Pin subjects'
            if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Draft in main mailbox') { throw 'Desktop resizing lost the draft.' }
        }
        $subjectItems = (Wait-List 'Subjects').FindAll([System.Windows.Automation.TreeScope]::Children,
            [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::ListItem))
        if ($subjectItems.Count -lt 2) { throw 'The restored subjects column is missing its threads.' }
        $subjectItems[1].GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
        Start-Sleep -Milliseconds 500
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Subject-specific draft') { throw 'The restored column did not select the subject and its draft.' }
        $subjectItems[0].GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
        Start-Sleep -Milliseconds 500
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Draft in main mailbox') { throw 'The restored column lost the original subject draft.' }
        Capture 'medium-window.png'
        $null = [NativeWindowBounds]::SetWindowPos($process.MainWindowHandle, [IntPtr]::Zero, 12, 12, 520, 720, 4)
        $null = Wait-Element 'Back to conversations'
        if (Element 'Subjects') { throw 'Compact conversation should hide the subjects column.' }
        if (Element 'Search senders') { throw 'Compact conversation still shows the sidebar.' }
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Draft in main mailbox') { throw 'Resizing lost the draft.' }
        Capture 'compact-conversation.png'
        Invoke-Control 'Back to conversations'
        $null = Wait-Element 'Search senders'
        if (Element 'Message') { throw 'Compact inbox should hide the composer.' }
        Capture 'compact-inbox.png'
        # Ctrl+F reopens the selected conversation even from the compact inbox.
        (Wait-Element 'Search senders').SetFocus()
        [System.Windows.Forms.SendKeys]::SendWait('^f')
        Set-Text 'Find in this subject' 'History message 1:'
        $null = Wait-Element '1 of 1'
        [System.Windows.Forms.SendKeys]::SendWait('{ESC}')
        Start-Sleep -Milliseconds 250
        if (Element 'Find in this subject') { throw 'Escape did not close search.' }
        (Wait-Element 'Message').SetFocus()
        [System.Windows.Forms.SendKeys]::SendWait('^k')
        $null = Wait-Element 'Search senders'
        $null = [NativeWindowBounds]::SetWindowPos($process.MainWindowHandle, [IntPtr]::Zero, 12, 12, 1440, 900, 4)
        Assert-DesktopColumns
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Draft in main mailbox') { throw 'Returning to wide layout lost the draft.' }
        (Wait-Element 'Pin subjects').GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle()
        Start-Sleep -Milliseconds 500
        if (Element 'Subjects') { throw 'Unpinning did not hide the subjects pane.' }
        Capture 'subjects-hidden.png'
        (Wait-Element 'Pin subjects').GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle()
        Assert-DesktopColumns
        Capture 'refreshed-inbox.png'
        [IO.File]::WriteAllText($env:PIGEONPOST_UI_THEME_FILE, 'Dark')
        for ($n = 0; $n -lt 40 -and (Test-Path $env:PIGEONPOST_UI_THEME_FILE); $n++) { Start-Sleep -Milliseconds 100 }
        if (Test-Path $env:PIGEONPOST_UI_THEME_FILE) { throw 'Native theme switch was not applied.' }
        Capture 'dark-inbox.png'
        Invoke-Control 'New subject'
        Set-Text 'Subject title' 'Dark theme subject'
        Capture 'dark-new-subject.png'
        Invoke-Control 'Cancel'
        [IO.File]::WriteAllText($env:PIGEONPOST_UI_THEME_FILE, 'Light')
        for ($n = 0; $n -lt 40 -and (Test-Path $env:PIGEONPOST_UI_THEME_FILE); $n++) { Start-Sleep -Milliseconds 100 }
        if (Test-Path $env:PIGEONPOST_UI_THEME_FILE) { throw 'Restoring the light theme failed.' }
        Write-Host 'Checking the theme choice in the account menu.'
        Choose-Theme 'Dark'
        Assert-Theme 'Dark' 'dark'
        Capture 'theme-menu-dark.png'
        Invoke-Control 'Account'
        try { (Wait-Element 'Theme').GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand() } catch { }
        Start-Sleep -Milliseconds 400
        Capture 'theme-menu-open.png'
        foreach ($n in 1..2) { if (Element 'Copy my address') { [System.Windows.Forms.SendKeys]::SendWait('{ESC}'); Start-Sleep -Milliseconds 300 } }
        Choose-Theme 'Light'
        Assert-Theme 'Light' 'light'
        Capture 'theme-menu-light.png'
        Choose-Theme 'System'
        Assert-Theme 'Light' 'system'
        Write-Host 'Checking the start-at-sign-in setting in an unpackaged build.'
        Invoke-Control 'Account'
        $startup = Wait-Element 'Start Pigeonpost when you sign in'
        if ($startup.Current.IsEnabled) { throw 'Unpackaged builds cannot start at sign-in, so the setting must be unavailable.' }
        Capture 'startup-setting-unpackaged.png'
        foreach ($n in 1..2) { if (Element 'Copy my address') { [System.Windows.Forms.SendKeys]::SendWait('{ESC}'); Start-Sleep -Milliseconds 300 } }
        $filter = Wait-Element 'Conversation filter'
        $filter.SetFocus()
        [System.Windows.Forms.SendKeys]::SendWait('%{DOWN}{HOME}{DOWN}{ENTER}')
        $null = Wait-Element 'No archived conversations'
        $filter.SetFocus()
        [System.Windows.Forms.SendKeys]::SendWait('%{DOWN}{HOME}{ENTER}')
        Start-Sleep -Milliseconds 500
        if ((Wait-Element 'Message').GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ne 'Draft in main mailbox') { throw 'Changing the archive filter lost the draft.' }

        # Phase 1b: calls, missed-call lines, Filtered, privacy, blocking (stand-in postbox).
        Write-Host 'Checking the Phase 1b screens.'
        Open-Conversation '/preview/design'
        $null = Wait-Like 'Missed video call · *'
        Capture 'missed-call-line.png'
        Invoke-Control 'Calls, 1 missed'
        $null = Wait-List 'Call history'
        $null = Wait-Like '*missed video call*'
        $null = Wait-Element 'Calls'
        $filteredCalls = Wait-Element 'Filtered calls section'
        try { $filteredCalls.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand() } catch { }
        Start-Sleep -Milliseconds 400
        Capture 'calls-page.png'
        Invoke-Control 'Chats'
        Invoke-Control 'Filtered, 1 conversation'
        $null = Wait-List 'Filtered senders'
        $null = Wait-Element 'Limited offer just for you'
        Capture 'filtered.png'
        Invoke-Control 'Chats'
        Invoke-Control 'Account'
        Invoke-Control 'Privacy and filtering…'
        $null = Wait-Element 'Keep call history'
        $null = Wait-Like 'Calls from people you don?t know'
        Capture 'privacy-settings.png'
        Invoke-Control 'Done'
        Invoke-Control 'Account'
        Invoke-Control 'Blocked…'
        $null = Wait-Element 'Nobody is blocked.'
        Capture 'blocked-list.png'
        Invoke-Control 'Done'
        Open-Conversation '/preview/design'
        Invoke-Control 'Conversation actions'
        Invoke-Control 'Block…'
        $null = Wait-Element 'Also report as spam'
        Capture 'block-confirmation.png'
        Commit-Dialog 'Block'
        $null = Wait-Element 'Unblock'
        if ((Wait-Element 'Message').Current.IsEnabled) { throw 'A blocked conversation must disable the composer.' }
        Capture 'blocked-banner.png'
        Invoke-Control 'Unblock'
        for ($n = 0; $n -lt 40 -and -not (Wait-Element 'Message').Current.IsEnabled; $n++) { Start-Sleep -Milliseconds 150 }
        if (-not (Wait-Element 'Message').Current.IsEnabled) { throw 'Unblocking did not restore the composer.' }
        Invoke-Control 'Account'
        Invoke-Control 'Profile picture…'
        Invoke-Control 'Choose a picture'
        $null = Wait-Like 'Choose Save to use this picture.'
        Capture 'profile-picture.png'
        Commit-Dialog 'Save'

        # Phase 3b: an end-to-end encrypted group with the real group library.
        Write-Host 'Checking groups.'
        Invoke-Control 'Groups'
        Invoke-Control 'New group'
        Set-Text 'Group name' 'Native group'
        Capture 'new-group.png'
        Commit-Dialog 'Create'
        $null = Wait-List 'Group messages'
        Set-Text 'Group message' 'Hello encrypted group'
        Invoke-Control 'Send to group'
        $null = Wait-Element 'Hello encrypted group'
        Capture 'group-view.png'
        Invoke-Control 'Group info'
        $null = Wait-Like 'Safety number *'
        $null = Wait-Like '*devices: win-*'
        Capture 'group-info.png'
        (Dialog-Button 'CloseButton').GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 500
        Invoke-Control 'Chats'
        Write-Host 'Phase 1b and groups screens passed.'
    } else {
        $button = Wait-Element 'Sign in or create account'
        $null = Wait-Element 'Privacy policy'
        if (-not $button.Current.IsEnabled) { throw 'Sign-in button is not enabled.' }
        if (Element 'Could not access your saved account. Please try again.') { throw 'Windows credential vault could not be read.' }
        Capture 'welcome.png'
        if ($PackageFamilyName) {
            Write-Host 'Checking the notification-area icon of the installed package.'
            # The app's own diagnostics record the shell's answer for this process.
            $log = @("$env:LOCALAPPDATA\Pigeonpost\logs\desktop.log", "$env:LOCALAPPDATA\Packages\$PackageFamilyName\LocalCache\Local\Pigeonpost\logs\desktop.log") | Where-Object { Test-Path $_ } | Select-Object -First 1
            if (-not $log) { throw 'The installed app wrote no diagnostics log.' }
            Copy-Item $log (Join-Path $OutputDirectory 'installed-desktop.log') -Force
            $lines = @(Get-Content $log | Where-Object { $_.Contains("[$($process.Id)]") })
            $lines | Write-Host
            if (-not ($lines -like '*Launch * as WodoTeknolojiA.PigeonpostDesktop_*')) { throw 'The installed app did not run with its package identity.' }
            if (-not ($lines -like '*Notification-area icon added*shell reports 0x00000000*')) { throw 'Windows did not accept the notification-area icon.' }
            for ($n = 0; $n -lt 20 -and -not ($lines -like '*Start at sign-in: *'); $n++) {
                Start-Sleep -Milliseconds 250
                $lines = @(Get-Content $log | Where-Object { $_.Contains("[$($process.Id)]") })
            }
            if (-not ($lines -like '*Start at sign-in: Disabled*')) { throw "The package's sign-in startup task is missing or not off by default." }
            function Tray-Icon {
                $buttons = [System.Windows.Automation.AutomationElement]::RootElement.FindAll([System.Windows.Automation.TreeScope]::Descendants,
                    [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button))
                $script:trayNames = @($buttons | ForEach-Object { $_.Current.Name } | Where-Object { $_ -match 'pigeon' })
                # The tooltip names the tray icon; the taskbar button for the window is excluded.
                @($buttons | Where-Object { $_.Current.Name -like 'Pigeonpost*' -and $_.Current.Name -notlike '*running window*' -and $_.Current.Name -notlike 'Pigeonpost 1.*' }) | Select-Object -First 1
            }
            function Capture-Screen([string]$Name) {
                $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
                $bitmap = [System.Drawing.Bitmap]::new($bounds.Width, $bounds.Height)
                $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
                try { $graphics.CopyFromScreen($bounds.Left, $bounds.Top, 0, 0, $bitmap.Size); $bitmap.Save((Join-Path $OutputDirectory $Name), [System.Drawing.Imaging.ImageFormat]::Png) }
                finally { $graphics.Dispose(); $bitmap.Dispose() }
            }
            $icon = Tray-Icon
            $where = 'notification area'
            if (-not $icon) {
                # Windows 11 places new icons in the overflow until the user chooses to show them.
                $chevron = [System.Windows.Automation.AutomationElement]::RootElement.FindAll([System.Windows.Automation.TreeScope]::Descendants,
                    [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)) |
                    Where-Object { $_.Current.Name -like '*hidden icons*' } | Select-Object -First 1
                if ($chevron) { $chevron.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() }
                for ($n = 0; $n -lt 10 -and -not $icon; $n++) { Start-Sleep -Milliseconds 400; $icon = Tray-Icon }
                $where = 'overflow'
            }
            Capture-Screen 'installed-notification-area.png'
            if ($where -eq 'overflow') { [System.Windows.Forms.SendKeys]::SendWait('{ESC}'); Start-Sleep -Milliseconds 300 }
            if (-not $icon) { throw "The Pigeonpost icon is missing from the notification area and its overflow (buttons: $($script:trayNames -join ' | '))." }
            Write-Host "Pigeonpost icon found in the $where`: $($icon.Current.Name)"
        }
    }
    Write-Host 'Native Windows UI smoke passed.'
} catch {
    if ($root -and -not $process.HasExited) { Capture 'failure.png' }
    Write-Host "App process exited: $($process.HasExited)"
    $appLog = "$env:LOCALAPPDATA\Pigeonpost\logs\desktop.log"
    if ($PackageFamilyName -and (Test-Path $appLog)) { Get-Content $appLog -Tail 40 | Write-Host }
    if ($PackageFamilyName) {
        Get-WinEvent -LogName Application -MaxEvents 60 -ErrorAction SilentlyContinue |
            Where-Object { $_.ProviderName -in '.NET Runtime', 'Application Error', 'Windows Error Reporting' -and $_.Message -match 'Pigeonpost' } |
            Select-Object -First 3 | ForEach-Object { Write-Host "Event $($_.Id) $($_.ProviderName): $($_.Message)" }
    }
    throw
} finally {
    if ($Fixture) { Remove-Item Env:PIGEONPOST_UI_ATTACHMENT_FILE, Env:PIGEONPOST_UI_DOWNLOAD_DIRECTORY, Env:PIGEONPOST_UI_ARRIVAL_FILE, Env:PIGEONPOST_UI_THEME_FILE, Env:PIGEONPOST_UI_SCROLL_LOG -ErrorAction SilentlyContinue }
    if (-not $process.HasExited) { $null = $process.CloseMainWindow(); if (-not $process.WaitForExit(5000)) { $process.Kill() } }
}
