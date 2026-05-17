#Requires -Version 5.1
<#
.SYNOPSIS
    Ridge installer for Windows (PowerShell).

.DESCRIPTION
    Installs the Ridge CLI and LSP server by verifying prerequisites
    (Rust >= 1.88, Erlang/OTP >= 26, git >= 2.20) and then running
    `cargo install` for ridge-cli and ridge-lsp.

.PARAMETER DryRun
    Print every command that would be executed (one per line, prefixed
    "[dry-run]") then exit without side-effects.  Used by reviewers and
    the CI dry-run snapshot lane.

.EXAMPLE
    iwr -useb https://ridge-lang.org/install.ps1 | iex

.EXAMPLE
    .\install.ps1 -DryRun

.NOTES
    If PowerShell script execution is blocked, run first:
        Set-ExecutionPolicy -Scope Process Bypass
#>
[CmdletBinding()]
param(
    [switch]$DryRun,
    [switch]$Snapshot   # CI mode: strip platform-detected values for determinism
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# Banner / progress messages flow through the Information stream so they reach
# interactive consoles by default while remaining capturable / suppressible
# (PSScriptAnalyzer rule PSAvoidUsingWriteHost).
$InformationPreference = 'Continue'

# ── Dry-run mode (step 2 of §3.14) ───────────────────────────────────────────
if ($DryRun) {
    # Step 1 — platform detection
    if ($Snapshot) {
        Write-Output '[dry-run] $env:PROCESSOR_ARCHITECTURE  # => <ARCH>'
        Write-Output '[dry-run] $env:OS  # => <OS>'
    }
    else {
        $arch = if ($env:PROCESSOR_ARCHITECTURE) { $env:PROCESSOR_ARCHITECTURE } else { '<unknown>' }
        $os   = if ($env:OS)                     { $env:OS }                     else { '<unknown>' }
        Write-Output "[dry-run] `$env:PROCESSOR_ARCHITECTURE  # => $arch"
        Write-Output "[dry-run] `$env:OS  # => $os"
    }
    # Step 3 — Rust check
    Write-Output '[dry-run] cargo --version'
    # Step 4 — Erlang check.  Uses `io:put_chars` (no format string) instead of
    # `io:format("~s~n", ...)` because PowerShell 5.1 strips inner double quotes
    # when marshaling native command args, which corrupts the eval expression.
    Write-Output "[dry-run] erl -noshell -eval 'io:put_chars(erlang:system_info(otp_release)),init:stop().'"
    # Step 5 — git check
    Write-Output '[dry-run] git --version'
    # Step 6 — install binaries.
    # Snapshot mode is environment-independent: it ALWAYS emits the literal
    # canonical default so the snapshot file stays identical across CI runners
    # whether or not RIDGE_REPO/RIDGE_BRANCH are set.  Non-snapshot -DryRun
    # echoes the *resolved* URL so reviewers see what would be used in this
    # environment (D155 attestation).
    if ($Snapshot) {
        $DryRepo   = 'https://github.com/ridge-lang/ridge'
        $DryBranch = 'main'
    }
    else {
        $DryRepo   = if ($env:RIDGE_REPO)   { $env:RIDGE_REPO }   else { 'https://github.com/ridge-lang/ridge' }
        $DryBranch = if ($env:RIDGE_BRANCH) { $env:RIDGE_BRANCH } else { 'main' }
    }
    Write-Output "[dry-run] cargo install --git $DryRepo --branch $DryBranch ridge-cli"
    Write-Output "[dry-run] cargo install --git $DryRepo --branch $DryBranch ridge-lsp"
    # Step 7 — verify
    Write-Output '[dry-run] ridge --version'
    exit 0
}

# ── Step 1: Detect platform / architecture ────────────────────────────────────
$Arch = $env:PROCESSOR_ARCHITECTURE   # AMD64, ARM64, x86
Write-Information "Platform: Windows ($Arch)"

# ── Helper: semantic version comparison ──────────────────────────────────────
function Compare-Version {
    param([string]$Version, [string]$Minimum)
    $v = $Version -split '\.' | Select-Object -First 2 | ForEach-Object { [int]$_ }
    $m = $Minimum  -split '\.' | Select-Object -First 2 | ForEach-Object { [int]$_ }
    if ($v[0] -gt $m[0]) { return $true }
    if ($v[0] -eq $m[0] -and $v[1] -ge $m[1]) { return $true }
    return $false
}

# ── Step 3: Verify Rust >= 1.88 ──────────────────────────────────────────────
$MinRust = '1.88'
try {
    $cargoOut = & cargo --version 2>&1
}
catch {
    Write-Error @"
error: cargo not found -- Rust is not installed.

  Install Rust via rustup:
    Invoke-WebRequest -Uri 'https://win.rustup.rs/x86_64' -OutFile 'rustup-init.exe'
    .\rustup-init.exe

  Or visit https://rustup.rs
"@
    exit 1
}

if ($cargoOut -match 'cargo (\d+\.\d+)') {
    $rustVer = $Matches[1]
}
else {
    Write-Error "error: could not parse cargo version from: $cargoOut"
    exit 1
}

if (-not (Compare-Version $rustVer $MinRust)) {
    Write-Error @"
error: Rust $rustVer is too old; Ridge requires Rust $MinRust or newer.

  Update via rustup:
    rustup update stable
"@
    exit 1
}

# ── Step 4: Verify Erlang/OTP >= 26 ──────────────────────────────────────────
# Use `io:put_chars(...)` rather than `io:format("~s~n", ...)`: PowerShell 5.1
# strips inner `"` chars when marshaling native command args, so any -eval with
# a quoted format string arrives at erl mangled and triggers an Erlang parse
# error during boot.  `io:put_chars/1` takes char data directly (no format
# string, no `"` in the eval), which is robust across PS 5.1 / 7.x.
$MinOtp = 26
try {
    $otpOut = & erl -noshell -eval 'io:put_chars(erlang:system_info(otp_release)),init:stop().' 2>&1
}
catch {
    Write-Error @"
error: erl not found -- Erlang/OTP is not installed.

  Install Erlang/OTP via Chocolatey:
    choco install erlang

  Or download the installer from https://www.erlang.org/downloads
"@
    exit 1
}

$otpVer = ($otpOut | Out-String).Trim()
if ($otpVer -notmatch '^\d+$') {
    Write-Error "error: could not parse OTP release from erl output: $otpOut"
    exit 1
}
$otpVerInt = [int]$otpVer
if ($otpVerInt -lt $MinOtp) {
    Write-Error @"
error: Erlang/OTP $otpVer is too old; Ridge requires OTP $MinOtp or newer.

  Upgrade Erlang/OTP via Chocolatey:
    choco upgrade erlang

  Or download the installer from https://www.erlang.org/downloads
"@
    exit 1
}

# ── Step 5: Verify git >= 2.20 ────────────────────────────────────────────────
# Uses same rejection message as ridge-pkg's P008 PkgGitTooOld.
$MinGit = '2.20'
try {
    $gitOut = & git --version 2>&1
}
catch {
    Write-Error @"
error: git not found -- git is not installed.

  Install git via Chocolatey:
    choco install git

  Or download from https://git-scm.com/download/win
"@
    exit 1
}

# Lenient parse: first MAJOR.MINOR match (R17 — handles exotic version strings).
if ($gitOut -match '(\d+\.\d+)') {
    $gitVer = $Matches[1]
}
else {
    Write-Error "error: could not parse git version from: $gitOut  (P009 PkgGitVersionUnparseable)"
    exit 1
}

if (-not (Compare-Version $gitVer $MinGit)) {
    Write-Error @"
error: git $gitVer is too old; Ridge requires git $MinGit or newer. (P008 PkgGitTooOld)

  Upgrade git via Chocolatey:
    choco upgrade git

  Or download from https://git-scm.com/download/win
"@
    exit 1
}

# ── Step 6: Install ridge-cli and ridge-lsp ───────────────────────────────────
# Repository / branch are overridable via env vars so CI matrices can pin to
# the transient public mirror (`ridge-lang/ridge`) until `ridge-lang/ridge`
# opens publicly.  Defaults are deterministic and used by `-Snapshot` mode.
$RidgeRepo   = if ($env:RIDGE_REPO)   { $env:RIDGE_REPO }   else { 'https://github.com/ridge-lang/ridge' }
$RidgeBranch = if ($env:RIDGE_BRANCH) { $env:RIDGE_BRANCH } else { 'main' }

Write-Information 'Installing ridge-cli ...'
try {
    & cargo install --git $RidgeRepo --branch $RidgeBranch ridge-cli
    if ($LASTEXITCODE -ne 0) { throw "cargo install ridge-cli exited $LASTEXITCODE" }
}
catch {
    Write-Information ''
    Write-Error "error: cargo install ridge-cli failed: $_"
    exit 1
}

Write-Information 'Installing ridge-lsp ...'
try {
    & cargo install --git $RidgeRepo --branch $RidgeBranch ridge-lsp
    if ($LASTEXITCODE -ne 0) { throw "cargo install ridge-lsp exited $LASTEXITCODE" }
}
catch {
    Write-Information ''
    Write-Error "error: cargo install ridge-lsp failed: $_"
    exit 1
}

# ── Step 7: Verify binary works ───────────────────────────────────────────────
Write-Information 'Verifying installation ...'
$ExpectedVersion = 'ridge 0.1.0'
try {
    $ridgeOut = & ridge --version 2>&1
    if ($LASTEXITCODE -ne 0) { throw "ridge --version exited $LASTEXITCODE" }
}
catch {
    Write-Error @"
error: ridge --version failed after install.

  Ensure %USERPROFILE%\.cargo\bin is on your PATH, then open a new terminal.
"@
    exit 1
}

if ($ridgeOut -notlike "*$ExpectedVersion*") {
    Write-Warning "ridge --version printed '$ridgeOut'; expected '$ExpectedVersion'."
    Write-Warning 'The binary was installed but may be a different version.'
}

# ── Step 8: Success message ────────────────────────────────────────────────────
Write-Information ''
Write-Information 'Ridge installed successfully!'
Write-Information ''
Write-Information "  ridge version: $ridgeOut"
Write-Information ''
Write-Information 'Get started:'
Write-Information '  ridge new my-app; cd my-app; ridge run'
Write-Information ''
Write-Information 'Documentation: https://ridge-lang.org/docs'
