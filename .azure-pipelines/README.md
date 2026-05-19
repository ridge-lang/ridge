# `.azure-pipelines/` — auxiliary scripts for `azure-pipelines.yml`

This directory holds the per-platform setup and assertion scripts referenced by the **Build, test, and install** stage of the root-level `azure-pipelines.yml`.  Layout:

| File | Used by | Purpose |
|------|---------|---------|
| `setup-toolchain-linux.sh` | Linux job in BuildTestMatrix | Installs Rust (rustup, stable, minimal), Erlang/OTP from the Erlang Solutions repo, build-essential, git, curl on a fresh Ubuntu agent. |
| `setup-toolchain-macos.sh` | macOS job in BuildTestMatrix | Installs Rust (rustup) and Erlang/OTP via Homebrew on a fresh macOS agent. |
| `setup-toolchain-windows.ps1` | Windows job in BuildTestMatrix | Installs Rust (rustup), Erlang/OTP, and git via Chocolatey on a fresh Windows agent. |
| `assert-install-timing.sh` | Linux + macOS jobs | Asserts the Ridge install script ran in `< 300 s` (G2 DoD). |
| `assert-install-timing.ps1` | Windows job | Same, for the Windows install path. |

## Stage-by-stage purpose

The full pipeline (`../azure-pipelines.yml`) is composed of six stages:

| # | Stage | Trigger | Purpose |
|---|-------|---------|---------|
| 1 | `Validate` | every PR + push to main | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo build --all`, `cargo test --all` (Linux + Windows matrix). |
| 2 | `SlowCI_Stdlib` | scheduled (nightly) | `cargo test -p ridge-stdlib --features stdlib-e2e` on three platforms — gated to avoid burning per-PR minutes. |
| 3 | `install_lint` | every PR | `shellcheck install.sh --severity=warning` + `Invoke-ScriptAnalyzer install.ps1 -EnableExit`.  Static lint layer. |
| 4 | `install_dryrun_snapshot` | every PR | Diffs `install.sh --snapshot` and `install.ps1 -DryRun -Snapshot` against `tools/install/expected_dryrun.txt`.  Snapshot layer. |
| 5 | `BuildTestMatrix` | every PR | On Linux + macOS + Windows agents, from zero: install toolchain, run `cargo test --workspace`, build release, exercise install script pinned to `RIDGE_REPO=https://github.com/ridge-lang/ridge`, assert install < 5 min, run smoke tests, exercise the four canonical examples through `ridge build/run/check/fmt --check`, run stdlib `.test.ridge` slow-CI, run escript end-to-end.  Asserts G1, G2, G3, G6, G8 in §11.2. |
| 6 | `ReleaseGate` | only on main branch | Azure DevOps Environment-gated deployment job.  Pauses the pipeline awaiting per-platform human approval; signed-off run-id is recorded in `tools/install/ATTESTATIONS.md`. |

## Toolchain decisions and rationale

### Linux: `esl-erlang` from Erlang Solutions

Ubuntu LTS images (`ubuntu-latest` on Azure DevOps is currently 22.04 / 24.04) ship Erlang/OTP 24-25 in the default `apt` archives.  Ridge requires OTP >= 26 (per the install-script gate).  The Erlang Solutions repo (`packages.erlang-solutions.com`) provides modern OTP (currently 27.x) packaged for Debian/Ubuntu.

The setup script downloads the `erlang-solutions_2.0_all.deb` repository-bootstrap package, runs `dpkg -i` on it, then `apt-get update && apt-get install -y esl-erlang`.  This avoids the deprecated `apt-key` mechanism (Debian 12+ enforces signed-by URLs).

### macOS: Homebrew

Homebrew is the de-facto default on macOS-latest agents and tracks current Erlang/OTP within a few weeks of upstream.  `brew install erlang` is idempotent; `brew upgrade erlang` is the fallback path on a partially provisioned agent.

### Windows: Chocolatey

Chocolatey is the most automatable option for ephemeral Windows agents.  `choco install -y erlang rustup.install git` provides all three prerequisites in one pass.  After `rustup.install`, the script runs `rustup default stable` explicitly so subsequent `cargo` invocations do not error out with "no default toolchain".

