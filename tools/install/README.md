# Ridge Install Scripts

Scripts for installing the Ridge toolchain (`ridge-cli` + `ridge-lsp`).
By default both scripts download a pre-built binary from GitHub Releases.
If no binary is available for your platform, they fall back to building from
source via `cargo install`.

## Quick install

**Linux / macOS**

```bash
curl -sSf https://ridge-lang.org/install.sh | sh
```

**Windows** (PowerShell)

```powershell
& ([scriptblock]::Create((iwr -useb 'https://ridge-lang.org/install.ps1').Content))
```

If PowerShell script execution is blocked, run first:

```powershell
Set-ExecutionPolicy -Scope Process Bypass
```

### Pipe install with options

Options are passed via environment variables (PowerShell's `Invoke-Expression` does not support param blocks):

```powershell
$env:RIDGE_FORCE_SOURCE = "1"
& ([scriptblock]::Create((iwr -useb 'https://raw.githubusercontent.com/ridge-lang/ridge/main/tools/install/install.ps1').Content))
$env:RIDGE_FORCE_SOURCE = $null
```

> **Why `& ([scriptblock]::Create(...))` instead of `| iex`?** Two reasons:
> 1. `iex` (`Invoke-Expression`) evaluates the input as an expression, not a script file, so `param` blocks and `#Requires` directives at the top of `install.ps1` are mishandled.
> 2. `exit` statements inside `iex` terminate the **calling shell** (i.e. close your terminal window). Wrapping the script in a scriptblock isolates `exit` to the scriptblock's scope.
>
> The `[scriptblock]::Create(...)` pattern is the same one used by `rustup-init.ps1`.

## Install behavior

Both scripts follow a binary-first approach:

1. **Binary install (default)** — downloads the pre-built archive for your
   platform from GitHub Releases, verifies its SHA256 checksum, and extracts
   `ridge` + `ridge-lsp` to `$INSTALL_DIR`.  This is the fastest path (~10 s).
2. **Source install (fallback)** — if binary download fails or the platform
   has no pre-built artifact, the script falls back to
   `cargo install --git <repo> ridge-cli` and `cargo install … ridge-lsp`.
   This requires Rust ≥ 1.88 and takes 3–5 minutes.

To skip the binary path and always build from source:

```bash
RIDGE_FORCE_SOURCE=1 sh install.sh
```

```powershell
$env:RIDGE_FORCE_SOURCE = "1"
.\install.ps1
```

## Files

| File | Purpose |
|------|---------|
| `install.sh` | POSIX installer — Linux and macOS |
| `install.ps1` | PowerShell installer — Windows |
| `expected_dryrun.txt` | Snapshot fixture for the CI dry-run lane |

## Prerequisites

Both scripts verify the following before installing:

| Prerequisite | Minimum version | Why |
|--------------|----------------|-----|
| Rust (`cargo`) | 1.88 | Required by Ridge's transitive deps (icu_*, time, zip require ≥ 1.88) |
| Erlang/OTP (`erl`) | 26 | Required BEAM runtime |
| `git` | 2.20 | Used by `cargo install --git` (protocol.version=2 requires ≥ 2.20) |

If a prerequisite is missing or too old, the script exits 1 with a platform-specific install/upgrade hint.

## `--dry-run` / dry-run mode

Both scripts support a no-side-effects mode that prints every command they would execute — one per line, prefixed `[dry-run]` — then exits 0 without making any changes.

```bash
# Linux / macOS
sh install.sh --dry-run
```

```powershell
# Windows — env var (works in both pipe and download-then-execute modes)
$env:RIDGE_DRY_RUN = "1"
& ([scriptblock]::Create((iwr -useb 'https://ridge-lang.org/install.ps1').Content))
$env:RIDGE_DRY_RUN = $null

# Windows — download then execute
$env:RIDGE_DRY_RUN = "1"
.\install.ps1
$env:RIDGE_DRY_RUN = $null
```

Example output:

```
[dry-run] uname -s  # => Linux
[dry-run] uname -m  # => x86_64
[dry-run] cargo --version
[dry-run] erl -noshell -eval 'io:put_chars(erlang:system_info(otp_release)),init:stop().'
[dry-run] git --version
[dry-run] cargo install --git https://github.com/ridge-lang/ridge --branch main ridge-cli
[dry-run] cargo install --git https://github.com/ridge-lang/ridge --branch main ridge-lsp
[dry-run] ridge --version
```

## CI snapshot lane (`install-dryrun-snapshot`)

The CI lane diffs the live `--dry-run` output byte-for-byte against `expected_dryrun.txt`.

### Snapshot format

`expected_dryrun.txt` contains two sections separated by header lines:

```
### install.sh --dry-run (snapshot mode) ###
<lines from install.sh --snapshot>
### install.ps1 -DryRun (snapshot mode) ###
<lines from install.ps1 dry-run snapshot>
```

The `--snapshot` / `-Snapshot` flags suppress platform-detected runtime values (e.g., `uname -s` output) and replace them with `<OS>` / `<ARCH>` placeholders, making the snapshot fully deterministic across runner platforms.

**Line endings:** `expected_dryrun.txt` uses Unix line endings (`\n`).  PowerShell on Windows emits `\r\n`; the CI diff step strips `\r` before comparing (`tr -d '\r'`), so the file stays portable.

### Canonical update flow (R18)

When you intentionally change a command in either install script:

1. Edit the script (`install.sh` and/or `install.ps1`).
2. Regenerate the snapshot:
   ```bash
   bash tools/install/install.sh --snapshot > /tmp/sh_snap.txt
   powershell.exe -NoProfile -NonInteractive -Command \
     "$env:RIDGE_DRY_RUN='1'; $env:RIDGE_SNAPSHOT='1'; & 'tools/install/install.ps1'" | tr -d '\r' > /tmp/ps_snap.txt
   
   printf '%s\n%s\n%s\n%s\n' \
     '### install.sh --dry-run (snapshot mode) ###' \
     "$(cat /tmp/sh_snap.txt)" \
     '### install.ps1 -DryRun (snapshot mode) ###' \
     "$(cat /tmp/ps_snap.txt)" \
     > tools/install/expected_dryrun.txt
   ```
3. Diff the new snapshot against the old one to confirm the change is intentional.
4. Commit both files together (`install.sh`/`install.ps1` + `expected_dryrun.txt`) in the same commit so the PR reviewer can validate the diff.

The CI `install-dryrun-snapshot` stage fails if these files drift.

## CI lint lane (`install-lint`)

The CI runs static analysis on every PR:

| Script | Tool | Command |
|--------|------|---------|
| `install.sh` | [shellcheck](https://www.shellcheck.net/) | `shellcheck install.sh --severity=warning` |
| `install.ps1` | [PSScriptAnalyzer](https://github.com/PowerShell/PSScriptAnalyzer) | `Invoke-ScriptAnalyzer install.ps1 -EnableExit` |

`shellcheck --severity=warning` treats warnings as errors (shellcheck's default is to fail on any finding at or above the specified severity, so warnings are included).  `Invoke-ScriptAnalyzer -EnableExit` fails the build on `Error` severity findings.

**Local verification (best-effort):**

```bash
# shellcheck (skip if not installed — CI is the enforcing layer)
shellcheck tools/install/install.sh --severity=warning

# PSScriptAnalyzer (skip if not installed)
Invoke-ScriptAnalyzer tools/install/install.ps1 -EnableExit
```

Installing the tools locally is recommended but not required.  The CI is the authoritative gate for both linters.

## Environment variables

### Binary install controls

| Variable | Default | Purpose |
|----------|---------|---------|
| `RIDGE_VERSION` | _(latest GitHub release)_ | Pin a specific release tag, e.g. `RIDGE_VERSION=v0.2.0-rc2` |
| `RIDGE_INSTALL_DIR` | `~/.cargo/bin` (Linux/macOS) / `%USERPROFILE%\.cargo\bin` (Windows) | Directory where `ridge` and `ridge-lsp` are placed |
| `RIDGE_FORCE_SOURCE` | `0` | Set to `1` to skip the binary path and always build from source |

**Examples:**

```bash
# Install a specific version
RIDGE_VERSION=v0.2.0-rc2 sh install.sh

# Install to a custom directory
RIDGE_INSTALL_DIR=/usr/local/bin sh install.sh

# Force source build (requires Rust)
RIDGE_FORCE_SOURCE=1 sh install.sh
```

```powershell
# Install a specific version
$env:RIDGE_VERSION = 'v0.2.0-rc2'
.\install.ps1

# Install to a custom directory
$env:RIDGE_INSTALL_DIR = 'C:\tools\ridge'
.\install.ps1

# Force source build (requires Rust)
$env:RIDGE_FORCE_SOURCE = '1'
.\install.ps1
```

### Source install controls (`cargo install --git`)

| Variable | Default | Purpose |
|----------|---------|---------|
| `RIDGE_REPO` | `https://github.com/ridge-lang/ridge` | Git repository URL |
| `RIDGE_BRANCH` | `main` | Git branch / ref |

These are used only when the source-install path runs (fallback or `RIDGE_FORCE_SOURCE=1`).  This is the lever the CI matrix uses to pin installs to the transient public mirror until the canonical `ridge-lang/ridge` repository opens.

**Example — Linux / macOS:**

```bash
export RIDGE_REPO=https://github.com/ridge-lang/ridge
export RIDGE_BRANCH=main
bash tools/install/install.sh
```

**Example — Windows (PowerShell):**

```powershell
$env:RIDGE_REPO   = 'https://github.com/ridge-lang/ridge'
$env:RIDGE_BRANCH = 'main'
.\tools\install\install.ps1
```

**Visibility in `--dry-run`.** Non-snapshot `--dry-run` / `-DryRun` echoes the *resolved* URL so a reviewer can see exactly what the install would do under the current environment.

**Snapshot determinism.** The CI `--snapshot` / `-Snapshot` mode is environment-independent on purpose: it always emits the literal canonical default (`https://github.com/ridge-lang/ridge` / `main`) regardless of whether the env vars are set.  This keeps `expected_dryrun.txt` byte-identical across runners and prevents env leakage from breaking the snapshot diff.

## Advisory codes

The binary install path emits structured advisory messages to stderr when a non-fatal issue occurs.  These are prefixed `advisory <CODE>:` and allow callers to detect specific failure modes programmatically.

| Code | Trigger | Meaning |
|------|---------|---------|
| R051 | Download failed | Could not fetch the release archive or SHA256 sidecar from GitHub. Check network connectivity and that the release tag exists. |
| R052 | SHA256 mismatch | The downloaded archive's checksum does not match the `.sha256` sidecar. The file may be corrupt or tampered with; do not use it. |
| R053 | Unsupported platform | No pre-built binary exists for this OS / architecture combination.  The script falls back to the source install path automatically. |
| R054 | Extract failed | The archive could not be extracted to `INSTALL_DIR`.  Check available disk space and write permissions. |

When any of R051–R054 fires, the binary path returns failure and the script falls back to `cargo install` automatically (unless `RIDGE_FORCE_SOURCE=1`, in which case the source path was already the primary path).

## Edge cases

| Scenario | Behaviour |
|----------|-----------|
| GitHub API unreachable | R051 emitted; falls back to source install |
| Release tag not found | R051 emitted; falls back to source install |
| SHA256 mismatch | R052 emitted; aborts binary install, falls back to source |
| Linux aarch64 | R053 emitted (no artifact yet); falls back to source install |
| Windows ARM64 | R053 emitted (no artifact yet); falls back to source install |
| Extraction fails | R054 emitted; falls back to source install |
| Rust missing | Exit 1 with `curl … sh.rustup.rs` / `rustup-init.exe` hint |
| Rust < 1.88 | Exit 1 with `rustup update stable` hint |
| Erlang missing | Exit 1 with `apt` / `brew` / `choco` hint |
| Erlang/OTP < 26 | Exit 1 with upgrade hint |
| `git` missing | Exit 1 with `apt` / `brew` / `choco` hint |
| `git` < 2.20 | Exit 1 with `P008 PkgGitTooOld` message + upgrade hint |
| `cargo install` fails | Relay `cargo` stderr; exit 1 |
| `~/.cargo/bin` permission denied (Linux) | Print `chown -R $(whoami) ~/.cargo/bin` hint |
| Windows execution policy blocks script | Print `Set-ExecutionPolicy -Scope Process Bypass` hint |
| Exotic `git` version string (e.g. `Apple Git-143`) | Lenient parse: first `MAJOR.MINOR` match wins; parse failure emits `P009 PkgGitVersionUnparseable` |
| Exotic OTP package (e.g. `esl-erlang`) | If version detection fails, set `SKIP_ERLANG_CHECK=1` (see R10 in the plan) |

## Pipeline attestation (T16)

The three-platform install attestation is driven by the **Azure DevOps pipeline** at `azure-pipelines.yml` (Stage 3 `BuildTestMatrix`), which runs the full install + test flow on Ubuntu / macOS / Windows agents from zero, pinned to `RIDGE_REPO=https://github.com/ridge-lang/ridge` while the canonical `ridge-lang/ridge` repo is closed.

T16 is reduced to **~1 h of human work**: trigger the pipeline, review the published logs and artefacts, and record a per-platform sign-off in `tools/install/ATTESTATIONS.md`.  The Stage 4 `ReleaseGate` Environment provides an Azure DevOps approval checkpoint humans attach to.

See `.azure-pipelines/README.md` for stage-by-stage pipeline documentation.
