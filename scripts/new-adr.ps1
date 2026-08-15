[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-z0-9-]+$')]
    [string]$Slug,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$Title
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$adrDirectory = Join-Path $PSScriptRoot "..\docs\adr"
$numbers = Get-ChildItem -LiteralPath $adrDirectory -Filter "*.md" |
    ForEach-Object {
        if ($_.BaseName -match '^(\d{4})-') {
            [int]$Matches[1]
        }
    }
$next = if ($numbers) { ($numbers | Measure-Object -Maximum).Maximum + 1 } else { 1 }
$prefix = $next.ToString("0000")
$path = Join-Path $adrDirectory "$prefix-$Slug.md"

if (Test-Path -LiteralPath $path) {
    throw "The ADR path already exists: $path"
}

$content = @"
# ADR ${prefix}: $Title

## Status

Proposed.

## Context

Describe the technical constraint.

## Decision

Describe the selected design.

## Consequences

Describe benefits, costs, and follow-up work.
"@

Set-Content -LiteralPath $path -Value $content -Encoding UTF8
Write-Output $path
