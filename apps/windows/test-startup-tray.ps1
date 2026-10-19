#Requires -Version 7.0
# Start at sign-in, simulated in the session that owns the taskbar: the UI-test build treats
# PIGEONPOST_UI_STARTUP=1 like Windows' StartupTask activation. It must stay out of sight with its
# notification-area icon and inbox running, open on the next launch, and explain closing to the
# notification area once (or log that Windows alerts are off). Disposable runners only.
param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'This helper is only for disposable GitHub Windows runners.' }
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing, System.Windows.Forms
$OutputDirectory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$fixtures = New-Item -ItemType Directory -Force (Join-Path ([IO.Path]::GetTempPath()) "pp-startup-$([Guid]::NewGuid().ToString('N'))")
$env:PIGEONPOST_UI_ATTACHMENT_FILE = Join-Path $fixtures 'attachment-only.txt'
Set-Content $env:PIGEONPOST_UI_ATTACHMENT_FILE 'fixture'
$env:PIGEONPOST_UI_DOWNLOAD_DIRECTORY = $fixtures.FullName
$log = "$env:LOCALAPPDATA\Pigeonpost\logs\desktop.log"
Remove-Item "$env:LOCALAPPDATA\Pigeonpost\preferences.json" -ErrorAction SilentlyContinue
$process = $null
function Lines { if (Test-Path $log) { @(Get-Content $log | Where-Object { $_.Contains("[$($process.Id)]") }) } else { @() } }
function Wait-Line([string]$Pattern, [int]$Seconds = 30) {
    for ($n = 0; $n -lt $Seconds * 4; $n++) { if ((Lines) -like $Pattern) { return }; Start-Sleep -Milliseconds 250 }
    throw "The app never logged '$Pattern'."
}
function Visible-Window { $process.Refresh(); $process.MainWindowHandle -ne 0 }
function Capture-Screen([string]$Name) {
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bitmap = [System.Drawing.Bitmap]::new($bounds.Width, $bounds.Height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try { $graphics.CopyFromScreen($bounds.Left, $bounds.Top, 0, 0, $bitmap.Size); $bitmap.Save((Join-Path $OutputDirectory $Name), [System.Drawing.Imaging.ImageFormat]::Png) }
    finally { $graphics.Dispose(); $bitmap.Dispose() }
}
function Buttons { [System.Windows.Automation.AutomationElement]::RootElement.FindAll([System.Windows.Automation.TreeScope]::Descendants,
    [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)) }
function Launch-Again {
    $second = Start-Process -FilePath $Executable -PassThru
    if (-not $second.WaitForExit(20000)) { $second.Kill(); throw 'A second launch did not hand over to the running app.' }
    for ($n = 0; $n -lt 60 -and -not (Visible-Window); $n++) { Start-Sleep -Milliseconds 250 }
    if (-not (Visible-Window)) { throw 'Launching again did not open the window.' }
}
function Close-ToTray {
    $null = $process.CloseMainWindow()
    for ($n = 0; $n -lt 40 -and (Visible-Window); $n++) { Start-Sleep -Milliseconds 250 }
    if ($process.HasExited) { throw 'Closing the window quit the app instead of keeping it in the notification area.' }
    if (Visible-Window) { throw 'Closing did not hide the window.' }
    Start-Sleep -Milliseconds 500
}
try {
    $env:PIGEONPOST_UI_STARTUP = '1'
    $process = Start-Process -FilePath $Executable -PassThru
    Remove-Item Env:PIGEONPOST_UI_STARTUP
    Wait-Line '*Started at sign-in; staying in the notification area*'
    Wait-Line '*Inbox open*'
    if (-not ((Lines) -like '*Notification-area icon added*shell reports 0x00000000*')) { throw 'The sign-in start has no notification-area icon.' }
    Start-Sleep -Seconds 2
    if (Visible-Window) { throw 'Starting at sign-in opened a window.' }
    if ($process.HasExited) { throw 'Starting at sign-in exited.' }
    $chevron = Buttons | Where-Object { $_.Current.Name -like '*hidden icons*' } | Select-Object -First 1
    if ($chevron) { $chevron.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke(); Start-Sleep -Milliseconds 800 }
    $icon = Buttons | Where-Object { $_.Current.Name -like 'Pigeonpost*' -and $_.Current.Name -notlike '*running window*' -and $_.Current.Name -notlike 'Pigeonpost 1.*' } | Select-Object -First 1
    Capture-Screen 'startup-in-notification-area.png'
    [System.Windows.Forms.SendKeys]::SendWait('{ESC}')
    if (-not $icon) { throw 'The sign-in start is missing from the notification area.' }
    Write-Host "Started at sign-in with no window; icon: $($icon.Current.Name)"

    Launch-Again
    Start-Sleep -Seconds 2
    Capture-Screen 'startup-window-opened.png'
    Close-ToTray
    Capture-Screen 'startup-closed-to-tray.png'
    $hints = @((Lines) -like '*Notification-area hint*')
    if ($hints.Count -ne 1) { throw "The first close should explain the notification area once (got $($hints.Count))." }
    Write-Host "First close: $($hints[0])"
    if ($hints[0] -like '*hint shown*' -and -not (Get-Content "$env:LOCALAPPDATA\Pigeonpost\preferences.json" -Raw).Contains('"tray_hint_shown":true')) { throw 'The hint was not remembered.' }
    Launch-Again
    Close-ToTray
    if (@((Lines) -like '*Notification-area hint*').Count -ne 1) { throw 'The notification-area hint repeated.' }
    Write-Host 'Start at sign-in and the one-time notification-area hint passed.'
} finally {
    if ($process) { Lines | Write-Host; Lines | Set-Content (Join-Path $OutputDirectory 'startup-desktop.log') }
    if ($process -and -not $process.HasExited) { $process.Kill() }
    Remove-Item Env:PIGEONPOST_UI_ATTACHMENT_FILE, Env:PIGEONPOST_UI_DOWNLOAD_DIRECTORY, Env:PIGEONPOST_UI_STARTUP -ErrorAction SilentlyContinue
    Remove-Item $fixtures -Recurse -Force -ErrorAction SilentlyContinue
}
