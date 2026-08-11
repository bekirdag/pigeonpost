#Requires -Version 7.0

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$BinaryPath,

    [ValidateRange(5, 120)]
    [int]$StartupTimeoutSeconds = 30
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

if (-not [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
        [System.Runtime.InteropServices.OSPlatform]::Windows
    )) {
    throw "windows-release acceptance must run on Windows"
}

function Resolve-ExactBinary {
    param([Parameter(Mandatory = $true)][string]$Path)

    $resolved = Resolve-Path -LiteralPath $Path -ErrorAction Stop
    if (@($resolved).Count -ne 1) {
        throw "the exact release binary path must resolve to one file"
    }
    $item = Get-Item -LiteralPath $resolved.ProviderPath -Force
    if ($item.PSIsContainer -or $item.Extension -ine ".exe") {
        throw "the exact release binary must be a Windows .exe file"
    }
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "the exact release binary must not be a reparse point"
    }
    return $item.FullName
}

function Resolve-SidValue {
    param([Parameter(Mandatory = $true)][string]$Identity)

    try {
        return ([System.Security.Principal.SecurityIdentifier]::new($Identity)).Value
    } catch {
        return ([System.Security.Principal.NTAccount]::new($Identity)).Translate(
            [System.Security.Principal.SecurityIdentifier]
        ).Value
    }
}

function Assert-OwnerPrivateAcl {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][bool]$Directory
    )

    $acl = Get-Acl -LiteralPath $Path
    if (-not $acl.AreAccessRulesProtected) {
        throw "private fixture ACL is not protected: $Path"
    }
    if ((Resolve-SidValue -Identity $acl.Owner) -ne $script:CurrentSid.Value) {
        throw "private fixture is not owned by the current user: $Path"
    }

    $rules = @($acl.GetAccessRules(
            $true,
            $false,
            [System.Security.Principal.SecurityIdentifier]
        ))
    if ($rules.Count -ne 1) {
        throw "private fixture must have exactly one explicit access rule: $Path"
    }
    $rule = $rules[0]
    $ruleSid = $rule.IdentityReference.Translate(
        [System.Security.Principal.SecurityIdentifier]
    ).Value
    $fullControl = [System.Security.AccessControl.FileSystemRights]::FullControl
    if ($ruleSid -ne $script:CurrentSid.Value -or
        $rule.AccessControlType -ne [System.Security.AccessControl.AccessControlType]::Allow -or
        ($rule.FileSystemRights -band $fullControl) -ne $fullControl -or
        $rule.PropagationFlags -ne [System.Security.AccessControl.PropagationFlags]::None) {
        throw "private fixture must grant full access only to the current user: $Path"
    }

    if ($Directory) {
        $required = [System.Security.AccessControl.InheritanceFlags]::ObjectInherit -bor
            [System.Security.AccessControl.InheritanceFlags]::ContainerInherit
        if (($rule.InheritanceFlags -band $required) -ne $required) {
            throw "private directory rule must protect descendant files and directories: $Path"
        }
    } elseif ($rule.InheritanceFlags -ne [System.Security.AccessControl.InheritanceFlags]::None) {
        throw "private file rule must not carry inheritance flags: $Path"
    }
}

function Set-OwnerPrivateAcl {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][bool]$Directory
    )

    if ($Directory) {
        $acl = [System.Security.AccessControl.DirectorySecurity]::new()
        $inheritance = [System.Security.AccessControl.InheritanceFlags]::ObjectInherit -bor
            [System.Security.AccessControl.InheritanceFlags]::ContainerInherit
    } else {
        $acl = [System.Security.AccessControl.FileSecurity]::new()
        $inheritance = [System.Security.AccessControl.InheritanceFlags]::None
    }
    $acl.SetOwner($script:CurrentSid)
    $acl.SetAccessRuleProtection($true, $false)
    $rule = [System.Security.AccessControl.FileSystemAccessRule]::new(
        $script:CurrentSid,
        [System.Security.AccessControl.FileSystemRights]::FullControl,
        $inheritance,
        [System.Security.AccessControl.PropagationFlags]::None,
        [System.Security.AccessControl.AccessControlType]::Allow
    )
    [void]$acl.AddAccessRule($rule)
    Set-Acl -LiteralPath $Path -AclObject $acl
    Assert-OwnerPrivateAcl -Path $Path -Directory $Directory
}

