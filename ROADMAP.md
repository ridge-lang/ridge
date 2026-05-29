# Ridge Roadmap

Ridge is a typed functional language for the BEAM. WebAssembly and
native (LLVM) backends remain on the roadmap as exploratory work
behind a target-neutral intermediate representation; neither is
committed to a fixed schedule. This document tracks what has shipped,
what is in flight, and what is planned through 1.0.0. It is updated at
every release cut and is intentionally honest about gaps.

The project is pre-1.0 and experimental. The language, its standard
library, and its toolchain are subject to breaking changes between
minor versions. Patch releases within a `0.x.y` line will not introduce
breaking changes. Treat anything not yet marked ✅ as in motion.

For the canonical language definition, see [`docs/spec.md`](docs/spec.md).
The multi-target framing follows [`docs/spec.md` §14](docs/spec.md):
BEAM-primary, with exploratory backends gated on user traction.

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
| 🟡 | `net.http` | Minimal client (`get`, `post`, `put`, `delete`) and server (`listen`, `respond`); web-layer hardening defaults (`Sql`/`Html`/`SecureCookie` newtypes, default CSP/HSTS on `respond`) shipped in 0.2.6 (see the maintenance section) | `crates/ridge-stdlib/tests/net_http_test.rs` |

### Distribution

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ✅ | Install scripts | `tools/install/install.sh` (POSIX) and `install.ps1` (PowerShell). SHA256 on every download; opportunistic Sigstore verification when `cosign` is on PATH; expected version derived at runtime (no hardcoded literals) | `.github/workflows/install-smoke.yml` runs end-to-end on Ubuntu 22.04, macOS 14, Windows 2022 on every release publish and on every PR touching install scripts |
| ✅ | Release pipeline | 4-target cross-compile (`x86_64-unknown-linux-gnu`, `x86_64-apple-darwin` from `macos-14`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`); ad-hoc macOS sign; archives + SHA256s; Sigstore keyless signing via OIDC; draft GitHub Release | `.github/workflows/release.yml`; exercised by every rc and v0.2.0 |
| 🟡 | Marketplace attestation | Windows 11 + VS Code 1.120.0 attested 2026-05-20. Linux and macOS rows still pending -- recipe documented, platforms not yet signed off | `docs/marketplace-attestation.md` |
| 🟡 | CI workflow | `cargo build --workspace --locked`, `cargo test --workspace --no-fail-fast --locked`, `cargo fmt --check`, `cargo clippy` on Ubuntu against Rust 1.88 + Erlang/OTP 26. Clippy runs with `continue-on-error: true`; cross-platform CI beyond install-smoke is 0.2.x work | `.github/workflows/ci.yml` |

---

## 0.2.x maintenance

The 0.2.x line runs longer than a typical patch lane. Beyond bugfixes
and small improvements it carries the language and standard-library
work that closes the largest gaps with mainstream typed languages
before the 0.3.0 cut: typeclasses, inline record types, a richer
testing surface, and additional capability-bearing modules. Items in
this section are scheduled, not aspirational.

### Cross-platform attestation

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| 🟡 | Linux row in Marketplace attestation | Headless install + version-listing verify automated on Ubuntu 22.04 every release publish. Visual signoff (syntax highlighting + live diagnostics rendered in the editor) still pending from a human on real hardware | `.github/workflows/marketplace-attest.yml`; `docs/marketplace-attestation.md` |
| 🟡 | macOS row in Marketplace attestation | Headless install + version-listing verify automated on macos-14 (Apple Silicon) every release publish. Visual signoff still pending from a human on real hardware | `.github/workflows/marketplace-attest.yml`; `docs/marketplace-attestation.md` |

### Tooling and CI

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ✅ | Tighten clippy to hard-fail | The clippy step in `ci.yml` runs without `continue-on-error`; warnings break the build | `.github/workflows/ci.yml` |
| ✅ | Cross-platform CI matrix | `cargo build`, `cargo test`, `cargo fmt --check`, and `cargo clippy -D warnings` run on Ubuntu 22.04, macOS 14, and Windows 2022 on every PR and push to `main` | `.github/workflows/ci.yml` |

### Standard library

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ✅ | `std.net.http` hardening defaults | `Sql` and `Html` newtypes that escape on construction; `SecureCookie` with `Secure` + `HttpOnly` + `SameSite=Lax` defaults; default CSP / HSTS headers on `respond`. Shipped across 0.2.6 | `CHANGELOG.md` (0.2.6 entries) |
| ✅ | Open `ToText` dispatch for interpolation | User-defined `toText` participates in string interpolation. Shipped in 0.2.6; the typeclass formalisation lands with the typeclass core (below) | `CHANGELOG.md` (0.2.6 entries) |
| ✅ | Bounded mailboxes + backpressure for actors (drop newest, error) | `mailbox` actor member with two overflow policies. `drop oldest` parses but is type-check-rejected pending a broker process intermediary | `docs/spec.md §7.2.1`; `CHANGELOG.md` (0.2.7 entries) |
| ✅ | Mailbox observability API (`mailboxSize`) | `Actor.mailboxSize : Handle a -> Option Int`. `peek` and `drain` deferred until typeclass-derived message typing is available | `docs/spec.md §7.2.1`; `crates/ridge-stdlib/stdlib/actor.ridge` |
| ⏳ | Mailbox `drop oldest` policy + broker | Sliding-window overflow handling via a broker process intermediary. Parsed in 0.2.7 but type-check-rejected pending implementation | `docs/spec.md §7.2.1` |
| ⏳ | `std.crypto` | SHA-2 / SHA-3 hashes, HMAC, AEAD (ChaCha20-Poly1305 default), constant-time compare. Thin bridges to the BEAM `:crypto` module | — |
| ⏳ | `std.uuid` | UUIDv4 (random) and UUIDv7 (timestamp-ordered), plus `toText` / `fromText` round-trips | — |
| ⏳ | `std.url` | RFC 3986 parser and builder, query-string encoding/decoding, `Url` normalisation | — |
| ⏳ | `std.log` | Structured logging (`key=value` fields), level filtering, backend-agnostic emit | — |
| ⏳ | `std.test+` | Extends `std.test` baseline with assertion family, property-based primitives (generators, automatic shrinking), mocking helpers, and a snapshot framework | — |

### Language polish

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ✅ | Test-discovery via `@test` | `@test "<name>"` accepted alongside `pub fn test_*`; both forms recognised with `C304 PrefixTestDeprecated` per prefix test; `ridge fmt --migrate-tests` one-shot migration. Prefix removed in 0.3.0 GA. Shipped in 0.2.8. | `docs/spec.md §8.8`, `CHANGELOG.md` (0.2.8) |
| ✅ | Multi-line and raw string literals | `"""..."""` cooked with dedent; `r"..."` / `r#"..."#` raw without dedent. Shipped in 0.2.8. | `docs/spec.md §4.1.1`, `CHANGELOG.md` (0.2.8) |
| ✅ | Rest patterns in list and record patterns | `[first, ..]`, `[.., last]`, `[first, rest @ .., last]`; `User { name, .. }`. Shipped in 0.2.8. | `docs/spec.md §4.5`, `CHANGELOG.md` (0.2.8) |
| ⏳ | Inline record types | First-class structural record types in type positions (e.g. `{ name: Text, age: Int }`), with the structural-vs-nominal decision documented in the decision log | — |
| ⏳ | Typeclasses (`class` / `instance` / `deriving` / `where`) | Keywords are already reserved in the lexer and parser. The 0.2.x typeclass cut adds declaration syntax, instance resolution, deriving for `Eq` / `Ord` / `Show`, and constraint propagation through inference. Formalises the `ToText` dispatch added in 0.2.6 | — |

### LSP enhancements (bridging into 0.3.0)

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ⏳ | Hover with inferred types and capability annotations | `tower-lsp` server already in place; no `hoverProvider` advertised yet -- the `initialize` reply explicitly omits it | `crates/ridge-lsp/src/server.rs`; asserted absent by `tests/lsp_replay.rs` |
| ⏳ | Go-to-definition | -- | -- |
| ⏳ | Completion | -- | -- |

Each LSP item above lands in a 0.2.x patch when the cost to ship is
bounded. Work that grows to require an IR-level symbol index or
incremental compilation slides to 0.3.0 RC1.

---

## 0.3.0 -- LSP at scale + frameworks tier-1

Goal: graduate the developer experience and ship the first wave of
official frameworks. The release builds in four release candidates,
each shippable on its own and accumulating into the GA tag. The
exploratory WebAssembly and native backends remain out of scope for
0.3.0 -- see the [exploratory backends](#beyond-030--exploratory-backends)
section.

### 0.3.0 RC1 -- LSP incremental + features

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ⏳ | Per-module incremental compilation | Content-hash module cache layered over `check_workspace`; the recompile granularity drops from the full workspace to the changed module set | `crates/ridge-lsp/src/lib.rs` |
| ⏳ | Hover with inferred types and capability annotations | -- | -- |
| ⏳ | Go-to-definition | Cross-module aware | -- |
| ⏳ | Completion | Context-aware: type positions vs expression positions | -- |
| ⏳ | Find-references | Reverse-index over `Symbol -> Vec<Span>` | -- |
| ⏳ | Latency budget | Synthetic 200-module bench under 200 ms p50, under 500 ms p99 | `crates/ridge-lsp/tests/lsp_replay.rs` (extended) |

### 0.3.0 RC2 -- `ridge.web`

Typed HTTP framework for the BEAM. Cowboy / Bandit underneath,
exposed through a typed router DSL, JSON via `Encode` / `Decode`
typeclasses, and a composable middleware chain.

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ⏳ | Typed router | Routes declared via typeclass instances; path, method, handler in the type | -- |
| ⏳ | Request / response typed extensions | Query / header / body parsing typed end to end | -- |
| ⏳ | Middleware composition | Auth, logging, rate-limit, CORS as composable middleware | -- |
| ⏳ | Typed JSON | `Encode` / `Decode` typeclasses, deriving-friendly | -- |
| ⏳ | WebSocket support | Cowboy upgrade path | -- |
| ⏳ | Server-Sent Events | Real-time push without the WebSocket lifecycle | -- |
| ⏳ | Adapter trait | Cowboy primary, Bandit alternative | -- |
| ⏳ | Examples corpus | `examples/web/blog/` + `examples/web/realtime-chat/` | -- |
| ⏳ | Getting-started doc | `docs/frameworks/ridge-web.md` | -- |

### 0.3.0 RC3 -- `ridge.data`

Typed query and migration framework for the BEAM. Postgres is the
first driver; MySQL and SQLite stay on the table for later minors.

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ⏳ | Connection pool | `gen_server`-backed connection lifecycle | -- |
| ⏳ | Typed query builder | `Select<Row, Conditions>`, `Insert<Row>`, ... | -- |
| ⏳ | Migrations | Apply / rollback / version tracking | -- |
| ⏳ | Postgres driver | Either `epgsql` or `pgo`; the choice is captured in the decision log | -- |
| ⏳ | Row deriving | `deriving (Decode, Encode)` on user records | -- |
| ⏳ | Transactions | With savepoints | -- |
| ⏳ | Examples corpus | `examples/data/users-crud/` with docker-compose Postgres in CI | -- |
| ⏳ | Getting-started doc | `docs/frameworks/ridge-data.md` | -- |

### 0.3.0 GA -- `ridge.obs` + `ridge.test+` + housekeeping

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| ⏳ | `ridge.obs.Logger` | Structured logging with pluggable backends (Telemetry default, OTel optional) | -- |
| ⏳ | `ridge.obs` metrics | Counter, Histogram, Gauge primitives | -- |
| ⏳ | `ridge.obs.Trace` | Distributed tracing spans with W3C Trace Context propagation | -- |
| ⏳ | `ridge.test+` wire-up | Adoption guide and migration of stdlib tests where applicable | -- |
| ⏳ | Remove `pub fn test_*` discovery | The 0.2.x deprecation cycle completes here; only `@test "<name>"` remains | -- |
| ⏳ | Capability set audit | Go / no-go on each of the nine capabilities, with a documented rationale for every decision | -- |

---

## Beyond 0.3.0 -- exploratory backends

WebAssembly and native (LLVM) backends remain on the roadmap as
exploratory work, not committed to a fixed schedule. The decision on
whether and when to activate either is re-evaluated 18 months after
the 0.3.0 GA tag, with a mid-cycle checkpoint at 9 months
post-0.3.0 GA. The criteria are described in
[`docs/spec.md` §14.4](docs/spec.md): concrete user traction signals,
maintainer capacity, and the state of the WebAssembly and native
ecosystems at the time.

Both backends today have stub codegen crates
(`crates/ridge-codegen-wasm/`, `crates/ridge-codegen-llvm/`) that
compile against every PR and are guarded by the target-neutrality
test (`crates/ridge-lower/tests/neutrality.rs`). The discipline costs
roughly a 5% tax on lowering work and preserves the option without
dictating a delivery date.

### WebAssembly (exploratory)

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| 🔄 | `ridge-codegen-wasm` (IR → WebAssembly module) | Crate is a stub; the module compiles against every PR but emits no real WebAssembly today | `crates/ridge-codegen-wasm/` |
| ⏳ | WASM limited | Pure code + deterministic capabilities (`time`, `random` via host shims), single-threaded, no actors or async I/O. Target use cases: in-browser playground and stateless edge functions | `docs/spec.md §14.2.1` |
| ⏳ | WASM complete | Actors via the WASM threads proposal, WASI for `fs` / `net` / `proc`, WASM GC where available. Target use cases: production edge computing | `docs/spec.md §14.2.1` |
| ⏳ | Host-shim contract | Specification of the import surface a host must provide for the limited phase | -- |
| ⏳ | Browser playground harness | In-page compile and execute | -- |
| ⏳ | wasmtime CI lane | Smoke tests under `crates/ridge-codegen-wasm/tests/` exercised by an additional workflow | -- |

### Native via LLVM (exploratory)

| Status | Item | Description | Evidence |
|--------|------|-------------|----------|
| 🔄 | `ridge-codegen-llvm` (IR → LLVM IR) | Crate is a stub | `crates/ridge-codegen-llvm/` |
| ⏳ | Custom runtime | Scheduler, garbage collector, data representation, FFI, concurrency primitives, debugging and observability. Comparable in effort to the BEAM frontend, effectively a second compiler | `docs/spec.md §14.2.2` |
| ⏳ | Native target use cases | Compute-bound workloads, fast CLI startup (target under 10 ms vs the 50-100 ms typical of BEAM), standalone binaries without the Erlang runtime, embedded environments | `docs/spec.md §14.2.2` |

---

## 1.0.0 -- Stable

Goal: a stable language, standard library, and tooling surface, with
explicit semver guarantees from the 1.0 tag forward.

### What 1.0.0 commits to

| Surface | Commitment |
|---------|------------|
| **Language** | Syntax, semantics, and the nine-capability set are frozen across the 1.x line. Pattern-matching exhaustiveness, type inference rules, actor message semantics, and the implicit prelude are locked. |
| **Standard library** | Every `pub` function in the pre-1.0 stdlib that survives the 1.0 review is part of the 1.x compatibility surface. Items dropped during the stabilisation pass are listed in the 1.0.0 release notes. |
| **IR (minimum bar)** | The Ridge Core IR is the contract between frontend and backends. Backwards-incompatible IR changes require a major bump. Backward-compatible additions (new node kinds with default lowering) are allowed within 1.x. |
| **Compiled-artefact wire format** | A `.beam` produced by 1.0.0 loads against the 1.x stdlib. If a second backend has been activated by 1.0.0, its artefact contract is documented at that time and held under the same compatibility policy. |
| **CLI** | Subcommand names, flag names, and exit codes are part of the 1.x compatibility surface. |

### What 1.0.0 does not commit to

| Surface | Reason |
|---------|--------|
| Internal Rust API of `ridge-*` crates | Crates remain `publish = false`; downstream tooling depends on the binaries, not the crates. |
| Text of diagnostic messages | Error codes (`R013`, `T001`, `P008`, `M005`, ...) are stable; the human-readable strings around them are not. |
| Intermediate file formats | `.core`, internal AST snapshots, IR serialisation -- these are implementation artefacts. |
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
pre-1.0 stabilisation pass and the final list locked in a dedicated
1.0 proposal document before the tag is cut.

---

## Strategic principles

These principles are inherited from
[`docs/spec.md` §14.5](docs/spec.md) and govern every decision on this
roadmap.

1. **BEAM is the production target.** The language and tooling ship
   against BEAM; alternative backends do not gate any 0.x release.
   New features are designed against BEAM first.
2. **The IR is the contract.** Backends consume the Ridge Core IR and
   need nothing else from the frontend. Target neutrality is asserted
   by `crates/ridge-lower/tests/neutrality.rs` and gated by stub
   compilations against the `ridge-codegen-wasm` and
   `ridge-codegen-llvm` crates on every PR.
3. **Capabilities are target-agnostic.** Capability tracking is a
   compile-time check with zero runtime cost on any target. A function
   declared `fn io` carries the same meaning whether it lowers to
   Erlang or, later, to any other backend.
4. **Pre-1.0, breaking changes are expected.** Minor versions
   (`0.2 → 0.3`, `0.3 → 0.4`, ...) may break source compatibility,
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
