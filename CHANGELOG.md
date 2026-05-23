# Changelog

All notable changes to Ridge will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Diagnostic `R023` when a project source tree contains legacy `.rg` files, with a `git mv` renaming hint. Affects all build, check, run, test, and fmt entry points.
- `Int.rem`, `Int.mod`, and the `%` operator wired through `BinOp::Mod` to `std.int.mod`. `Int.rem` is the BEAM truncating remainder (same sign as the dividend); `Int.mod` is mathematical modulo (same sign as the divisor) and matches the canonical FizzBuzz idiom `match n { m when (m % 15) == 0 -> ... }`.
- `Int.pow` and the `^` operator. `^` already had a precedence and a `BinOp::Pow` lowering target in the compiler, but `std.int` exposed no `pow` symbol, so any user program writing `x ^ y` failed at codegen with `E002 NoStdlibBridge`. `pow` is implemented via repeated squaring; negative exponents truncate to `0` to keep the result in `Int`.

### Fixed

- `compile_stdlib_beams` no longer silently emits zero `.beam` files on machines other than the build host. The 0.2.0 binary embedded `env!("CARGO_MANIFEST_DIR")` (a path on the GitHub Actions runner) as the stdlib source directory; on every other machine the path was missing and the bundling pass failed quietly. Any program calling a Ridge-bodied stdlib function — `List.head`, `Option.withDefault`, `Float.parse`, … — crashed at runtime with `undef`. The stdlib sources are now embedded via `include_str!` at compile time and unpacked into the workspace's `OUT_DIR` on every build; bundling failures are surfaced loudly instead of being swallowed.
- `ridge-lsp` no longer advertises `diagnosticProvider` in its `initialize` response. The server emits diagnostics by `client.publish_diagnostics(...)` (push) only and never implemented the pull side, so VS Code logged a `Method not found (-32601)` for every document open and change. The capability is removed; VS Code falls back to push and the error log clears.
- `Float.parse` returns `None` instead of crashing the BEAM with `badarg` when handed an integer-shaped string like `"100"`. The wrapper now goes through `ridge_rt:float_parse/1`, which tries `binary_to_float/1` first and falls back to `float(binary_to_integer/1)` before reporting `None`.
- T017 `RedundantPattern` no longer fires on arms that carry a `when` guard. The exhaustiveness algorithm in `crates/ridge-typecheck/src/exhaustiveness.rs` now skips guarded arms in both the T016 coverage matrix and the T017 prefix matrix, matching Maranget's algorithm. The previous behaviour rejected every canonical guarded `match` (e.g. `match n { m when (m % 15) == 0 -> "FizzBuzz" ; m when (m % 3) == 0 -> "Fizz" ; ... }`) as redundant.
- Non-BIF calls in `when` guards no longer make `erlc` reject the generated Core Erlang. Guards that contain calls outside the BEAM guard-BIF whitelist — e.g. `m when (m % 15) == 0`, which lowers through `std.int:mod/2` — are lifted out of clause-guard position into a nested `case` chain. The whitelist now matches the OTP reference manual exactly, so non-guard `erlang:*` functions (`integer_to_binary`, `list_to_binary`, …) that previously slipped past the loose `module == "erlang"` check are correctly routed through the lift path too.
- Actor handlers invoked via `!` (cast) no longer drop the side-effecting expressions in their body. `lower_handler_body_for_cast` ignored the leaf value when wrapping the `{noreply, V_State}` tuple, so every `Io.println`, `partner ! msg`, and non-assign call disappeared from `handle_cast/2` (state mutations survived because they thread through `V_State<n>` SSA). The wrap now sequences the leaf via `Do { first: val, then: noreply }`, mirroring the `?>` (ask) path.
- `partner ! handler arg1 arg2` now sends `{handler, arg1, arg2}` instead of `{''}`. The lowering of `Expr::Send` only recognised a bare `Expr::Ident` as the handler name and hard-coded `args: Vec::new()`, so every send with arguments emitted an empty 1-tuple that no receiver could pattern-match against. `unfold_send_message` peels the `Call { callee: Ident, args }` shape the parser produces and propagates the args through `IrExpr::Send`.
- Reads of an actor state field that follow a `<-` assign in the same handler invocation now see the new value. Before, `count <- count + 1; Io.println $"count = ${count}"` lowered the second `count` against the pre-assign `V_State`, so the print reported the stale value; the `received == N` checks in collector-style actors silently never matched. Codegen now tracks the current state SSA index on the local scope and retargets `IrExpr::Local { name: "__state" }` to the latest `V_State<n>` after every assign, propagating the per-arm result back to the outer scope after a `Match`.
- `ridge run` streams the BEAM program's stdout to the terminal as it is produced instead of buffering the whole pipe and dumping it at exit. Long-running programs, anything with a `Time.sleep`, and any non-trivial actor flow previously looked like a hang followed by a single output dump. Stdout is now inherited; stderr stays piped so `RunError::ErlExitNonZero` can still surface BEAM crash dumps and warnings.

