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
    [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool CreateProcessWithTokenW(IntPtr token, uint logon, string app, StringBuilder command, uint flags, IntPtr environment, string directory, ref Startup startup, out ProcessInfo info);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr value);
    static void Check(bool ok) { if (!ok) throw new Win32Exception(Marshal.GetLastWin32Error()); }
    public static int Run(string executable, string script, string directory) {
        IntPtr original = IntPtr.Zero, limited = IntPtr.Zero, sid = IntPtr.Zero;
        try {
            Check(OpenProcessToken(Process.GetCurrentProcess().Handle, 0xF01FF, out original));
            Check(CreateRestrictedToken(original, 0x5, 0, IntPtr.Zero, 0, IntPtr.Zero, 0, IntPtr.Zero, out limited));
            Check(ConvertStringSidToSid("S-1-16-8192", out sid));
            var label = new Label { sid = sid, attributes = 0x20 };
            Check(SetTokenInformation(limited, 25, ref label, Marshal.SizeOf<Label>() + GetLengthSid(sid)));
            var startup = new Startup { cb = Marshal.SizeOf<Startup>(), desktop = @"winsta0\default" };
            ProcessInfo info;
            Check(CreateProcessWithTokenW(limited, 0, executable,
                new StringBuilder("\"" + executable + "\" -NoProfile -ExecutionPolicy Bypass -File \"" + script + "\""),
                0x08000000, IntPtr.Zero, directory, ref startup, out info));
            try {
                using (var child = Process.GetProcessById(info.pid)) {
                    if (!child.WaitForExit(180000)) { child.Kill(true); throw new TimeoutException("Standard-user UI checks timed out."); }
                    return child.ExitCode;
                }
            } finally { CloseHandle(info.thread); CloseHandle(info.process); }
        } finally {
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
finally { if (Test-Path $log) { Get-Content $log | Write-Host } }
if ($code -ne 0) { throw "Standard-user native UI checks failed ($code)." }
