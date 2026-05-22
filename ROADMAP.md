# Ridge Roadmap

Ridge is a typed functional language for the BEAM, with WebAssembly and
native LLVM backends on the roadmap. This document tracks what has
shipped, what is in flight, and what is planned through 1.0.0. It is
updated at every release cut and is intentionally honest about gaps.

The project is pre-1.0 and experimental. The language, its standard
library, and its toolchain are subject to breaking changes between
minor versions. Patch releases within a `0.x.y` line will not introduce
breaking changes. Treat anything not yet marked ✅ as in motion.

For the canonical language definition, see [`docs/spec.md`](docs/spec.md).
Version targets in this document follow [`docs/spec.md` §14.1](docs/spec.md).

## Status legend

| Symbol | Label | Meaning |
|--------|-------|---------|
| ✅ | Done & verified | Implemented and exercised by automated tests, a CI workflow, install-smoke, or documented cross-platform attestation. Each row cites the evidence. |
| 🟡 | Done, awaiting verification | Implemented, but without automated test coverage on the feature itself or without the corresponding cross-platform attestation having been completed. |
| 🔄 | In progress | Partial implementation visible in the tree (scaffolding, stub crate, half-finished module). Concrete artefacts exist; the surface area is incomplete. |
| ⏳ | Planned | Scoped in the spec but not yet started. |

"Verified" means cited evidence in the tree. Running something once on
a developer laptop does not count.

---

## 0.2.0 — Shipped 2026-05-20

First public release. The language and tooling are usable end-to-end on
Linux, macOS, and Windows via signed prebuilt binaries; the VS Code
extension is published to the Marketplace and Open VSX. See
[`CHANGELOG.md`](CHANGELOG.md) for the release notes.

### Language

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ✅ | Hindley-Milner inference + row polymorphism | Generalisation, unification, instantiation | `crates/ridge-typecheck/tests/snapshots.rs`, `errors.rs` |
| ✅ | Nine first-class capabilities | `io`, `fs`, `net`, `time`, `random`, `env`, `proc`, `spawn`, `ffi` tracked in every signature | `crates/ridge-typecheck/src/caps_{infer,check}.rs`; `tests/capability_leaks.rs` |
| ✅ | Actor-first concurrency | Mutable state confined to actors; `!` async send, `?>` sync ask, gen_server-style handlers | `crates/ridge-codegen-erl/src/actor.rs`; `tests/beam_e2e.rs`; `examples/` |
| ✅ | Pattern matching with exhaustiveness checking | Maranget's algorithm | `crates/ridge-typecheck/src/exhaustiveness.rs`; `tests/fixtures/` |
| ✅ | Implicit prelude | Auto-imports `Option`, `Result`, constructors, and pure-data module aliases (`Int`, `Float`, `Bool`, `Text`, `List`, `Map`, `Set`, `Json`); capability modules remain explicit-import | `crates/ridge-typecheck/src/prelude.rs` |
| ✅ | Pipe `\|>`, string interpolation, doc comments, guards with `else`, qualified imports | Core syntactic surface | `crates/ridge-parser/tests/snapshots.rs`; `tests/fixtures/` |
| ✅ | Workspace model with `[workspace.rules] forbid` | Architectural rules enforced by the compiler | `crates/ridge-resolve/tests/workspace.rs`; `ridge-typecheck/tests/workspace.rs` |
| ✅ | Source-file extension `.ridge` | Renamed from `.rg` to avoid a GitHub Linguist collision with Rouge. BREAKING vs pre-public drafts; CLI no longer recognises `.rg` | `crates/ridge-cli/tests/build.rs`, `run.rs` |

