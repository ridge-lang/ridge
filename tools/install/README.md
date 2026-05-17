# Ridge Install Scripts

Scripts for installing the Ridge toolchain (`ridge-cli` + `ridge-lsp`) from source.

## Quick install

**Linux / macOS**

```bash
curl -sSf https://ridge-lang.org/install.sh | sh
```

**Windows** (PowerShell)

```powershell
iwr -useb https://ridge-lang.org/install.ps1 | iex
```

If PowerShell script execution is blocked, run first:

```powershell
Set-ExecutionPolicy -Scope Process Bypass
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

## `--dry-run` / `-DryRun` mode

Both scripts support a no-side-effects mode that prints every command they would execute — one per line, prefixed `[dry-run]` — then exits 0 without making any changes.

```bash
# Linux / macOS
sh install.sh --dry-run
```

```powershell
# Windows
.\install.ps1 -DryRun
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
<lines from install.ps1 -DryRun -Snapshot>
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
     "& 'tools/install/install.ps1' -DryRun -Snapshot" | tr -d '\r' > /tmp/ps_snap.txt
   
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

## Environment overrides (`RIDGE_REPO` / `RIDGE_BRANCH`)

Both scripts honour two environment variables that override the repository URL and branch passed to `cargo install --git`:

| Variable | Default | Purpose |
|----------|---------|---------|
| `RIDGE_REPO` | `https://github.com/ridge-lang/ridge` | Git repository URL |
| `RIDGE_BRANCH` | `main` | Git branch / ref |

This is the lever the Phase 8 CI matrix uses to pin installs to the transient public mirror (`https://github.com/ridge-lang/ridge`) until the canonical `ridge-lang/ridge` repository opens in Phase 9 (per the OQ-C014 / D155 disposition rewrite of 2026-05-06).

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

## Edge cases

| Scenario | Behaviour |
|----------|-----------|
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

Per the OQ-C014 / D155 disposition rewrite of 2026-05-06 (Option C), the three-platform install attestation is now driven by the **Azure DevOps pipeline** at `azure-pipelines.yml` (Stage 3 `BuildTestMatrix`), which runs the full install + test flow on Ubuntu / macOS / Windows agents from zero, pinned to `RIDGE_REPO=https://github.com/ridge-lang/ridge` while the canonical `ridge-lang/ridge` repo is closed.

T16 is reduced to **~1 h of human work**: trigger the pipeline, review the published logs and artefacts, and record a per-platform sign-off in `tools/install/ATTESTATIONS.md`.  The Stage 4 `ReleaseGate` Environment provides an Azure DevOps approval checkpoint humans attach to.

See `.azure-pipelines/README.md` for stage-by-stage pipeline documentation.