function New-OwnerPrivateDirectory {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (Test-Path -LiteralPath $Path) {
        throw "refusing to reuse an existing acceptance directory: $Path"
    }
    [void][System.IO.Directory]::CreateDirectory($Path)
    Set-OwnerPrivateAcl -Path $Path -Directory $true
}

function Set-OwnerPrivateFile {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "private fixture file does not exist: $Path"
    }
    Set-OwnerPrivateAcl -Path $Path -Directory $false
}

function Convert-HexToBytes {
    param([Parameter(Mandatory = $true)][string]$Hex)

    if ($Hex -notmatch '^[0-9a-f]{64}$') {
        throw "fixture seed must be exactly 32 lowercase hex bytes"
    }
    $bytes = [byte[]]::new(32)
    for ($index = 0; $index -lt $bytes.Length; $index++) {
        $bytes[$index] = [Convert]::ToByte($Hex.Substring($index * 2, 2), 16)
    }
    return ,$bytes
}

function Write-OwnerPrivateSeed {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Hex
    )

    $seed = Convert-HexToBytes -Hex $Hex
    try {
        [System.IO.File]::WriteAllBytes($Path, $seed)
    } finally {
        [System.Array]::Clear($seed, 0, $seed.Length)
    }
    Set-OwnerPrivateFile -Path $Path
    if ((Get-Item -LiteralPath $Path).Length -ne 32) {
        throw "fixture signing seed must contain exactly 32 raw bytes"
    }
}

function Write-OwnerPrivateText {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Content
    )

    [System.IO.File]::WriteAllText(
        $Path,
        $Content,
        [System.Text.UTF8Encoding]::new($false)
    )
    Set-OwnerPrivateFile -Path $Path
}

function New-ExactProcessStartInfo {
    param([Parameter(Mandatory = $true)][string[]]$ArgumentList)

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $script:ExactBinary
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $ArgumentList) {
        [void]$startInfo.ArgumentList.Add($argument)
    }

    # Explicit arguments and fixture files are the whole test contract. Host configuration must
    # not silently redirect a service, select another home, or enable additional logging.
    foreach ($name in @(
            "PIGEONPOST_HOME",
            "PIGEONPOST_RECOVERY_DIR",
            "PIGEONPOST_LOFT_DIR",
            "PIGEONPOST_BIND",
            "PIGEONPOST_CAPACITY_GB",
            "PIGEONPOST_RETENTION_DAYS",
            "PIGEONPOST_DIRECTORY_BIND",
            "PIGEONPOST_DIRECTORY_DIR",
            "PIGEONPOST_DIRECTORY_URL",
            "PIGEONPOST_REGISTRY_BIND",
            "PIGEONPOST_REGISTRY_DIR",
            "PIGEONPOST_REGISTRY_ORIGIN",
            "PIGEONPOST_REGISTRY_URL",
            "PIGEONPOST_LEGACY_CHECKPOINT",
            "PIGEONPOST_NPM_LAUNCHER_ENTRY",
            "PIGEONPOST_NPM_LAUNCHER_NODE",
            "PIGEONPOST_NPM_LAUNCHER_PROTOCOL",
            "PIGEONPOST_UNTRUSTED_BODY_END",
            "PIGEONPOST_ALLOW_INSECURE_PROVIDER_SECRET_ENV",
            "PIGEONPOST_ALLOW_MOCK_IDENTITIES",
            "PIGEONPOST_TEST_ALLOW_MOCK_IDENTITIES",
            "PIGEONPOST_GITHUB_CLIENT_ID",
            "PIGEONPOST_GITHUB_CLIENT_SECRET",
            "PIGEONPOST_GITHUB_CLIENT_SECRET_FILE",
            "PIGEONPOST_GOOGLE_CLIENT_ID"
        )) {
        [void]$startInfo.Environment.Remove($name)
    }
    $startInfo.Environment["PIGEONPOST_LOG"] = "off"
    return $startInfo
}

