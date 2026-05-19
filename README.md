# Ridge

[![CI](https://github.com/ridge-lang/ridge/actions/workflows/ci.yml/badge.svg)](https://github.com/ridge-lang/ridge/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/ridge-lang/ridge?include_prereleases&label=latest)](https://github.com/ridge-lang/ridge/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.88+-orange.svg)](https://www.rust-lang.org/)
[![BEAM](https://img.shields.io/badge/BEAM-OTP%2026+-purple.svg)](https://www.erlang.org/)

A typed functional language for the BEAM. Hindley-Milner inference, row
polymorphism, actor-first concurrency, and effects tracked in the type
system. Compiles to BEAM bytecode via Core Erlang.

**Status:** 0.2.0-rc4 — release candidate. The language and tooling are
usable end-to-end, but the surface area is still moving. Expect breaking
changes between release candidates. See [`CHANGELOG.md`](CHANGELOG.md) for
what landed and [`docs/spec.md`](docs/spec.md) for the full language
specification.

## What you get

- Statically typed with Hindley-Milner inference and row polymorphism
- Compiled to BEAM bytecode via Core Erlang
- Nine first-class capabilities (`io`, `fs`, `net`, `time`, `random`, `env`,
  `proc`, `spawn`, `ffi`) visible in every function signature
- Immutable by default; mutable state confined to actors
- Actor-first concurrency
- Workspace model with architectural rules enforced by the compiler
- No `null` — `Option` and `Result` are the only way to express optionality
  and failure
- LSP server (diagnostics, hover, go-to-definition) and VS Code extension
- Built-in test runner, formatter, and REPL

## Hello, world

```ridge
fn io main () =
    Io.println "Hello, World"
```

More sample programs live under [`examples/`](examples/).

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

- **VS Code:** install the Ridge extension (TextMate grammar + LSP client).
  Source under [`tools/vscode-ridge/`](tools/vscode-ridge/).
- Any LSP-capable editor can talk to the `ridge-lsp` binary directly.

## Documentation

- [Tutorial](docs/tutorial.md) — install plus a guided first project
- [Language specification](docs/spec.md) — formal definition
- [Grammar (EBNF)](docs/grammar.ebnf) — parser reference
- [Examples](examples/) — runnable sample programs

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
first — it covers branch naming, commit conventions, and how to propose
language-level changes. By participating you agree to the
[Code of Conduct](CODE_OF_CONDUCT.md).

## Security

To report a vulnerability, see [`SECURITY.md`](SECURITY.md).

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
