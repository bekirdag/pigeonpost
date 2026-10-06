#Requires -Version 7.0
param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$directory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$smoke = Join-Path $PSScriptRoot 'smoke-test.ps1'
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    & $smoke -Executable $Executable -OutputDirectory $directory -Fixture
    exit 0
}
# WinUI does not support drag-and-drop in elevated apps. Exercise the same standard-user
# context as a normal Store launch, even when GitHub's build worker itself is elevated.
Add-Type @'
using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
public static class StandardUserUi {
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] struct Startup {
        public int cb; public string reserved, desktop, title;
        public int x,y,cx,cy,xChars,yChars,fill,flags; public short show, reservedBytes;
        public IntPtr reservedPointer, input, output, error;
    }
    [StructLayout(LayoutKind.Sequential)] struct ProcessInfo { public IntPtr process, thread; public int pid, tid; }
    [StructLayout(LayoutKind.Sequential)] struct Label { public IntPtr sid; public uint attributes; }
    [DllImport("advapi32.dll", SetLastError=true)] static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError=true)] static extern bool CreateRestrictedToken(IntPtr token, uint flags, uint disabledCount, IntPtr disabled, uint privilegeCount, IntPtr privileges, uint restrictedCount, IntPtr restricted, out IntPtr result);
    [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool ConvertStringSidToSid(string value, out IntPtr sid);
    [DllImport("advapi32.dll", SetLastError=true)] static extern bool SetTokenInformation(IntPtr token, int kind, ref Label label, int size);
    [DllImport("advapi32.dll")] static extern int GetLengthSid(IntPtr sid);
    [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool CreateProcessAsUserW(IntPtr token, string app, StringBuilder command, IntPtr processAttributes, IntPtr threadAttributes, bool inheritHandles, uint flags, IntPtr environment, string directory, ref Startup startup, out ProcessInfo info);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetExitCodeProcess(IntPtr process, out uint code);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool TerminateProcess(IntPtr process, uint code);
    [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr value);
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] static extern IntPtr GetProcessWindowStation();
    [DllImport("user32.dll")] static extern IntPtr GetThreadDesktop(uint thread);
    [DllImport("user32.dll", SetLastError=true)] static extern bool GetUserObjectSecurity(IntPtr handle, ref int information, byte[] descriptor, uint size, out uint needed);
    [DllImport("user32.dll", SetLastError=true)] static extern bool SetUserObjectSecurity(IntPtr handle, ref int information, byte[] descriptor);
    static void Check(bool ok) { if (!ok) throw new Win32Exception(Marshal.GetLastWin32Error()); }
    static byte[] GrantDesktopAccess(IntPtr handle, int access) {
        int information = 4; uint needed;
        GetUserObjectSecurity(handle, ref information, null, 0, out needed);
        var original = new byte[needed];
        Check(GetUserObjectSecurity(handle, ref information, original, needed, out needed));
        var descriptor = new System.Security.AccessControl.RawSecurityDescriptor(original, 0);
        var user = System.Security.Principal.WindowsIdentity.GetCurrent().User;
        descriptor.DiscretionaryAcl.InsertAce(descriptor.DiscretionaryAcl.Count,
            new System.Security.AccessControl.CommonAce(System.Security.AccessControl.AceFlags.None,
                System.Security.AccessControl.AceQualifier.AccessAllowed, access, user, false, null));
        var updated = new byte[descriptor.BinaryLength]; descriptor.GetBinaryForm(updated, 0);
        Check(SetUserObjectSecurity(handle, ref information, updated));
        return original;
    }
    public static int Run(string executable, string script, string directory) {
        IntPtr original = IntPtr.Zero, limited = IntPtr.Zero, sid = IntPtr.Zero;
        var station = GetProcessWindowStation(); var desktop = GetThreadDesktop(GetCurrentThreadId());
        byte[] stationAcl = null, desktopAcl = null;
        try {
            // The runner's desktop ACL grants Administrators. The reduced token instead
            // needs an explicit current-user ACE; restore both objects when checks finish.
            stationAcl = GrantDesktopAccess(station, 0xF037F);
            desktopAcl = GrantDesktopAccess(desktop, 0xF01FF);
            Check(OpenProcessToken(Process.GetCurrentProcess().Handle, 0xF01FF, out original));
            Check(CreateRestrictedToken(original, 0x5, 0, IntPtr.Zero, 0, IntPtr.Zero, 0, IntPtr.Zero, out limited));
            Check(ConvertStringSidToSid("S-1-16-8192", out sid));
            var label = new Label { sid = sid, attributes = 0x20 };
            Check(SetTokenInformation(limited, 25, ref label, Marshal.SizeOf<Label>() + GetLengthSid(sid)));
            var startup = new Startup { cb = Marshal.SizeOf<Startup>(), desktop = @"winsta0\default" };
            ProcessInfo info;
            var shell = Environment.GetEnvironmentVariable("COMSPEC");
            var startupLog = System.IO.Path.ChangeExtension(script, "startup.log");
            var command = "\"" + shell + "\" /d /s /c \"\"" + executable + "\" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File \"" + script + "\" > \"" + startupLog + "\" 2>&1\"";
            Check(CreateProcessAsUserW(limited, shell, new StringBuilder(command),
                IntPtr.Zero, IntPtr.Zero, false, 0x08000000, IntPtr.Zero, directory, ref startup, out info));
            Console.WriteLine("Started standard-user UI process " + info.pid);
            try {
                var wait = WaitForSingleObject(info.process, 180000);
                if (wait == 258) { TerminateProcess(info.process, 1); throw new TimeoutException("Standard-user UI checks timed out."); }
                if (wait != 0) throw new Win32Exception(Marshal.GetLastWin32Error());
                Check(GetExitCodeProcess(info.process, out var code));
                return unchecked((int)code);
            } finally { CloseHandle(info.thread); CloseHandle(info.process); }
        } finally {
            int information = 4;
            if (desktopAcl != null) Check(SetUserObjectSecurity(desktop, ref information, desktopAcl));
            if (stationAcl != null) Check(SetUserObjectSecurity(station, ref information, stationAcl));
            if (sid != IntPtr.Zero) LocalFree(sid);
            if (limited != IntPtr.Zero) CloseHandle(limited);
            if (original != IntPtr.Zero) CloseHandle(original);
        }
    }
}
'@
# Only this disposable artifact directory needs write access from the reduced token.
& icacls $directory /grant "$($env:USERNAME):(OI)(CI)M" /T /Q | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Could not prepare the native UI artifact directory.' }
$log = Join-Path $directory 'standard-user-ui.log'
$childScript = Join-Path $directory 'standard-user-ui.ps1'
function Literal([string]$Value) { "'" + $Value.Replace("'", "''") + "'" }
@"
`$ErrorActionPreference = 'Stop'
Start-Transcript -Path $(Literal $log) | Out-Null
try {
    `$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
    if (`$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'UI process is still elevated.' }
    Write-Host 'Native UI checks are running as a standard user.'
    & $(Literal $smoke) -Executable $(Literal (Resolve-Path $Executable).Path) -OutputDirectory $(Literal $directory) -Fixture
    exit 0
} catch { Write-Host (`$_ | Out-String); exit 1 }
finally { Stop-Transcript | Out-Null }
"@ | Set-Content $childScript
try { $code = [StandardUserUi]::Run((Get-Command pwsh).Source, $childScript, (Get-Location).Path) }
catch {
    Add-Type -AssemblyName System.Drawing, System.Windows.Forms
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bitmap = [System.Drawing.Bitmap]::new($bounds.Width, $bounds.Height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $graphics.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bitmap.Size)
        $bitmap.Save((Join-Path $directory 'standard-user-failure.png'))
    } finally { $graphics.Dispose(); $bitmap.Dispose() }
    throw
}
finally {
    foreach ($item in @($log, [IO.Path]::ChangeExtension($childScript, 'startup.log'))) {
        if (Test-Path $item) { Get-Content $item | Write-Host }
    }
}
if ($code -ne 0) { throw "Standard-user native UI checks failed ($code)." }