function Stop-ProcessTree {
    param([Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process)

    if ($Process.HasExited) {
        return
    }
    try {
        $Process.Kill($true)
    } catch [System.Management.Automation.MethodException] {
        $Process.Kill()
    }
    if (-not $Process.WaitForExit(10000)) {
        throw "acceptance child process $($Process.Id) did not terminate"
    }
}

function Invoke-ExactBinary {
    param(
        [Parameter(Mandatory = $true)][string[]]$ArgumentList,
        [int]$TimeoutSeconds = 30
    )

    $process = [System.Diagnostics.Process]::new()
    $started = $false
    try {
        $process.StartInfo = New-ExactProcessStartInfo -ArgumentList $ArgumentList
        if (-not $process.Start()) {
            throw "could not start the exact release binary"
        }
        $started = $true
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            Stop-ProcessTree -Process $process
            throw "exact release command exceeded its timeout"
        }
        $stdout = $stdoutTask.GetAwaiter().GetResult()
        $stderr = $stderrTask.GetAwaiter().GetResult()
        $exitCode = $process.ExitCode
        if ($exitCode -ne 0) {
            throw "exact release command failed with exit code $exitCode; stdout=$($stdout.Trim()); stderr=$($stderr.Trim())"
        }
        return [pscustomobject]@{
            Stdout = $stdout
            Stderr = $stderr
        }
    } finally {
        if ($started -and -not $process.HasExited) {
            Stop-ProcessTree -Process $process
        }
        $process.Dispose()
    }
}

function Start-ManagedExactBinary {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string[]]$ArgumentList
    )

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = New-ExactProcessStartInfo -ArgumentList $ArgumentList
    if (-not $process.Start()) {
        $process.Dispose()
        throw "could not start exact $Name process"
    }
    return [pscustomobject]@{
        Name = $Name
        Process = $process
        StdoutTask = $process.StandardOutput.ReadToEndAsync()
        StderrTask = $process.StandardError.ReadToEndAsync()
    }
}

function Start-ManagedProgram {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$FileName,
        [Parameter(Mandatory = $true)][string[]]$ArgumentList
    )

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FileName
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $ArgumentList) {
        [void]$startInfo.ArgumentList.Add($argument)
    }
    foreach ($environmentName in @("NODE_OPTIONS", "NODE_PATH")) {
        [void]$startInfo.Environment.Remove($environmentName)
    }

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    if (-not $process.Start()) {
        $process.Dispose()
        throw "could not start $Name fixture process"
    }
    return [pscustomobject]@{
        Name = $Name
        Process = $process
        StdoutTask = $process.StandardOutput.ReadToEndAsync()
        StderrTask = $process.StandardError.ReadToEndAsync()
    }
}

function Get-ManagedDiagnostics {
    param([Parameter(Mandatory = $true)]$Runtime)

    if (-not $Runtime.Process.HasExited) {
        return "process is still running"
    }
    $stdout = $Runtime.StdoutTask.GetAwaiter().GetResult().Trim()
    $stderr = $Runtime.StderrTask.GetAwaiter().GetResult().Trim()
    return "exit=$($Runtime.Process.ExitCode); stdout=$stdout; stderr=$stderr"
}

function Assert-ManagedProcessRunning {
    param([Parameter(Mandatory = $true)]$Runtime)

    if ($Runtime.Process.HasExited) {
        throw "$($Runtime.Name) exited during acceptance; $(Get-ManagedDiagnostics -Runtime $Runtime)"
    }
}

