# .azure-pipelines/setup-toolchain-windows.ps1
#
# Installs Rust (stable, minimal) + Erlang/OTP + git on a fresh Windows agent
# via Chocolatey so subsequent pipeline steps can run `cargo`, `erl`, `git`.
# Used by Stage 3 `BuildTestMatrix` of `azure-pipelines.yml`.
#
# Idempotent: choco install on an already-installed package is a no-op.

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Write-Host '[setup-toolchain-windows] choco install erlang rustup.install git'
choco install -y erlang
choco install -y rustup.install
choco install -y git

# rustup.install is silent; force-default the stable toolchain explicitly so
# subsequent `cargo` calls do not prompt or fail with "no default toolchain".
& "$env:USERPROFILE\.cargo\bin\rustup.exe" default stable

# Add Rust + Erlang + git directories to PATH for subsequent pipeline steps.
# Azure DevOps requires the `##vso[task.prependpath]` syntax so the change
# survives across `script:` / `powershell:` step boundaries.
Write-Host "##vso[task.prependpath]$env:USERPROFILE\.cargo\bin"

# ── Erlang/OTP PATH wiring ───────────────────────────────────────────────────
# `##vso[task.prependpath]` only affects *subsequent* pipeline steps; it does
# NOT update the current PowerShell session's $env:Path.  We therefore do
# both: (a) emit the prependpath directives for the agent so G1/G2/... see
# erl on PATH, and (b) splice the discovered erl bin dir into $env:Path so
# the verification at the end of *this* script can actually run erl.

$chocoBin = 'C:\ProgramData\chocolatey\bin'

# Locate the real erl.exe via a scoped glob -- `C:\Program Files\Erlang OTP`
# is choco's install root; the erts version subdir name (erts-XX.Y) varies
# between OTP releases so we use a glob.  Recursive scans of `C:\Program
# Files` walk into WindowsApps (ACL-restricted) and silently return empty.
$erlRoot = 'C:\Program Files\Erlang OTP'
$erlBinDir = $null
if (Test-Path -LiteralPath $erlRoot) {
    $candidate = Get-Item -Path "$erlRoot\erts-*\bin\erl.exe" -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($null -ne $candidate) {
        $erlBinDir = $candidate.DirectoryName
        Write-Host "[setup-toolchain-windows] discovered erl.exe at: $($candidate.FullName)"
    }
}

# Tell the agent about both paths (subsequent steps).  Order matters: the
# real erl bin dir wins over the choco shim if both are emitted, which we
# want because the shim adds a process layer that has historically misbehaved
# under PowerShell strict-mode invocation in install.ps1 (`& erl -eval ...`).
Write-Host "##vso[task.prependpath]$chocoBin"
if ($null -ne $erlBinDir) {
    Write-Host "##vso[task.prependpath]$erlBinDir"
}

# Splice into the *current* session's PATH so the fail-fast verification
# below sees erl.  Mirror the agent ordering (real dir first, shim second).
if ($null -ne $erlBinDir) {
    $env:Path = "$erlBinDir;$env:Path"
}
$env:Path = "$chocoBin;$env:Path"

# Fail fast if erl is still unreachable -- much cheaper to bail here (one
# `Setup toolchain` step) than after G1/cargo build/release on a Windows
# agent (~10 min of compile time wasted before G2 hits the same wall).
Write-Host '[setup-toolchain-windows] verifying erl is on PATH...'
$erlCmd = Get-Command erl -ErrorAction SilentlyContinue
if ($null -eq $erlCmd) {
    Write-Host '[setup-toolchain-windows] FATAL: erl.exe is not on PATH after toolchain setup.'
    Write-Host '[setup-toolchain-windows] current $env:Path (one entry per line):'
    foreach ($p in $env:Path -split ';') {
        if ($p) { Write-Host "  $p" }
    }
    if (Test-Path -LiteralPath $erlRoot) {
        Write-Host "[setup-toolchain-windows] erl.exe candidates under '$erlRoot':"
        Get-ChildItem -Path $erlRoot -Filter 'erl.exe' -Recurse -ErrorAction SilentlyContinue |
            ForEach-Object { Write-Host "  $($_.FullName)" }
    }
    else {
        Write-Host "[setup-toolchain-windows] '$erlRoot' does not exist."
    }
    throw 'Erlang/OTP not reachable from this session after toolchain setup.'
}
Write-Host "[setup-toolchain-windows] erl resolved to: $($erlCmd.Source)"

Write-Host '[setup-toolchain-windows] tool versions:'
& cargo --version
# We intentionally do NOT invoke `erl -eval ...` here.  PowerShell 5.1
# (Windows Server 2025 agents) strips inner `"` chars when marshaling
# native command args, so `io:format("OTP ~s~n", ...)` arrives at erl
# as `io:format(OTP~s~n, ...)` and triggers an Erlang parse error.
# `Get-Command erl` above already proves erl.exe is reachable; the
# downstream G1/G2/... steps will surface any erl runtime issue.
& git --version
