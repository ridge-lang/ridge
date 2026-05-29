<p align="center">
  <img src="assets/logo.svg" alt="Ridge logo" width="160" height="160">
</p>

<h1 align="center">Ridge</h1>

<p align="center">A typed functional language for the BEAM.</p>

<p align="center">
  <a href="https://github.com/ridge-lang/ridge/actions/workflows/ci.yml"><img src="https://github.com/ridge-lang/ridge/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/ridge-lang/ridge/releases"><img src="https://img.shields.io/github/v/release/ridge-lang/ridge?include_prereleases&label=latest" alt="Latest release"></a>
  <a href="https://marketplace.visualstudio.com/items?itemName=ridge-lang.vscode-ridge"><img src="https://img.shields.io/visual-studio-marketplace/v/ridge-lang.vscode-ridge?label=vscode" alt="VS Code Marketplace"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/rust-1.88+-orange.svg" alt="Rust"></a>
  <a href="https://www.erlang.org/"><img src="https://img.shields.io/badge/BEAM-OTP%2026+-purple.svg" alt="BEAM"></a>
</p>

---

Hindley-Milner inference, row polymorphism, actor-first concurrency, and
nine first-class capabilities (`io`, `fs`, `net`, `time`, `random`, `env`,
`proc`, `spawn`, `ffi`) tracked in the type system. Compiles to BEAM
bytecode via Core Erlang.

**Status:** 0.2.7, the seventh release on the 0.2.x line. The
language and tooling are usable end-to-end. Pre-1.0 minors may include
breaking changes; patch releases within 0.2.x will not. See
[`CHANGELOG.md`](CHANGELOG.md) for what landed and
[`docs/spec.md`](docs/spec.md) for the full language specification.

## Why Ridge?

Ridge is meant to be both teachable and shippable: the same language
should carry you from a first-day exercise to a real BEAM service
without swapping dialects.

- The teachable half is the surface. Pure functions, total pattern
  matches, no `null`, and types that double as documentation.
- The shippable half is the runtime. The BEAM gives you preemptive
  scheduling, isolated processes, supervisor trees, and crash-only
  design without a framework on top.

BEAM is the production target. WebAssembly and native (LLVM)
backends are exploratory, kept on the roadmap behind a target-neutral
intermediate representation so they remain feasible without a fixed
schedule. See [`ROADMAP.md`](ROADMAP.md).

## What you get

- Statically typed with Hindley-Milner inference and row polymorphism
- Compiled to BEAM bytecode via Core Erlang
- Nine first-class capabilities visible in every function signature
- Immutable by default; mutable state confined to actors
- Actor-first concurrency
- Workspace model with architectural rules enforced by the compiler
- No `null` &mdash; `Option` and `Result` are the only way to express
  optionality and failure
- LSP server (diagnostics, hover, go-to-definition) and VS Code extension
- Built-in test runner, formatter, and REPL

## A taste of Ridge

Hello, world. The `io` capability in the signature is the type system
recording that this function performs side effects:

```ridge
fn io main () =
    Io.println "Hello, World"
```

Tagged unions with positional payloads and exhaustive `match`:

```ridge
type Shape = Circle Int | Rectangle Int Int

fn area (s: Shape) -> Int =
    match s
        Circle r       -> 3 * r * r
        Rectangle w h  -> w * h
```

More sample programs live under [`examples/`](examples/) and
[`dogfood/`](dogfood/).

## Install

Pre-built binaries are available for Linux, macOS, and Windows. Install
scripts download the release archive, verify its SHA256, and place `ridge`
and `ridge-lsp` on your PATH. If no binary exists for your platform the
scripts fall back to `cargo install`.

**Linux / macOS**

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/ridge-lang/ridge/main/tools/install/install.sh)"
```

**Windows (PowerShell)**

```powershell
& ([scriptblock]::Create((iwr -useb 'https://raw.githubusercontent.com/ridge-lang/ridge/main/tools/install/install.ps1').Content))
```

Full install notes, environment overrides, and troubleshooting live in
[`tools/install/README.md`](tools/install/README.md).

## Quickstart

```sh
ridge new hello            # scaffold a new project
cd hello
ridge run                  # build and run
ridge test                 # run the test suite
ridge fmt                  # format .ridge files
ridge repl                 # interactive REPL
```

The full walk-through, including project layout and capability declarations,
is in [`docs/tutorial.md`](docs/tutorial.md).

## Editor support

- **VS Code:** install the [Ridge
  extension](https://marketplace.visualstudio.com/items?itemName=ridge-lang.vscode-ridge)
  from the Marketplace, or run `code --install-extension
  ridge-lang.vscode-ridge`. It bundles a TextMate grammar and an LSP
  client wired to `ridge-lsp`. Source under
  [`tools/vscode-ridge/`](tools/vscode-ridge/).
- Any LSP-capable editor can talk to the `ridge-lsp` binary directly.

## Documentation

- [Tutorial](docs/tutorial.md) &mdash; install plus a guided first project
- [Language specification](docs/spec.md) &mdash; formal definition
- [Grammar (EBNF)](docs/grammar.ebnf) &mdash; parser reference
- [Examples](examples/) &mdash; runnable sample programs
- [Roadmap](ROADMAP.md) &mdash; release plan and what is shipped, in progress, and planned

## Release signing

Release archives are signed with [Sigstore](https://www.sigstore.dev/)
keyless signing via the GitHub Actions OIDC token. Each archive ships with
a `.cosign.bundle` sidecar containing the signature, certificate, and Rekor
transparency-log entry. The install scripts verify the signature
automatically when `cosign` is present (advisory diagnostic `R055` if
`cosign` is missing, fatal `R056` if verification fails). To verify a
release manually, see [Verifying release signatures
manually](tools/install/README.md#verifying-release-signatures-manually).

## Building from source

```sh
cargo build --workspace
cargo test --workspace
```

Prerequisites: Rust 1.88+, Erlang/OTP 26+, git 2.20+. Repository layout and
contributor conventions live in [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Contributing

Pull requests are welcome. Please read [`CONTRIBUTING.md`](CONTRIBUTING.md)
first &mdash; it covers branch naming, commit conventions, and how to
propose language-level changes. By participating you agree to the
[Code of Conduct](CODE_OF_CONDUCT.md).

## Security

To report a vulnerability, see [`SECURITY.md`](SECURITY.md).

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

## Trademarks

"Ridge" and the Ridge logo are trademarks of The Ridge Language
Authors. Apache-2.0 grants code rights; it does not grant trademark
rights. You may use the name and logo unmodified to refer to this
project. You may not use them to imply endorsement of forks, derivative
works, or third-party products without prior written permission.
