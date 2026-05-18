#Requires -Version 5.1
<#
.SYNOPSIS
    Ridge installer for Windows (PowerShell).

.DESCRIPTION
    Installs the Ridge CLI and LSP server by verifying prerequisites
    (Rust >= 1.88, Erlang/OTP >= 26, git >= 2.20) and then running
    `cargo install` for ridge-cli and ridge-lsp.

.ENVIRONMENT
    RIDGE_DRY_RUN     Set to "1" to print every command that would be
                      executed (one per line, prefixed "[dry-run]") then
                      exit without side-effects.
    RIDGE_SNAPSHOT    Set to "1" alongside RIDGE_DRY_RUN to strip
                      platform-detected values for deterministic snapshot
                      output (CI lane).
    RIDGE_VERSION     Override the release version to install (e.g.
                      "v0.2.0-rc2"). Defaults to latest published release.
    RIDGE_INSTALL_DIR Override the install directory. Defaults to
                      "$env:USERPROFILE\.cargo\bin".
    RIDGE_FORCE_SOURCE Set to "1" to skip the binary-download path and
                      install from source via cargo.

.EXAMPLE
    # Standard pipe install:
    iwr -useb https://ridge-lang.org/install.ps1 | iex

.EXAMPLE
    # Dry-run via env var:
    $env:RIDGE_DRY_RUN = "1"
    iwr -useb https://ridge-lang.org/install.ps1 | iex
    $env:RIDGE_DRY_RUN = $null

.EXAMPLE
    # Download then execute:
    iwr -useb https://ridge-lang.org/install.ps1 -OutFile "$env:TEMP\ridge-install.ps1"
    & "$env:TEMP\ridge-install.ps1"
    Remove-Item "$env:TEMP\ridge-install.ps1"

.NOTES
    If PowerShell script execution is blocked, run first:
        Set-ExecutionPolicy -Scope Process Bypass
#>
# Flags are read from environment variables instead of param() because this
# script is also invoked via `iwr -useb <url> | iex`, and PowerShell's
# Invoke-Expression does not support param blocks (it evaluates the script
# as an expression, not a script file). Env vars work in both invocation
# modes.
$DryRun   = $env:RIDGE_DRY_RUN   -eq "1"
$Snapshot = $env:RIDGE_SNAPSHOT  -eq "1"

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

# ──────────────────────────────────────────────────────────────────────────────
# Binary install path (R051-R054)
# ──────────────────────────────────────────────────────────────────────────────

function Install-FromBinary {
    $triple = Get-PlatformTriple
    if (-not $triple) { return $false }

    $version = $env:RIDGE_VERSION
    if (-not $version) {
        $version = Get-LatestVersion
        if (-not $version) {
            Write-Advisory "R051" "Could not query latest release tag from GitHub"
            return $false
        }
    }

    $assetName = "ridge-$triple.zip"
    $assetUrl = "https://github.com/ridge-lang/ridge/releases/download/$version/$assetName"
    $shaUrl   = "$assetUrl.sha256"

    $tmpDir = New-Item -ItemType Directory -Path (Join-Path $env:TEMP "ridge-install-$([guid]::NewGuid().ToString('N'))")
    try {
        Write-Host "Downloading $assetName ($version)..."
        try {
            Invoke-WebRequest -Uri $assetUrl -OutFile (Join-Path $tmpDir $assetName) -UseBasicParsing -ErrorAction Stop
        } catch {
            Write-Advisory "R051" "Failed to download $assetUrl"
            return $false
        }

        try {
            Invoke-WebRequest -Uri $shaUrl -OutFile (Join-Path $tmpDir "$assetName.sha256") -UseBasicParsing -ErrorAction Stop
        } catch {
            Write-Advisory "R051" "Failed to download SHA256 sidecar from $shaUrl"
            return $false
        }

        Write-Host "Verifying SHA256..."
        $expectedHash = (Get-Content (Join-Path $tmpDir "$assetName.sha256")).Split(' ')[0].ToLower()
        $actualHash = (Get-FileHash (Join-Path $tmpDir $assetName) -Algorithm SHA256).Hash.ToLower()
        if ($expectedHash -ne $actualHash) {
            Write-Advisory "R052" "SHA256 mismatch for $assetName (expected $expectedHash, got $actualHash)"
            return $false
        }

        Write-Host "Extracting to $InstallDir..."
        New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

        # Stop any running ridge-lsp.exe processes — editors like VS Code keep
        # the binary locked while the language server is alive, which causes
        # Expand-Archive -Force to fail with "Access denied" on overwrite.
        # The LSP reconnects cleanly when the editor re-launches it.
        $lspProcs = @(Get-Process -Name "ridge-lsp" -ErrorAction SilentlyContinue)
        if ($lspProcs.Count -gt 0) {
            Write-Host "Stopping $($lspProcs.Count) running ridge-lsp process(es) to free the binary..."
            $lspProcs | Stop-Process -Force -ErrorAction SilentlyContinue
            Start-Sleep -Milliseconds 200
        }

        try {
            Expand-Archive -Path (Join-Path $tmpDir $assetName) -DestinationPath $InstallDir -Force -ErrorAction Stop
        } catch {
            Write-Advisory "R054" "Failed to extract $assetName to $InstallDir : $($_.Exception.Message)"
            return $false
        }

        # Verify both binaries landed on disk (catches partial extracts where a
        # locked file silently kept its old content).
        $ridgeExe    = Join-Path $InstallDir "ridge.exe"
        $ridgeLspExe = Join-Path $InstallDir "ridge-lsp.exe"
        foreach ($binPath in @($ridgeExe, $ridgeLspExe)) {
            if (-not (Test-Path $binPath)) {
                Write-Advisory "R054" "Expected binary missing after extract: $binPath"
                return $false
            }
        }

        Write-Host "Installed ridge + ridge-lsp to $InstallDir"
        return $true
    } finally {
        Remove-Item -Recurse -Force $tmpDir -ErrorAction SilentlyContinue
    }
}