### Compiler

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ✅ | Lexer | Logos-based tokeniser; layout algorithm with `INDENT`/`DEDENT`/`NEWLINE`, in-bracket suppression; doc, raw-string, and interpolation segments | `crates/ridge-lexer/tests/` |
| ✅ | Parser | chumsky-based; error recovery, ariadne-rendered diagnostics, trivia-preserving mode used by the formatter | `crates/ridge-parser/tests/snapshots.rs`, `errors.rs` |
| ✅ | Name resolution | Workspace manifest parsing, module graph, imports, visibility, forbid rules, "did you mean?", partial-AST resolve for LSP | `crates/ridge-resolve/tests/snapshots.rs`, `errors.rs` |
| ✅ | Type + capability checker | Inference, generalisation, capability tracking. See Language section above | (same evidence as Language rows) |
| ✅ | Lowering to Ridge Core IR | Target-neutral contract between frontend and backends | `crates/ridge-lower/tests/snapshots.rs`, `lowering.rs`, `neutrality.rs` |
| ✅ | Core Erlang codegen | Emits `.core`, invokes `erlc` to produce `.beam`. Records → maps, unions → tagged tuples, actors → gen_servers, sends/asks → BEAM messaging | `crates/ridge-codegen-erl/tests/beam_e2e.rs`, `core_text_snapshot.rs`, `core_ast_snapshot.rs`, `escript_test.rs` |
| ✅ | Diagnostics with stable error codes | `P###` parser, `R###` resolver, `T###` type checker, `M###` manifest, `C###` formatter and CLI, `L8##` LSP-specific | `crates/ridge-diagnostics/` plus per-crate emission sites |

### Tooling

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ✅ | CLI (`ridge`) | `build`, `run`, `check`, `fmt`, `new`, `init`, `test`, `repl` | `crates/ridge-cli/tests/{build,run,check,fmt,new,init,repl,test_cmd,parse_error_render}.rs` |
| ✅ | REPL | Bracket-counting auto-continuation; allows all capabilities except `ffi` | `crates/ridge-cli/src/cmd/repl.rs`; `tests/repl.rs` |
| ✅ | Formatter (`ridge fmt`) | Opinionated, zero-config, trivia-preserving round-trip via the parser's trivia-preserving mode | `crates/ridge-fmt/tests/fmt_tests.rs` and fixtures |
| ✅ | Test runner (`ridge test`) | Discovers `pub fn test_*` functions; runs each in a fresh BEAM child; per-test pass/fail reporting | `crates/ridge-cli/tests/test_cmd.rs` |
| ✅ | LSP server (`ridge-lsp`) | stdio transport; diagnostics on open/change/save with 250 ms debounce and in-flight cancellation; correct file attribution; `--version` flag | `crates/ridge-lsp/tests/lsp_replay.rs` (replays initialise/didOpen/didChange against the live server) |
| ✅ | VS Code extension | `ridge-lang.vscode-ridge` v0.2.1 live on Marketplace and Open VSX; TextMate grammar, LSP client, branded icon | `.github/workflows/vscode-publish.yml`; package job runs on every PR touching `tools/vscode-ridge/**` |
| ✅ | Package manager (`ridge-pkg`) | `path = "..."` and `git = { ..., tag/branch = "..." }` dependencies; workspace inheritance; shared cache under the user's data dir. No registry, no semver solver, no lockfile in 0.2.0 | `crates/ridge-pkg/tests/{path,git,cache,version_dep}_test.rs` |

### Standard library

Stdlib modules are written in Ridge under `crates/ridge-stdlib/stdlib/`,
compiled by the same driver as user code, and exposed as compiled
artefacts at runtime. Each module ships a `.test.ridge` file plus a
Rust-level integration test.

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ✅ | Pure data modules | `bool`, `int`, `float`, `text`, `list`, `map`, `set`, `option`, `result`, `json` | `crates/ridge-stdlib/tests/{bool,int,float,text,list,map,set,option,result,json}_test.rs` |
| ✅ | Capability-bearing modules | `cli`, `env`, `fs`, `io`, `proc`, `random`, `time` | `crates/ridge-stdlib/tests/{cli,env,fs,io,proc,random,time}_test.rs` |
| 🟡 | `net.http` | Minimal client (`get`, `post`, `put`, `delete`) and server (`listen`, `respond`); web-layer hardening defaults (`Sql`/`Html`/`SecureCookie` newtypes, default CSP/HSTS on `respond`) unresolved | `crates/ridge-stdlib/tests/net_http_test.rs` |

