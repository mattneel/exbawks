[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Push-Location (Join-Path $PSScriptRoot "..")
try {
    python scripts/static-validate.py
    cargo xtask check
}
finally {
    Pop-Location
}