function Stop-ManagedProcess {
    param([Parameter(Mandatory = $true)]$Runtime)

    Stop-ProcessTree -Process $Runtime.Process
    [void]$Runtime.StdoutTask.GetAwaiter().GetResult()
    [void]$Runtime.StderrTask.GetAwaiter().GetResult()
    $Runtime.Process.Dispose()
}

function Get-UniqueFreePort {
    for ($attempt = 0; $attempt -lt 20; $attempt++) {
        $listener = [System.Net.Sockets.TcpListener]::new(
            [System.Net.IPAddress]::Loopback,
            0
        )
        try {
            $listener.Start()
            $port = ([System.Net.IPEndPoint]$listener.LocalEndpoint).Port
        } finally {
            $listener.Stop()
        }
        if ($script:UsedPorts.Add($port)) {
            return $port
        }
    }
    throw "could not reserve a unique loopback port"
}

function Wait-HttpSuccess {
    param(
        [Parameter(Mandatory = $true)][string]$Uri,
        [Parameter(Mandatory = $true)]$Runtime
    )

    $deadline = [System.DateTimeOffset]::UtcNow.AddSeconds($StartupTimeoutSeconds)
    while ([System.DateTimeOffset]::UtcNow -lt $deadline) {
        if ($Runtime.Process.HasExited) {
            throw "$($Runtime.Name) exited before $Uri became ready; $(Get-ManagedDiagnostics -Runtime $Runtime)"
        }
        try {
            $response = $script:HttpClient.GetAsync($Uri).GetAwaiter().GetResult()
            try {
                if ([int]$response.StatusCode -eq 200) {
                    return
                }
            } finally {
                $response.Dispose()
            }
        } catch {
            # Startup connection failures are expected until the listener is bound.
        }
        Start-Sleep -Milliseconds 200
    }
    throw "$($Runtime.Name) did not return HTTP 200 from $Uri within $StartupTimeoutSeconds seconds"
}

function Assert-LeafFile {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "expected release-created file is missing: $Path"
    }
    Assert-OwnerPrivateAcl -Path $Path -Directory $false
}

$script:ExactBinary = Resolve-ExactBinary -Path $BinaryPath
$script:CurrentSid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
if ($null -eq $script:CurrentSid) {
    throw "could not resolve the current Windows user SID"
}
$script:UsedPorts = [System.Collections.Generic.HashSet[int]]::new()

$handler = [System.Net.Http.HttpClientHandler]::new()
$handler.UseProxy = $false
$script:HttpClient = [System.Net.Http.HttpClient]::new($handler)
$script:HttpClient.Timeout = [System.TimeSpan]::FromSeconds(2)

