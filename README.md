# Ridge

[![CI](https://github.com/ridge-lang/ridge/actions/workflows/ci.yml/badge.svg)](https://github.com/ridge-lang/ridge/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/ridge-lang/ridge?include_prereleases&label=latest)](https://github.com/ridge-lang/ridge/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.88+-orange.svg)](https://www.rust-lang.org/)
[![BEAM](https://img.shields.io/badge/BEAM-OTP%2026+-purple.svg)](https://www.erlang.org/)

> A general-purpose typed functional language for the BEAM, built around
> developer experience, safety from the root, first-class performance, and
> approachability.

**Status:** 0.2.0-rc3 (release candidate). See [`CHANGELOG.md`](CHANGELOG.md)
for what landed, and [`docs/spec.md`](docs/spec.md) for the full language
specification.

## Key characteristics

- Statically typed with Hindley-Milner inference and row polymorphism
- Compiled to BEAM bytecode via Core Erlang
- Nine first-class capabilities (`io`, `fs`, `net`, `time`, `random`, `env`,
  `proc`, `spawn`, `ffi`) visible in every function signature
- Immutable by default; mutable state confined to actors
- Actor-first concurrency
- Workspace model with architectural rules enforced by the compiler
- No `null` — `Option` and `Result` are the only way to express optionality
  and failure
- LSP server (diagnostics, hover, go-to-definition) + VS Code extension
- Built-in test runner, formatter, and REPL

## Elevator pitch

> Ridge is the only language where your architecture and your effects live
> in the type system, not in your PR reviews.

## Hello, world

```ridge
fn io main () =
    Io.println "Hello, World"
```

See [`examples/`](examples/) for more sample programs.

## Install

Cross-platform install scripts are under [`tools/install/`](tools/install/).
Full instructions in [`docs/tutorial.md`](docs/tutorial.md).

```sh
# Linux / macOS — pass the script as an argument; do NOT pipe to a shell.
# The installer's Erlang prereq check reads stdin; piping through `sh` or
# `bash` causes `erl` to consume the script body and the shell to exit
# silently before installing anything.
bash -c "$(curl -fsSL https://raw.githubusercontent.com/ridge-lang/ridge/main/tools/install/install.sh)"

# Windows (PowerShell)
& ([scriptblock]::Create((iwr -useb 'https://raw.githubusercontent.com/ridge-lang/ridge/main/tools/install/install.ps1').Content))
```

## CLI usage

```sh
ridge new my-app          # scaffold a new project
ridge run                 # build and run the current project
ridge test                # run the test suite
ridge fmt                 # format all .rg files
ridge repl                # interactive REPL
```

## Editor support

- **VS Code:** install the Ridge extension (TextMate grammar + LSP client).
  Bundled in [`tools/vscode-ridge/`](tools/vscode-ridge/).
- Any LSP-capable editor can connect to the `ridge-lsp` server binary.

## Documentation

- [Tutorial](docs/tutorial.md) — install + quickstart
- [Language specification](docs/spec.md) — formal definition
- [Grammar (EBNF)](docs/grammar.ebnf) — parser reference
- [Examples](examples/) — runnable sample programs

## Building from source

```sh
cargo build --workspace
cargo test --workspace
```

Prerequisites and conventions: see [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Contributing

PRs welcome. Please read [`CONTRIBUTING.md`](CONTRIBUTING.md) first — it
covers branch naming, commit conventions, and the proposal process for
language changes.

## Security

To report a vulnerability, see [`SECURITY.md`](SECURITY.md).

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
