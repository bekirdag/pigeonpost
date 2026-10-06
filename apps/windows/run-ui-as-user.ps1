#Requires -Version 7.0
param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:GITHUB_ACTIONS -ne 'true') { throw 'This helper is only for disposable GitHub Windows runners.' }
$directory = (New-Item -ItemType Directory -Force $OutputDirectory).FullName
$smoke = Join-Path $PSScriptRoot 'smoke-test.ps1'
# WinUI drag/drop requires an ordinary user. Hosted workers run elevated with UAC disabled.
# Use a real, short-lived local account instead of modifying the worker's security token.
Add-Type @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using System.Security.Principal;
public sealed class TestDesktopAccess : IDisposable {
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] static extern IntPtr GetProcessWindowStation();
    [DllImport("user32.dll")] static extern IntPtr GetThreadDesktop(uint thread);
    [DllImport("user32.dll", SetLastError=true)] static extern bool GetUserObjectSecurity(IntPtr handle, ref int information, byte[] descriptor, uint size, out uint needed);
    [DllImport("user32.dll", SetLastError=true)] static extern bool SetUserObjectSecurity(IntPtr handle, ref int information, byte[] descriptor);
    readonly IntPtr station = GetProcessWindowStation(), desktop = GetThreadDesktop(GetCurrentThreadId());
    byte[] stationAcl, desktopAcl;
    static void Check(bool ok) { if (!ok) throw new Win32Exception(Marshal.GetLastWin32Error()); }
    static byte[] Grant(IntPtr handle, int access, SecurityIdentifier user) {
        int information = 4; uint needed;
        GetUserObjectSecurity(handle, ref information, null, 0, out needed);
        var original = new byte[needed];
        Check(GetUserObjectSecurity(handle, ref information, original, needed, out needed));
        var descriptor = new RawSecurityDescriptor(original, 0);
        descriptor.DiscretionaryAcl.InsertAce(descriptor.DiscretionaryAcl.Count,
            new CommonAce(AceFlags.None, AceQualifier.AccessAllowed, access, user, false, null));
        var updated = new byte[descriptor.BinaryLength]; descriptor.GetBinaryForm(updated, 0);
        Check(SetUserObjectSecurity(handle, ref information, updated)); return original;
    }
    public TestDesktopAccess(string sid) {
        try { var user = new SecurityIdentifier(sid); stationAcl = Grant(station, 0xF037F, user); desktopAcl = Grant(desktop, 0xF01FF, user); }
        catch { Dispose(); throw; }
    }
    public void Dispose() {
        int information = 4;
        if (desktopAcl != null) { Check(SetUserObjectSecurity(desktop, ref information, desktopAcl)); desktopAcl = null; }
        if (stationAcl != null) { Check(SetUserObjectSecurity(station, ref information, stationAcl)); stationAcl = null; }
    }
}
'@
$name = 'ppui-' + [Guid]::NewGuid().ToString('N').Substring(0, 10)
$password = ConvertTo-SecureString ('Pp!' + [Convert]::ToBase64String([Security.Cryptography.RandomNumberGenerator]::GetBytes(32)) + '9') -AsPlainText -Force
$user = $null; $access = $null; $child = $null
$log = Join-Path $directory 'standard-user-ui.log'
$childScript = Join-Path $directory 'standard-user-ui.ps1'
function Literal([string]$Value) { "'" + $Value.Replace("'", "''") + "'" }
try {
    $user = New-LocalUser -Name $name -Password $password -Description 'Disposable Pigeonpost native UI test'
    Add-LocalGroupMember -Group 'Users' -Member $name
    $sid = $user.SID.Value
    & icacls (Get-Location).Path /grant "*${sid}:(OI)(CI)RX" /T /Q | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Could not grant test-source access.' }
    & icacls $directory /grant "*${sid}:(OI)(CI)M" /T /Q | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Could not grant test-artifact access.' }
    $access = [TestDesktopAccess]::new($sid)
    @"
`$ErrorActionPreference = 'Stop'
Start-Transcript -Path $(Literal $log) | Out-Null
try {
    `$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
    if (`$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'UI process is elevated.' }
    Write-Host 'Native UI checks are running as a standard Windows user.'
    & $(Literal $smoke) -Executable $(Literal (Resolve-Path $Executable).Path) -OutputDirectory $(Literal $directory) -Fixture
    exit 0
} catch { Write-Host (`$_ | Out-String); exit 1 }
finally { Stop-Transcript | Out-Null }
"@ | Set-Content $childScript
    $credential = [Management.Automation.PSCredential]::new("$env:COMPUTERNAME\$name", $password)
    $child = Start-Process -FilePath (Get-Command pwsh).Source -Credential $credential -LoadUserProfile -UseNewEnvironment -WindowStyle Hidden -WorkingDirectory (Get-Location).Path -ArgumentList "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$childScript`"" -PassThru
    if (-not $child.WaitForExit(240000)) { $child.Kill($true); throw 'Native UI checks timed out.' }
    if ($child.ExitCode -ne 0) { throw "Native UI checks failed ($($child.ExitCode))." }
} finally {
    if ($child -and -not $child.HasExited) { $child.Kill($true) }
    if (Test-Path $log) { Get-Content $log | Write-Host }
    if ($access) { $access.Dispose() }
    if ($user) { Remove-LocalUser -Name $name }
    $password.Dispose()
}