# RUNNER_TEMP is workflow scratch and may sit beneath a volume whose ancestry is not suitable for
# private Pigeonpost state. Use the current user's standard temporary namespace; the exact binary
# still validates every ancestor and fails closed if the host has made that namespace unsafe.
$temporaryRoot = [System.IO.Path]::GetTempPath()
$temporaryRoot = [System.IO.Path]::GetFullPath($temporaryRoot).TrimEnd(
    [System.IO.Path]::DirectorySeparatorChar,
    [System.IO.Path]::AltDirectorySeparatorChar
)
$runRoot = Join-Path $temporaryRoot ("pigeonpost-windows-release-" + [guid]::NewGuid().ToString("N"))
$cleanupPrefix = $temporaryRoot + [System.IO.Path]::DirectorySeparatorChar
if (-not $runRoot.StartsWith($cleanupPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "acceptance directory escaped the current-user temporary directory"
}

$loftRuntime = $null
$directoryRuntime = $null
$registryRuntime = $null
$registryTrap = $null
$cleanupErrors = [System.Collections.Generic.List[string]]::new()

try {
    New-OwnerPrivateDirectory -Path $runRoot

    $version = (Invoke-ExactBinary -ArgumentList @("--version")).Stdout.Trim()
    if ($version -notmatch '^pigeonpost [0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$') {
        throw "exact release binary returned an invalid version string"
    }

    $agentHome = Join-Path $runRoot "agent-home"
    $identityResult = Invoke-ExactBinary -ArgumentList @(
        "--home", $agentHome, "--json", "id"
    )
    $identity = $identityResult.Stdout | ConvertFrom-Json -ErrorAction Stop
    if ($identity.address -notmatch '^/k/[a-z0-9]+$' -or
        -not [System.StringComparer]::OrdinalIgnoreCase.Equals(
            [System.IO.Path]::GetFullPath([string]$identity.home),
            [System.IO.Path]::GetFullPath($agentHome)
        ) -or
        @($identity.lofts).Count -ne 0 -or
        [int64]$identity.unread -ne 0) {
        throw "fresh agent identity JSON does not match the isolated home contract"
    }
    Assert-OwnerPrivateAcl -Path $agentHome -Directory $true
    Assert-OwnerPrivateAcl -Path (Join-Path $agentHome "recovery") -Directory $true
    foreach ($relative in @(
            "identity.key",
            "token.secret",
            "recovery\successor.key",
            "state.db",
            "state.db-wal",
            "state.db-shm"
        )) {
        Assert-LeafFile -Path (Join-Path $agentHome $relative)
    }

    $storageResult = Invoke-ExactBinary -ArgumentList @(
        "--home", $agentHome, "--json", "storage", "status"
    )
    $storage = $storageResult.Stdout | ConvertFrom-Json -ErrorAction Stop
    if ($storage.updated -ne $false -or
        [int64]$storage.limits.inbox_messages -le 0 -or
        [int64]$storage.limits.outbox_rows -le 0 -or
        [int64]$storage.usage.inbox_messages -ne 0 -or
        [int64]$storage.usage.outbox_rows -ne 0) {
        throw "fresh storage status JSON is incomplete or nonempty"
    }

    $loftPort = Get-UniqueFreePort
    $loftDirectory = Join-Path $runRoot "loft"
    [void](Invoke-ExactBinary -ArgumentList @(
            "install",
            "--dir", $loftDirectory,
            "--capacity-gb", "1",
            "--retention-days", "1",
            "--bind", "127.0.0.1:$loftPort",
            "--no-service"
        ))
    Assert-OwnerPrivateAcl -Path $loftDirectory -Directory $true
    Assert-OwnerPrivateAcl -Path (Join-Path $loftDirectory "data") -Directory $true
    Assert-LeafFile -Path (Join-Path $loftDirectory "loft.key")
    Assert-LeafFile -Path (Join-Path $loftDirectory "loft.toml")

    $loftRuntime = Start-ManagedExactBinary -Name "loft" -ArgumentList @(
        "loft", "serve", "--dir", $loftDirectory
    )
    Wait-HttpSuccess -Uri "http://127.0.0.1:$loftPort/ready" -Runtime $loftRuntime
    foreach ($relative in @(
            "data\loft.db",
            "data\loft.db-wal",
            "data\loft.db-shm"
        )) {
        Assert-LeafFile -Path (Join-Path $loftDirectory $relative)
    }
    Assert-ManagedProcessRunning -Runtime $loftRuntime
    Stop-ManagedProcess -Runtime $loftRuntime
    $loftRuntime = $null

    $nodeCommand = @(Get-Command node -CommandType Application -ErrorAction Stop)[0]
    if ($null -eq $nodeCommand) {
        throw "the hosted Windows acceptance requires Node.js for its witnessed Registry fixture"
    }
    $nodeItem = Get-Item -LiteralPath $nodeCommand.Source -Force
    if ($nodeItem.PSIsContainer -or
        ($nodeItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "the Node.js fixture runtime must be a regular executable"
    }
    $nodeBinary = $nodeItem.FullName

    $origin = "pigeonpost.test/registry"
    $witnessName = "pigeonpost.test/witness"
    $registryPublicKey = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
    $witnessPublicKey = "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c"
    $directoryPort = Get-UniqueFreePort

    # Hold the future Registry port while Directory starts. Any eager Registry connection remains
    # queued here and fails the assertion below; after that proof, the read-only fixture takes over.
    $registryTrap = [System.Net.Sockets.TcpListener]::new(
        [System.Net.IPAddress]::Loopback,
        0
    )
    $registryTrap.Start()
    $registryPort = ([System.Net.IPEndPoint]$registryTrap.LocalEndpoint).Port
    if (-not $script:UsedPorts.Add($registryPort)) {
        throw "registry connection-trap port collided with another fixture"
    }

    # The shipped Registry intentionally refuses Windows because its durable custody primitives are
    # Linux/macOS-only. This minimal loopback fixture exposes only the witnessed size-zero read API
    # needed to exercise the exact Windows Directory's fail-closed readiness verifier. The fixed
    # seeds below are public RFC 8032 test vectors, not deployment credentials.
    $registryFixtureSource = @'
"use strict";

const crypto = require("node:crypto");
const http = require("node:http");

const PRIVATE_KEY_PREFIX = Buffer.from("302e020100300506032b657004220420", "hex");
const EMPTY_ROOT_HEX = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const [
  portText,
  origin,
  witnessName,
  registrySeedHex,
  witnessSeedHex,
  expectedRegistryKey,
  expectedWitnessKey,
] = process.argv.slice(2);

function fail(message) {
  process.stderr.write(`windows Registry fixture: ${message}\n`);
  process.exit(1);
}

const port = Number(portText);
if (!Number.isSafeInteger(port) || port < 1024 || port > 65535) {
  fail("invalid loopback port");
}
if (
  typeof origin !== "string" ||
  origin.length < 1 ||
  origin.length > 256 ||
  !/^[\x21-\x7e]+$/.test(origin)
) {
  fail("invalid checkpoint origin");
}
if (
  typeof witnessName !== "string" ||
  witnessName.length < 1 ||
  witnessName.length > 256 ||
  !/^[\x21-\x7e]+$/.test(witnessName)
) {
  fail("invalid witness name");
}

function canonicalHex32(name, value) {
  if (typeof value !== "string" || !/^[0-9a-f]{64}$/.test(value)) {
    fail(`${name} must be 32 canonical hex bytes`);
  }
  return Buffer.from(value, "hex");
}

function privateKey(seed) {
  return crypto.createPrivateKey({
    key: Buffer.concat([PRIVATE_KEY_PREFIX, seed]),
    format: "der",
    type: "pkcs8",
  });
}

function rawPublicKey(key) {
  const encoded = crypto.createPublicKey(key).export({ format: "der", type: "spki" });
  if (!Buffer.isBuffer(encoded) || encoded.length < 32) {
    fail("could not derive an Ed25519 public key");
  }
  return encoded.subarray(encoded.length - 32);
}

function keyHash(name, algorithm, publicKey) {
  return crypto
    .createHash("sha256")
    .update(Buffer.from(`${name}\n`, "utf8"))
    .update(Buffer.from([algorithm]))
    .update(publicKey)
    .digest()
    .subarray(0, 4);
}

const registrySeed = canonicalHex32("Registry seed", registrySeedHex);
const witnessSeed = canonicalHex32("witness seed", witnessSeedHex);
canonicalHex32("expected Registry key", expectedRegistryKey);
canonicalHex32("expected witness key", expectedWitnessKey);
const registryKey = privateKey(registrySeed);
const witnessKey = privateKey(witnessSeed);
registrySeed.fill(0);
witnessSeed.fill(0);
const registryPublicKey = rawPublicKey(registryKey);
const witnessPublicKey = rawPublicKey(witnessKey);
if (registryPublicKey.toString("hex") !== expectedRegistryKey) {
  fail("Registry test-vector public key mismatch");
}
if (witnessPublicKey.toString("hex") !== expectedWitnessKey) {
  fail("witness test-vector public key mismatch");
}

const emptyRoot = Buffer.from(EMPTY_ROOT_HEX, "hex");
const body = `${origin}\n0\n${emptyRoot.toString("base64")}\n`;
const operatorSignature = crypto.sign(null, Buffer.from(body, "utf8"), registryKey);
const operatorBlob = Buffer.concat([
  keyHash(origin, 0x01, registryPublicKey),
  operatorSignature,
]);
const dash = String.fromCodePoint(0x2014);
const operatorNote = `${body}\n${dash} ${origin} ${operatorBlob.toString("base64")}\n`;

function witnessedCheckpoint() {
  const witnessedAt = Math.floor(Date.now() / 1000);
  const message = `cosignature/v1\ntime ${witnessedAt}\n${body}`;
  const signature = crypto.sign(null, Buffer.from(message, "utf8"), witnessKey);
  const blob = Buffer.alloc(4 + 8 + signature.length);
  keyHash(witnessName, 0x04, witnessPublicKey).copy(blob, 0);
  blob.writeBigUInt64BE(BigInt(witnessedAt), 4);
  signature.copy(blob, 12);
  return {
    note: `${operatorNote}${dash} ${witnessName} ${blob.toString("base64")}\n`,
    witnessedAt,
  };
}

function send(response, status, contentType, bodyText) {
  const payload = Buffer.from(bodyText, "utf8");
  response.writeHead(status, {
    "Cache-Control": "no-store",
    Connection: "close",
    "Content-Length": payload.length,
    "Content-Type": contentType,
    "X-Content-Type-Options": "nosniff",
  });
  response.end(payload);
}

const server = http.createServer((request, response) => {
  if (request.method !== "GET") {
    send(response, 405, "text/plain; charset=utf-8", "method not allowed");
    return;
  }
  const url = new URL(request.url, "http://127.0.0.1");
  if (url.pathname === "/health" && url.search === "") {
    send(response, 200, "text/plain; charset=utf-8", "ok");
    return;
  }
  if (url.pathname === "/v1/log/status" && url.search === "") {
    const checkpoint = witnessedCheckpoint();
    send(
      response,
      200,
      "application/json; charset=utf-8",
      JSON.stringify({
        ready: true,
        committed_size: 0,
        published_size: 0,
        lag_entries: 0,
        witnessed_at: checkpoint.witnessedAt,
      }),
    );
    return;
  }
  if (url.pathname === "/v1/log/checkpoint" && url.search === "") {
    send(response, 200, "text/plain; charset=utf-8", witnessedCheckpoint().note);
    return;
  }
  if (
    url.pathname === "/v1/log/consistency" &&
    url.searchParams.get("from") === "0" &&
    url.searchParams.get("to") === "0" &&
    [...url.searchParams.keys()].length === 2
  ) {
    send(
      response,
      200,
      "application/json; charset=utf-8",
      JSON.stringify({ from: 0, to: 0, root: EMPTY_ROOT_HEX, path: [] }),
    );
    return;
  }
  send(response, 404, "text/plain; charset=utf-8", "not found");
});

server.on("clientError", (_error, socket) => socket.destroy());
server.listen(port, "127.0.0.1");
'@
    $registryFixturePath = Join-Path $runRoot "witnessed-registry-fixture.cjs"
    Write-OwnerPrivateText -Path $registryFixturePath -Content $registryFixtureSource

    $directoryRoot = Join-Path $runRoot "directory"
    New-OwnerPrivateDirectory -Path $directoryRoot
    $seedPath = Join-Path $directoryRoot "directory-signing.key"
    Write-OwnerPrivateSeed -Path $seedPath -Hex (
        "c5aa8df43f9f837bedb7442f31dcb7b1" +
        "66d38535076f094b85ce3a2e0b4458f7"
    )

    $directoryConfig = @"
signing_key_file = "directory-signing.key"
witness_wait_seconds = 1

[registry]
registry_url = "http://127.0.0.1:$registryPort/"
expected_origin = "$origin"
registry_checkpoint_key = "$registryPublicKey"
witness_threshold = 1
minimum_checkpoint_size = 0
minimum_checkpoint_root = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
max_staleness_seconds = 600
refresh_interval_seconds = 1
state_path = "registry-state.json"

[[registry.witnesses]]
name = "$witnessName"
public_key = "$witnessPublicKey"
"@
    $configPath = Join-Path $directoryRoot "directory.toml"
    Write-OwnerPrivateText -Path $configPath -Content $directoryConfig

    $directoryRuntime = Start-ManagedExactBinary -Name "directory" -ArgumentList @(
        "directory", "serve",
        "--bind", "127.0.0.1:$directoryPort",
        "--dir", $directoryRoot
    )
    Wait-HttpSuccess -Uri "http://127.0.0.1:$directoryPort/health" -Runtime $directoryRuntime
    foreach ($relative in @(
            "directory.db",
            "directory.db-wal",
            "directory.db-shm",
            "prober.key"
        )) {
        Assert-LeafFile -Path (Join-Path $directoryRoot $relative)
    }
    Start-Sleep -Milliseconds 250
    if ($registryTrap.Pending()) {
        throw "empty Directory startup or liveness contacted Registry before readiness"
    }
    $registryTrap.Stop()
    $registryTrap = $null

    $registryRuntime = Start-ManagedProgram -Name "witnessed Registry" -FileName $nodeBinary -ArgumentList @(
        $registryFixturePath,
        [string]$registryPort,
        $origin,
        $witnessName,
        ("9d61b19deffd5a60ba844af492ec2cc4" +
            "4449c5697b326919703bac031cae7f60"),
        ("4ccd089b28ff96da9db6c346ec114e0f" +
            "5b8a319f35aba624da8cf6ed4fb8a6fb"),
        $registryPublicKey,
        $witnessPublicKey
    )
    Wait-HttpSuccess -Uri "http://127.0.0.1:$registryPort/health" -Runtime $registryRuntime
    Wait-HttpSuccess -Uri "http://127.0.0.1:$directoryPort/ready" -Runtime $directoryRuntime

    Assert-ManagedProcessRunning -Runtime $directoryRuntime
    Assert-ManagedProcessRunning -Runtime $registryRuntime
    Stop-ManagedProcess -Runtime $directoryRuntime
    $directoryRuntime = $null
    Stop-ManagedProcess -Runtime $registryRuntime
    $registryRuntime = $null
    Write-Output "windows release acceptance: exact binary client, Loft, and Directory lifecycle passed"
} finally {
    foreach ($runtime in @($directoryRuntime, $registryRuntime, $loftRuntime)) {
        if ($null -eq $runtime) {
            continue
        }
        try {
            Stop-ManagedProcess -Runtime $runtime
        } catch {
            $cleanupErrors.Add($_.Exception.Message)
        }
    }
    if ($null -ne $registryTrap) {
        try {
            $registryTrap.Stop()
        } catch {
            $cleanupErrors.Add($_.Exception.Message)
        }
    }
    try {
        $script:HttpClient.Dispose()
        $handler.Dispose()
    } catch {
        $cleanupErrors.Add($_.Exception.Message)
    }

    if ($cleanupErrors.Count -eq 0 -and (Test-Path -LiteralPath $runRoot)) {
        Remove-Item -LiteralPath $runRoot -Recurse -Force
    }
    if ($cleanupErrors.Count -ne 0) {
        throw "acceptance cleanup failed: $($cleanupErrors -join '; ')"
    }
}