### Distribution

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ✅ | Install scripts | `tools/install/install.sh` (POSIX) and `install.ps1` (PowerShell). SHA256 on every download; opportunistic Sigstore verification when `cosign` is on PATH; expected version derived at runtime (no hardcoded literals) | `.github/workflows/install-smoke.yml` runs end-to-end on Ubuntu 22.04, macOS 14, Windows 2022 on every release publish and on every PR touching install scripts |
| ✅ | Release pipeline | 4-target cross-compile (`x86_64-unknown-linux-gnu`, `x86_64-apple-darwin` from `macos-14`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`); ad-hoc macOS sign; archives + SHA256s; Sigstore keyless signing via OIDC; draft GitHub Release | `.github/workflows/release.yml`; exercised by every rc and v0.2.0 |
| 🟡 | Marketplace attestation | Windows 11 + VS Code 1.120.0 attested 2026-05-20. Linux and macOS rows still pending — recipe documented, platforms not yet signed off | `docs/marketplace-attestation.md` |
| 🟡 | CI workflow | `cargo build --workspace --locked`, `cargo test --workspace --no-fail-fast --locked`, `cargo fmt --check`, `cargo clippy` on Ubuntu against Rust 1.88 + Erlang/OTP 26. Clippy runs with `continue-on-error: true`; cross-platform CI beyond install-smoke is 0.2.x work | `.github/workflows/ci.yml` |

---

## 0.2.x maintenance

Patch-lane work: bugfixes, deferred-but-decided items, and small
improvements that do not warrant a minor bump. Items in this section
are scheduled, not aspirational.

### Cross-platform attestation

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ⏳ | Linux row in Marketplace attestation | Repeat the six-step recipe from a Linux box (Ubuntu LTS or equivalent) and commit the table update | `docs/marketplace-attestation.md` (pending row) |
| ⏳ | macOS row in Marketplace attestation | Same recipe on an Apple Silicon laptop; verify both `x86_64-apple-darwin` and `aarch64-apple-darwin` install paths | `docs/marketplace-attestation.md` (pending row) |

### Tooling and CI

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ⏳ | Tighten clippy to hard-fail | Remove `continue-on-error: true` from the clippy step in `ci.yml` | `.github/workflows/ci.yml:49` |
| ⏳ | Cross-platform CI matrix | Run `cargo build` and `cargo test` on macOS and Windows runners, not just Ubuntu | `.github/workflows/ci.yml` |

### Standard library

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ⏳ | `std.net.http` hardening defaults | `Sql` and `Html` newtypes that escape on construction; `SecureCookie` with `Secure` + `HttpOnly` + `SameSite=Lax` defaults; default CSP / HSTS headers on `respond` | `docs/spec.md §16.2` |
| ⏳ | Bounded mailboxes + backpressure for actors | Per the deferred-from-0.1.0 list in the spec | `docs/spec.md §16.2` |
| ⏳ | Mailbox observability API | `Actor.mailboxSize`, peek, drain | — |
| ⏳ | Open `ToText` typeclass for interpolation | Allow user-defined types to participate in string interpolation | — |

### Language polish

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ⏳ | Test-discovery sugar | Accept `@test "<name>"` alongside the current `pub fn test_*` convention; both forms recognised in 0.2.x with a `C304 PrefixTestDeprecated` warning per prefix test; `ridge fmt --migrate-tests` one-shot migration ships in the same line. Prefix removed in 0.3.0 | `docs/spec.md §16.2` |
| ⏳ | Multi-line and raw string literals | — | `docs/spec.md §16.2` |
| ⏳ | Range and rest-pattern syntax for `..` | Concrete semantics chosen during 0.2.x | `docs/spec.md §16.2` |

### LSP enhancements (bridging into 0.3.0)

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ⏳ | Hover with inferred types and capability annotations | `tower-lsp` server already in place; no `hoverProvider` advertised yet — the `initialize` reply explicitly omits it | `crates/ridge-lsp/src/server.rs`; asserted absent by `tests/lsp_replay.rs` |
| ⏳ | Go-to-definition | — | — |
| ⏳ | Completion | — | — |

Each LSP item above lands in a 0.2.x patch when the cost to ship is
bounded. Work that grows to require an IR-level symbol index or
incremental compilation slides to 0.3.0.

---

## 0.3.0 — WebAssembly limited

Goal: a second backend producing WebAssembly modules sufficient for a
browser-based playground and stateless edge functions. Actor-bearing
programs do **not** target WASM in 0.3.0; that lands in 0.5.0 once the
WASM threads proposal is broadly available.

Effort estimate per [`docs/spec.md` §14.3](docs/spec.md): 2–3 months
full-time, started in parallel with 0.2.x LSP work.

### WebAssembly backend

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| 🔄 | `ridge-codegen-wasm` (IR → WebAssembly module) | Crate exists but currently contains only the module-level doc comment and a smoke test. The IR is already target-neutral (asserted by `crates/ridge-lower/tests/neutrality.rs`), so bring-up is a backend implementation, not a frontend redesign | `crates/ridge-codegen-wasm/` |
| ⏳ | Pure Ridge code + deterministic capabilities | `time` and `random` bound via host-provided shims | `docs/spec.md §14.3` |
| ⏳ | Host-shim contract for `time` / `random` | Specification of the import surface a host must provide | `docs/spec.md §14.3` |
| ⏳ | Explicit exclusions for 0.3.0 | No actors, no async I/O, no network; single-threaded execution | `docs/spec.md §14.3` |
| ⏳ | Deployment targets | Cloudflare Workers, Fastly Compute@Edge, Fermyon Spin, wasmtime, wasmer, browser (playground + web apps) | `docs/spec.md §14.3` |
| ⏳ | Browser playground harness | Compiles and runs Ridge in-page | — |
| ⏳ | WASM smoke tests + wasmtime CI runner | Under `crates/ridge-codegen-wasm/tests/`, exercised by an additional CI job | — |

### LSP at 0.3.0

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ⏳ | Incremental LSP compilation | Per-module recompile granularity. Today the LSP recompiles the whole workspace on every change via `ridge_driver::check_workspace` (with debounce + cancellation) — the 0.1.0 ceiling is documented in code | `crates/ridge-lsp/src/lib.rs` |
| ⏳ | Full hover / goto / completion / references | Whatever did not ship in 0.2.x | — |

### Language polish at 0.3.0

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ⏳ | Remove `pub fn test_*` discovery | Per the 0.2.x deprecation cycle | — |
| ⏳ | Capability set review | Based on 0.2.x usage data | `docs/spec.md §16.2` |

---

## 0.4.0 — Native alpha (LLVM + custom runtime)

Goal: a third backend producing native executables via LLVM, plus the
runtime that BEAM provides for free today (scheduler, GC, data
representation, concurrency primitives, FFI). Total effort is
comparable to the entire BEAM frontend — effectively a second
compiler — and is started in parallel with 0.3.0 work rather than
strictly after it.

Total effort per [`docs/spec.md` §14.5](docs/spec.md): 12–18 months
full-time with assistance. 0.4.0 is an alpha — the runtime is expected
to be incomplete and the standard library will not yet have full
native ports.

### Native backend components

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| 🔄 | `ridge-codegen-llvm` (IR → LLVM IR) | Crate exists with module doc + smoke test only. Effort: 2–3 months | `crates/ridge-codegen-llvm/`; `docs/spec.md §14.5` |
| ⏳ | Actor scheduler (M:N) | Reference points: Go's runtime, Tokio, BEAM's scheduler. Decision open: preemptive (BEAM fidelity) vs cooperative (simpler, changes long-running-computation semantics). Effort: 4–6 months; correctness is the dominant risk | `docs/spec.md §14.4.1`, §14.5 |
| ⏳ | Garbage collection | Per-actor heaps where possible; global concurrent GC (Go-style) baseline. Reference-counting and ownership rejected. Effort: 3–4 months | `docs/spec.md §14.4.2`, §14.5 |
| ⏳ | Data representation | Lists (linked / vector / persistent HAMT); maps (HAMT); unions (tagged); text (UTF-8 + SSO); records (packed, aligned, field-ordered). Effort: 1–2 months | `docs/spec.md §14.4.3`, §14.5 |
| ⏳ | FFI and system integration | C ABI; bindings to established C libraries (curl, openssl, ...). Effort for basic stdlib coverage: 1–2 months | `docs/spec.md §14.4.4`, §14.5 |
| ⏳ | Concurrency primitives | Thread-safe MPMC channels, scheduler synchronisation, non-blocking timers, async I/O integrated with the custom scheduler. Effort: 1–2 months | `docs/spec.md §14.4.5`, §14.5 |
| ⏳ | Debugging and observability | DWARF info, stack traces with Ridge function names, profiler integration. Effort: 1–2 months; deferrable to a later 0.4.x patch | `docs/spec.md §14.4.6`, §14.5 |
| ⏳ | Linker orchestration | Driver-level wiring of `clang` / `lld` for the final executable. Effort: 2–4 weeks | `docs/spec.md §14.5` |

### What 0.4.0 unlocks

Per [`docs/spec.md` §14.4](docs/spec.md):

- Compute-bound workloads measurably faster than BEAM (the spec quotes
  10–50× — this remains an aspiration until benchmarks land; see the
  benchmark-methodology open question in
  [`docs/spec.md` §16.3](docs/spec.md)).
- Fast CLI startup (target: under 10 ms vs the 50–100 ms typical of
  BEAM).
- Standalone binaries without the Erlang runtime.
- Embedded and constrained environments.

---

## 0.5.0 — Native + WebAssembly complete

Goal: both alternative backends graduate to feature parity with BEAM
for the constructs they support.

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ⏳ | WASM complete | Actors via the WASM threads proposal; WASI for `fs`, `net`, `proc`; WASM GC where available. Additional 3–4 months on top of 0.3.0 | `docs/spec.md §14.3` |
| ⏳ | Native out of alpha | stdlib ports complete; GC and scheduler hardened beyond alpha; debugging and observability promoted out of "optional" | `docs/spec.md §14.4` |
| ⏳ | Cross-target benchmark suite | Comparable numbers across BEAM / native / WASM. Depends on the benchmark-methodology decision | `docs/spec.md §16.3` |
| ⏳ | Observability for actor-bearing programs across targets | Live tracing analogue to `recon` on BEAM; equivalents for native and WASM | — |

At this point Ridge is multi-target production-capable but pre-1.0:
breaking changes are still possible, and stability is not yet promised.

---

## 1.0.0 — Stable

Goal: a stable language, standard library, and tooling surface, with
explicit semver guarantees from the 1.0 tag forward.

### What 1.0.0 commits to

| Surface | Commitment |
|---------|------------|
| **Language** | Syntax, semantics, and the nine-capability set are frozen across the 1.x line. Pattern-matching exhaustiveness, type inference rules, actor message semantics, and the implicit prelude are locked. |
| **Standard library** | Every `pub` function in the 0.5.0 stdlib that survives the 1.0 review is part of the 1.x compatibility surface. Items dropped during the 0.5 → 1.0 pass are listed in the 1.0.0 release notes. |
| **IR (minimum bar)** | The Ridge Core IR is the contract between frontend and backends. Backwards-incompatible IR changes require a major bump. Backward-compatible additions (new node kinds with default lowering) are allowed within 1.x. |
| **Compiled-artefact wire format** | A `.beam` produced by 1.0.0 loads against the 1.x stdlib; a native binary produced by 1.0.0 continues to run against 1.x runtime libraries. |
| **CLI** | Subcommand names, flag names, and exit codes are part of the 1.x compatibility surface. |

### What 1.0.0 does not commit to

| Surface | Reason |
|---------|--------|
| Internal Rust API of `ridge-*` crates | Crates remain `publish = false`; downstream tooling depends on the binaries, not the crates. |
| Text of diagnostic messages | Error codes (`R013`, `T001`, `P008`, `M005`, …) are stable; the human-readable strings around them are not. |
| Intermediate file formats | `.core`, internal AST snapshots, IR serialisation — these are implementation artefacts. |
| Experimental features | Features still marked experimental at the time of the 1.0 cut are opt-in and may change within 1.x. |
| BEAM / OTP versions below the documented minimum | The minimum supported BEAM version is documented at release time and may advance in minor releases. |

### Breaking-change policy from 1.0.0 forward

- Removals and incompatible changes require a major bump (`2.0`) and a
  deprecation cycle of at least one minor version.
- Compiler error codes that previously fired must continue to fire on
  the same constructs across 1.x. New error codes can be added.
- Deprecations are emitted as warnings against a stable warning code
  and accompanied by an automated migration where feasible (analogous
  to the `ridge fmt --migrate-tests` cycle in 0.2.x → 0.3.0).

The exact set of 1.0.0 commitments will be reviewed during the
0.5 → 1.0 stabilisation pass and the final list locked in a dedicated
1.0 proposal document before the tag is cut.

---

## Strategic principles

These principles are inherited from
[`docs/spec.md` §14.6](docs/spec.md) and govern every decision on this
roadmap.

1. **BEAM is first-class; native and WASM are additive.** BEAM is not
   on a deprecation path. New language features are designed against
   BEAM first; native and WASM backends must serve the same semantics.
2. **The IR is the contract.** Backends consume the Ridge Core IR and
   need nothing else from the frontend. Target neutrality is asserted
   by `crates/ridge-lower/tests/neutrality.rs`.
3. **Capabilities are target-agnostic.** Capability tracking is a
   compile-time check with zero runtime cost on any target. A function
   declared `fn io` carries the same meaning whether it lowers to
   Erlang, WebAssembly, or native code.
4. **Pre-1.0, breaking changes are expected.** Minor versions
   (`0.2 → 0.3`, `0.3 → 0.4`, …) may break source compatibility,
   project manifest formats, or stdlib surface. Patches within a
   `0.x.y` line will not. After 1.0.0, the policy in the 1.0.0 section
   above applies.

---

## How to track progress

| Resource | What you'll find there |
|----------|------------------------|
| [`CHANGELOG.md`](CHANGELOG.md) | Every release, every breaking change, every error code introduced. |
| [GitHub Releases](https://github.com/ridge-lang/ridge/releases) | Signed archives, SHA256s, and Sigstore bundles for every tag. |
| [Issues](https://github.com/ridge-lang/ridge/issues) and [Pull Requests](https://github.com/ridge-lang/ridge/pulls) | Work currently in flight. |
| [VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=ridge-lang.vscode-ridge) | Editor extension, current version, install count. |
| [Open VSX](https://open-vsx.org/extension/ridge-lang/vscode-ridge) | Mirror for VSCodium / Cursor / other VS Code derivatives. |

This file is updated at each release cut. If a roadmap item changes
status, the change is reflected here in the same PR that ships the
underlying work.
