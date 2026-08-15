[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$XbePath,

    [ValidateSet(64, 128)]
    [int]$RamMiB = 64
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$resolved = (Resolve-Path -LiteralPath $XbePath).Path
Push-Location (Join-Path $PSScriptRoot "..")
try {
    cargo exbawks run $resolved --ram-mib $RamMiB
}
finally {
    Pop-Location
}