### Refactor

- `lift_guarded_match` hoists the remaining-arms expression into a `let V_LiftedRest<depth> = fun () -> <rest> end` thunk and replaces the duplicated fall-through references with `apply V_LiftedRest<depth> ()`. The previous shape cloned the rest into both the guard-case wildcard and the outer wildcard, so a chain of `N` lifted arms produced `2^N` copies of the deepest fall-through body.
- Stdlib per-tier scratch workspaces are managed by `tempfile::TempDir`. The directory is removed on every `compile_tier` exit (success, `Err`, or panic), eliminating the `/tmp/ridge_stdlib_tier*_<pid>/` orphans that the old manual cleanup left behind whenever discover, resolve, typecheck, or lower returned `Err`.

### Docs

- `docs/tutorial.md` Troubleshooting section gains a Windows entry covering `chcp 65001`. `Io.println` writes UTF-8 to stdout, but the default Windows console codepage is `cp1252` on most English/Spanish installs, so non-ASCII output rendered as mojibake (`°` → `Â°`, `é` → `Ã©`). The new entry documents both the per-session `chcp 65001` and the system-wide *Use Unicode UTF-8 for worldwide language support* toggle.

### Internal

- `crates/ridge-driver/tests/integration.rs` serialises the five `erl`-touching tests behind a module-level `Mutex` so the PATH-clearing `run_missing_erlang` test no longer races with parallel siblings that spawn `erl`. The earlier workaround — moving the related test to its own binary file — stays in place as defence-in-depth.

## [0.2.0] - 2026-05-20

First public release. Ridge is installable on Linux, macOS, and Windows
via signed prebuilt binaries; the VS Code extension is on the Marketplace
as `ridge-lang.vscode-ridge`.

### Added

- VS Code extension published to the Marketplace as
  [`ridge-lang.vscode-ridge`](https://marketplace.visualstudio.com/items?itemName=ridge-lang.vscode-ridge).
  Install with `code --install-extension ridge-lang.vscode-ridge` on any
  platform; first publish is v0.2.0. Three-platform install attestation
  in [`docs/marketplace-attestation.md`](docs/marketplace-attestation.md).
- VS Code extension prepared for Marketplace publication: Ridge brand
  icon (128×128 PNG with SVG vector source traced from the master),
  `galleryBanner` and `keywords` metadata, `homepage` / `bugs` / `license`
  fields, and an `Apache-2.0` `LICENSE` shipped inside the extension
  package. Extension version bumped from `0.1.0` to `0.2.0` to track the
  language release. Extension README rewritten as a Marketplace listing.

### Changed

- **BREAKING:** Source-file extension renamed from `.rg` to `.ridge`. Resolves a registry collision with Rouge on GitHub Linguist and avoids ambiguous syntax highlighting on github.com. Existing projects must rename their `.rg` files to `.ridge` and update `entry = "src/Main.rg"` in `ridge.toml` to `entry = "src/Main.ridge"`; the CLI no longer recognises `.rg` files.
- Install scripts no longer hardcode the expected version. Both `install.sh` and `install.ps1` now derive the version they validate against from `RIDGE_VERSION` (release-download path) or from `Cargo.toml` (cargo-install path). Future release cuts only need to bump `Cargo.toml` line 6 plus the resulting `Cargo.lock` regeneration; the eight hardcoded version strings the scripts previously carried are gone.

### CI

- `.github/workflows/vscode-publish.yml` packages the extension on every PR touching `tools/vscode-ridge/**` and publishes to the Marketplace via manual `workflow_dispatch` with a `publish` checkbox. The `VSCE_PAT` secret must be configured under repo settings before the first dispatched publish.
- `install-smoke.yml` gains `pull_request` (paths-filtered to `tools/install/**`, `Cargo.toml`, `Cargo.lock`, and itself) and `workflow_dispatch` triggers so install-script changes validate on Linux, macOS, and Windows before merging instead of only at release-publish time.

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

[Unreleased]: https://github.com/ridge-lang/ridge/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/ridge-lang/ridge/compare/v0.2.0-rc4...v0.2.0
[0.2.0-rc4]: https://github.com/ridge-lang/ridge/releases/tag/v0.2.0-rc4
[0.2.0-rc3]: https://github.com/ridge-lang/ridge/releases/tag/v0.2.0-rc3
[0.2.0-rc2]: https://github.com/ridge-lang/ridge/releases/tag/v0.2.0-rc2
[0.2.0-rc1]: https://github.com/ridge-lang/ridge/releases/tag/v0.2.0-rc1
