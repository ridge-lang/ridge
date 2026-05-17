# .azure-pipelines/assert-install-timing.ps1
#
# Asserts that the Ridge install script completed within the 5-minute budget
# (verifies install completes under target time).
#
# Usage:
#   ./assert-install-timing.ps1 -Elapsed <seconds>
#
# Exit 0 if Elapsed < 300; exit 1 otherwise.

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [int]$Elapsed
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($Elapsed -lt 0) {
    Write-Error "Elapsed must be non-negative, got: $Elapsed"
    exit 2
}

Write-Output "[install-timing] elapsed=${Elapsed}s"

if ($Elapsed -lt 300) {
    Write-Output '[install-timing] PASS (< 300 s budget)'
    exit 0
}
else {
    Write-Error '[install-timing] FAIL (>= 300 s budget — G2 violated)'
    exit 1
}