function Get-PlatformTriple {
    # install.ps1 is Windows-only (see #Requires -Version 5.1).
    # Use the PROCESSOR_ARCHITECTURE env var so this works on both
    # Windows PowerShell 5.1 (.NET Framework) and PowerShell 7+ (.NET Core).
    $arch = $env:PROCESSOR_ARCHITECTURE
    switch ($arch) {
        'AMD64' { return 'x86_64-pc-windows-msvc' }
        'ARM64' { Write-Advisory "R053" "Windows ARM64 not yet built"; return $null }
        default { Write-Advisory "R053" "Unsupported Windows architecture: $arch"; return $null }
    }
}

function Get-LatestVersion {
    try {
        $resp = Invoke-RestMethod -Uri "https://api.github.com/repos/ridge-lang/ridge/releases/latest" -UseBasicParsing -ErrorAction Stop
        return $resp.tag_name
    } catch {}
    try {
        $resp = Invoke-RestMethod -Uri "https://api.github.com/repos/ridge-lang/ridge/releases?per_page=1" -UseBasicParsing -ErrorAction Stop
        return $resp[0].tag_name
    } catch {}
    return $null
}

function Write-Advisory {
    param([string]$Code, [string]$Message)
    Write-Host "advisory ${Code}: $Message" -ForegroundColor Yellow
}

# ── Helper: pre-flight write-access test ─────────────────────────────────────
# Open the file in ReadWrite mode with no sharing. If the file is locked by
# another process (e.g. an editor's LSP child holding ridge-lsp.exe), the open
# fails and we return $false. If the file does not exist yet, we treat it as
# writable (the install will create it).
function Test-WriteAccess {
    param([string]$Path)
    if (-not (Test-Path $Path)) { return $true }
    try {
        $stream = [System.IO.File]::Open($Path, 'Open', 'ReadWrite', 'None')
        $stream.Close()
        $stream.Dispose()
        return $true
    } catch {
        return $false
    }
}

# Try to ensure a binary path is writable: if it isn't, stop processes that
# could be holding it (best-effort), sleep briefly, and re-test once. Returns
# $true if the file is writable on entry or after the kill; $false if still
# locked (typical when an editor's LSP client re-spawns the process before we
# can complete the install).
function Wait-ForUnlockedBinary {
    param([string]$Path, [string]$ProcessName)

    if (Test-WriteAccess $Path) { return $true }

    $procs = @(Get-Process -Name $ProcessName -ErrorAction SilentlyContinue)
    if ($procs.Count -gt 0) {
        Write-Host "Stopping $($procs.Count) running $ProcessName process(es) to free the binary..."
        $procs | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 500
    }

    return (Test-WriteAccess $Path)
}

# ── Step 6: Install ridge-cli and ridge-lsp ───────────────────────────────────
$InstallDir = if ($env:RIDGE_INSTALL_DIR) { $env:RIDGE_INSTALL_DIR } else { Join-Path $env:USERPROFILE ".cargo\bin" }
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

# Pre-flight: both the binary-fetch and cargo-install paths ultimately write
# to ridge-lsp.exe in the install dir. If an editor's LSP client (e.g. VS
# Code's Ridge extension) is actively holding the file and re-spawning the
# process as fast as we kill it, BOTH install paths will fail — and the
# cargo-install path wastes ~2 minutes compiling before discovering this.
# Detect the lock now and bail with an actionable message.
$ridgeLspExe = Join-Path $InstallDir "ridge-lsp.exe"
if (-not (Wait-ForUnlockedBinary $ridgeLspExe "ridge-lsp")) {
    Write-Error @"
error: $ridgeLspExe is locked by another process and could not be freed.

  An editor (likely VS Code) with the Ridge extension active is holding
  the language-server binary. The install script tried to stop the
  ridge-lsp process, but it was re-launched immediately by the editor's
  LSP client.

  Please fully close any editor with Ridge files open (in VS Code:
  File -> Exit, not just close the window), then re-run this install.

  To verify no ridge-lsp process is running:
    Get-Process ridge-lsp -ErrorAction SilentlyContinue
"@
    exit 1
}

# Binary-first install path (unless RIDGE_FORCE_SOURCE=1)
if ($env:RIDGE_FORCE_SOURCE -ne "1") {
    if (Install-FromBinary) {
        # Run existing post-install version check and exit success
        Write-Information 'Verifying installation ...'
        $ExpectedVersion = 'ridge 0.2.0-rc2'
        try {
            $ridgeOut = & ridge --version 2>&1
            if ($LASTEXITCODE -ne 0) { throw "ridge --version exited $LASTEXITCODE" }
        }
        catch {
            Write-Error @"
error: ridge --version failed after install.

  Ensure $InstallDir is on your PATH, then open a new terminal.
"@
            exit 1
        }
        if ($ridgeOut -notlike "*$ExpectedVersion*") {
            Write-Warning "ridge --version printed '$ridgeOut'; expected '$ExpectedVersion'."
            Write-Warning 'The binary was installed but may be a different version.'
        }
        Write-Information ''
        Write-Information 'Ridge installed successfully!'
        Write-Information ''
        Write-Information "  ridge version: $ridgeOut"
        Write-Information ''
        Write-Information 'Get started:'
        Write-Information '  ridge new my-app; cd my-app; ridge run'
        Write-Information ''
        Write-Information 'Documentation: https://ridge-lang.org/docs'
        exit 0
    }
    Write-Host "Falling back to source install via cargo..."
}

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
$ExpectedVersion = 'ridge 0.2.0-rc2'
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
