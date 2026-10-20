# Build the Windows native libraries (win-x64 and win-arm64) and stage them with the generated
# C# binding, laid out as runtimes/<rid>/native/ for a NuGet-style or direct project reference.
#   pwsh crates/pigeonpost-groupcrypto/scripts/build-windows.ps1
# Requires Visual Studio 2022 C++ build tools for x64 and ARM64.
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $PSScriptRoot
Set-Location $here
$out = Join-Path $here 'target/windows'
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
$targets = @{ 'x86_64-pc-windows-msvc' = 'win-x64'; 'aarch64-pc-windows-msvc' = 'win-arm64' }
foreach ($triple in $targets.Keys) {
  rustup target add $triple | Out-Null
  cargo build --locked --release -p pigeonpost-groupcrypto-ffi --target $triple
  if ($LASTEXITCODE -ne 0) { throw "cargo build failed for $triple" }
  $dest = Join-Path $out "runtimes/$($targets[$triple])/native"
  New-Item -ItemType Directory -Force -Path $dest | Out-Null
  Copy-Item "target/$triple/release/pigeonpost_groupcrypto_ffi.dll" $dest
}
Copy-Item 'bindings/csharp/pigeonpost_groupcrypto.cs' $out
Get-ChildItem -Recurse $out -File | ForEach-Object { '{0,10}  {1}' -f $_.Length, $_.FullName.Substring($out.Length + 1) }
