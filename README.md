# Ridge

> A general-purpose programming language built around developer experience,
> safety from the root, first-class performance, and approachability.

**Status:** 0.1.0 in development. Not yet usable. See [`docs/spec.md`](docs/spec.md)
for the full language specification and development roadmap.

## Key characteristics

- Compiled to Core Erlang (0.1.0), WebAssembly (0.3.0+), native via LLVM (0.4.0+)
- Statically typed with Hindley-Milner inference
- Immutable by default; mutable state confined to actors
- Actor-first concurrency
- 9 capabilities (`io`, `fs`, `net`, `time`, `random`, `env`, `proc`, `spawn`, `ffi`)
  visible in every function signature
- Workspace model with architectural rules enforced by the compiler
- No `null` — `Option` and `Result` are the only way to express optionality and failure

## Elevator pitch

> Ridge is the only language where your architecture and your effects live in
> the type system, not in your PR reviews.

## Example

```ridge
fn io main () =
    Io.println "Hello, World"
```

See [`examples/`](examples/) for more sample programs.

## Prerequisites (for contributors)

- Rust 1.75+
- Erlang/OTP 26+ (`erl`, `erlc` on PATH)

## Building

```sh
cargo build --all
cargo test --all
```

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
