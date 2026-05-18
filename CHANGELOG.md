# Changelog

All notable changes to Ridge will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **BREAKING:** Source-file extension renamed from `.rg` to `.ridge`. Resolves a registry collision with Rouge on GitHub Linguist and avoids ambiguous syntax highlighting on github.com. Existing projects must rename their `.rg` files to `.ridge` and update `entry = "src/Main.rg"` in `ridge.toml` to `entry = "src/Main.ridge"`; the CLI no longer recognises `.rg` files.

## [0.2.0-rc4] - 2026-05-18

Release candidate adding Sigstore keyless signing for release artifacts and
opportunistic signature verification in the install scripts. Integrity guarantees
remain SHA256-anchored when `cosign` is unavailable.

### Added

- Sigstore keyless signing in `release.yml`: every release archive is signed with `cosign sign-blob --yes --bundle`, producing a `.cosign.bundle` sidecar (signature, certificate, and Rekor transparency-log entry) uploaded next to the archive and its SHA256
- `install.sh` and `install.ps1` opportunistically download the `.cosign.bundle` and, when `cosign` is on PATH, verify it with `cosign verify-blob` pinned to the `ridge-lang/ridge` release workflow identity and the GitHub Actions OIDC issuer
- "Verifying release signatures manually" section in `tools/install/README.md` with the full `cosign verify-blob` recipe

### Security

- Release artifacts are now cryptographically signed and logged to the Rekor public transparency log, providing tamper-evident provenance in addition to SHA256 integrity
- Installer pins the verification identity to `https://github.com/ridge-lang/ridge/.github/workflows/release.yml@refs/tags/v*` and the OIDC issuer to `https://token.actions.githubusercontent.com`, so a signature minted by any other workflow or fork is rejected

### Changed

- New advisory codes in the installer output: `R055` when `cosign` is not on PATH (signature check skipped, SHA256 still enforced) and `R056` when `cosign verify-blob` fails (installation aborts)
- `release.yml` job permissions now include `id-token: write` so the runner can mint the OIDC token Sigstore exchanges for a short-lived signing certificate

## [0.2.0-rc3] - 2026-05-18

Release candidate cut to align release artifacts with the install-script
fixes landed in rc2. The rc2 binaries predated `ridge-lsp --version`,
which broke the cross-platform install-smoke verification.

### Added

- Install-smoke CI workflow validating `install.sh` / `install.ps1` end-to-end on Ubuntu, macOS, and Windows on every published release
- `ridge-lsp --version` flag for parity with `ridge --version`
- Post-install verification: both installers now confirm `ridge-lsp` and `ridge` report matching versions

### Fixed

- `install.sh` no longer exits silently when invoked via `curl … | sh` in CI. Root cause: the script's Erlang prerequisite check (`erl -noshell -eval …`) reads stdin, and when bash itself was reading the script from stdin, `erl` consumed the still-unread bytes and bash hit EOF before printing anything. Smoke workflow now downloads to a file and runs `bash -x` on it.
- `install.ps1` `exit N` calls inside `iex`/scriptblock no longer kill the host PowerShell session. Refactored to `throw` + `return` wrapped in `& { ... }` with try/catch that propagates `$LASTEXITCODE`.
- `install.ps1` no longer fails under `iwr | iex` due to `param()` blocks or UTF-8 BOM. Options now come from env vars (`$env:RIDGE_DRY_RUN`, etc.) and the file is BOM-free.
- macOS x86_64 release artifact builds via cross-compile from the `macos-14` (M1) runner instead of the deprecated `macos-13` image
- Windows install: `ridge-lsp.exe` extraction no longer fails when an existing VS Code LSP child has the binary locked (pre-flight stop + `Test-WriteAccess`)

## [0.2.0-rc2] - 2026-05-17

First release built by the cross-platform release pipeline. Superseded by rc3 — its `ridge-lsp` binary lacked the `--version` flag, breaking the smoke workflow's verify step.

## [0.2.0-rc1] - 2026-05-17

Initial public release candidate.

### Added

- Typed functional language for the BEAM with Hindley-Milner inference and row polymorphism
- Nine first-class capabilities (`io`, `fs`, `net`, `time`, `random`, `env`, `proc`, `spawn`, `ffi`) visible in every function signature
- Actor-first concurrency with mutable state confined to actors
- Compiler to BEAM bytecode via Core Erlang
- LSP server with diagnostics and correct file attribution
- Command-line tooling: `ridge run`, `ridge test`, `ridge fmt`, `ridge repl`, `ridge new`
- Workspace model with `git` and `path` dependencies
- VS Code extension (TextMate grammar + LSP client)
- Standard library: `bool`, `cli`, `env`, `float`, `fs`, `int`, `io`, `json`, `list`, `map`, `net.http`, `option`, `proc`, `random`, `text`, `time`
- Apache-2.0 licensed

[Unreleased]: https://github.com/ridge-lang/ridge/compare/v0.2.0-rc4...HEAD
[0.2.0-rc4]: https://github.com/ridge-lang/ridge/releases/tag/v0.2.0-rc4
[0.2.0-rc3]: https://github.com/ridge-lang/ridge/releases/tag/v0.2.0-rc3
[0.2.0-rc2]: https://github.com/ridge-lang/ridge/releases/tag/v0.2.0-rc2
[0.2.0-rc1]: https://github.com/ridge-lang/ridge/releases/tag/v0.2.0-rc1