`Get-ChildItem -Recurse` locates the actual `erl.exe` directory under `C:\Program Files\` (the version dir is suffixed with the OTP version, which churns), and `##vso[task.prependpath]` exports it for downstream pipeline steps.

## Wiring Azure DevOps to GitHub

The pipeline expects a GitHub service-connection on the `ridge-lang/ridge` repository.  The user owns this wiring; the agent only authors the YAML.

1. **Repo association.**  Pipelines → New pipeline → GitHub → select `ridge-lang/ridge` → existing YAML at `azure-pipelines.yml`.
2. **Service connection.**  The first run prompts for OAuth approval — accept once.  Subsequent runs use the stored connection.
3. **`ReleaseGate` Environment.**  Pipelines → Environments → New environment → name **`ridge-release`** (must match the value in stage 6's `environment:` field).
4. **Approvals on the Environment.**  Open `ridge-release` → Approvals and checks → Add → Approvals → add per-platform reviewers.  The pipeline pauses here until each approver signs off.
5. **Sign-off recording.**  After approval, transcribe approver / timestamp / run-id into `tools/install/ATTESTATIONS.md` (one entry per platform).

## How to reproduce a job locally (best-effort)

Full reproduction requires the platform agent images Azure DevOps uses; no local equivalent is byte-identical.  These commands run the same logical steps on a developer machine:

### Linux

```bash
sudo bash .azure-pipelines/setup-toolchain-linux.sh    # WARNING: installs erlang via apt + rustup
cargo test --workspace
cargo build --workspace --release
RIDGE_REPO=https://github.com/ridge-lang/ridge \
  RIDGE_BRANCH=main \
  bash tools/install/install.sh
ridge --version | grep -F 'ridge 0.1.0'
cargo test -p ridge-cli --test check
cargo test -p ridge-cli --test build
cargo test -p ridge-cli --test fmt
cargo test -p ridge-cli --test run
cargo test -p ridge-stdlib --features stdlib-e2e --release
cargo test -p ridge-codegen-erl --test escript_test
```

### Windows (PowerShell)

```powershell
./.azure-pipelines/setup-toolchain-windows.ps1
cargo test --workspace
cargo build --workspace --release
$env:RIDGE_REPO   = 'https://github.com/ridge-lang/ridge'
$env:RIDGE_BRANCH = 'main'
$duration = Measure-Command { & ./tools/install/install.ps1 }
& ./.azure-pipelines/assert-install-timing.ps1 -Elapsed ([int]$duration.TotalSeconds)
ridge --version
cargo test -p ridge-cli
cargo test -p ridge-stdlib --features stdlib-e2e --release
cargo test -p ridge-codegen-erl --test escript_test
```

## Troubleshooting

| Symptom | Likely cause | Fix |
|---------|--------------|-----|
| `cargo: command not found` after rustup install | The `~/.cargo/bin` directory is not on PATH for the current shell. | The setup script emits `##vso[task.prependpath]` so subsequent steps see it; if reproducing locally, run `. "$HOME/.cargo/env"`. |
| `erl: command not found` on Windows after `choco install` | The Erlang version dir is suffixed (`erl-27.x`) and not picked up by the auto-detect block. | Adjust the `Get-ChildItem` glob in `setup-toolchain-windows.ps1` or set `##vso[task.prependpath]` manually. |
| Agent cache hit but stale `target/` after Cargo.lock change | The cache key includes `Cargo.lock`; if the pipeline restored a key from a stale lock, `cargo build` re-resolves but the cached `target/` may have invalid object files. | Remove the stage's `Cache@2` task to force a clean run; the next run reseeds the key. |
| `install.ps1` fails on `Set-ExecutionPolicy` | Default Azure agents do not require the policy bypass; if running locally on a hardened box, the user must run `Set-ExecutionPolicy -Scope Process Bypass` first. | Per `tools/install/README.md` "Edge cases". |
| `ReleaseGate` never starts | Missing `ridge-release` Environment in Azure DevOps. | Create per "Wiring" section above. |

## References

- `tools/install/README.md` — `RIDGE_REPO` / `RIDGE_BRANCH` env-override contract.
- `docs/T15_G7_MANUAL_ATTESTATION.md` — the remaining manual sign-off step in the build/test/install stage.
