# Contributing to Ridge

Ridge is a new programming language in active early-stage development. This
document explains how to get set up, the conventions we follow, and how to
propose changes.

Please read [`docs/spec.md`](docs/spec.md) first. The spec is the contract.
When code and spec disagree, either the code is wrong or the spec needs a
deliberate update — never a silent drift.

## Prerequisites

- Rust **1.75** or newer (`rustc --version`)
- Erlang/OTP **26** or newer (`erl -version`)
- Git

On Linux/macOS, install Rust with [rustup](https://rustup.rs/). On Windows,
use the `rustup-init.exe` installer.

## Getting started

```sh
git clone https://github.com/ridge-lang/ridge.git
cd ridge
cargo build --all
cargo test --all
```

If everything goes green, you're ready to hack. The binary entry point is
`crates/ridge-cli` — `cargo run -p ridge-cli`.

## Repository layout

```
ridge/
├── Cargo.toml              # workspace manifest
├── crates/                 # Rust crates (compiler pipeline)
│   ├── ridge-lexer/        # tokenization + layout
│   ├── ridge-parser/       # AST construction
│   ├── ridge-ast/          # shared AST types
│   ├── ridge-resolve/      # name resolution, imports, workspace rules
│   ├── ridge-types/        # type and capability checker
│   ├── ridge-ir/           # Ridge Core IR
│   ├── ridge-lower/        # AST to IR
│   ├── ridge-codegen-erl/  # Core Erlang backend (0.1.0)
│   ├── ridge-codegen-wasm/ # WebAssembly backend (0.3.0+)
│   ├── ridge-codegen-llvm/ # LLVM backend (0.4.0+)
│   ├── ridge-diagnostics/  # error rendering
│   ├── ridge-driver/       # compilation orchestration
│   ├── ridge-cli/          # `ridge` binary
│   ├── ridge-lsp/          # language server
│   └── ridge-pkg/          # package manager
├── examples/               # sample Ridge programs (*.rg)
├── docs/
│   ├── spec.md             # language specification and roadmap (source of truth)
│   └── grammar.ebnf        # formal EBNF grammar
└── azure-pipelines.yml     # CI
```

Phase 0 crates are scaffolding only. Real implementation arrives phase by
phase per `docs/spec.md` §11.

## Development workflow

1. **Pick a phase task.** Every change must map to a Phase listed in the
   roadmap or to an explicit bug/improvement tracked as an issue.
2. **Branch from `main`.** Branch naming: `phase-N/short-description`,
   `fix/short-description`, or `docs/short-description`.
3. **Work in small, testable increments.** Write a failing test first where
   possible.
4. **Keep commits focused.** One logical change per commit.
5. **Open a pull request.** Link the phase or issue, describe what and why.

## Coding conventions

### Rust code

- Format with `cargo fmt --all` before committing. The CI enforces this.
- Pass `cargo clippy --all-targets --all-features -- -D warnings`. The
  workspace lint preset is strict (`pedantic`, `nursery`, plus
  `unwrap_used`, `expect_used`, `panic` as warnings). If clippy is wrong,
  document it with a narrow `#[allow(...)]` and a comment explaining why.
- **No `panic!` under user input.** The compiler must turn bad input into
  diagnostics, never crashes. See `docs/spec.md` §10.4.
- **No `unsafe`.** Forbidden at the workspace level.
- Prefer `Result<T, Vec<Diagnostic>>` over `Option<T>` for fallible compiler
  phases; accumulate errors where it's safe to do so.

### Ridge code (when writing stdlib or examples)

- Follow the idioms in `docs/spec.md` §3.
- File-level doc comment `---...---` describing the module's purpose.
- Capability prefix lists on every function that needs them.
- No `null`, no exceptions, no user-defined operators.
- Pipes go on their own continuation line.

## Testing

- Every crate has tests. `cargo test --all` must stay green on `main`.
- Parser and type-checker phases use **snapshot tests** via `insta`.
  Review snapshot diffs carefully: `cargo insta review`.
- Error messages are first-class output. When you change an error message,
  update the snapshot and eyeball the new rendering.
- Integration tests live under `tests/` once we get past Phase 2.

## Commit messages

- First line: imperative, ≤ 72 chars. Example: `lexer: handle nested string interpolation`.
- Body (optional): what and why. Reference the phase (`Phase 1`) or issue.
- No trailing periods on the subject line.

## Pull request checklist

- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` passes
- [ ] `cargo test --all` passes
- [ ] New behavior has tests
- [ ] Spec updated if language semantics change
- [ ] No new dependencies without justification in the PR description
  (cross-reference `docs/spec.md` §10.3)

## Proposing language changes

Changes to the language itself (syntax, semantics, capability set,
stdlib scope) go through the **Decision Log** in `docs/spec.md` §15.

1. Open an issue describing the problem and proposed change.
2. Once consensus is reached, add a new `DNNN` entry to the Decision Log
   with the decision, rationale, alternatives considered, and status.
3. Update the affected spec sections in the same PR.

Phase 0 is not the time for syntax debates. If the spec is silent on
something you need, raise an open question in `docs/grammar.ebnf` or as
a GitHub issue labeled `spec-gap`.

## Code of conduct

Be respectful. Focus on the work. Disagree with ideas, not people. When in
doubt, assume good faith.

## License

By contributing, you agree that your contributions are licensed under the
Apache License 2.0 (see [`LICENSE`](LICENSE)).
