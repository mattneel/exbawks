[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if (-not [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [System.Runtime.InteropServices.OSPlatform]::Windows
)) {
    throw "Exbawks runtime development requires Windows 11 on x86-64."
}

if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne "X64") {
    throw "Exbawks runtime development requires an x86-64 host."
}

if (-not (Get-Command rustup -ErrorAction SilentlyContinue)) {
    throw "rustup is not installed. Install rustup before this script."
}

rustup toolchain install 1.97.1 --profile minimal --component clippy,rustfmt --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) {
    throw "rustup toolchain install failed with exit code $LASTEXITCODE."
}

rustup override set 1.97.1
if ($LASTEXITCODE -ne 0) {
    throw "rustup override set failed with exit code $LASTEXITCODE."
}

cargo fetch
if ($LASTEXITCODE -ne 0) {
    throw "cargo fetch failed with exit code $LASTEXITCODE."
}

Write-Host "Exbawks tools are ready."
Write-Host "Run: cargo xtask check"
Write-Host "Run: cargo exbawks doctor"
