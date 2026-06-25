# Contributing to Ridge

Ridge is a new programming language. This document explains how to get set up,
the conventions we follow, and how to propose changes.

Please read [`docs/spec.md`](docs/spec.md) first. The spec is the contract.
When code and spec disagree, either the code is wrong or the spec needs a
deliberate update — never a silent drift.

## Prerequisites

- Rust **1.88** or newer (`rustc --version`)
- Erlang/OTP **26** or newer (`erl -version`)
- Git

On Linux/macOS, install Rust with [rustup](https://rustup.rs/). On Windows,
use the `rustup-init.exe` installer.

## Getting started

```sh
git clone https://github.com/ridge-lang/ridge.git
cd ridge
cargo build --workspace
cargo test --workspace
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
│   ├── ridge-typecheck/    # type and capability checker
│   ├── ridge-types/        # type representation
│   ├── ridge-ir/           # Ridge Core IR
│   ├── ridge-lower/        # AST to IR
│   ├── ridge-codegen-erl/  # Core Erlang backend
│   ├── ridge-diagnostics/  # error rendering
│   ├── ridge-driver/       # compilation orchestration
│   ├── ridge-cli/          # `ridge` binary
│   ├── ridge-lsp/          # language server
│   ├── ridge-fmt/          # formatter
│   ├── ridge-manifest/     # workspace manifest parsing
│   ├── ridge-stdlib/       # standard library (Rust + .ridge modules)
│   └── ridge-pkg/          # package manager
├── examples/               # sample Ridge programs (*.ridge)
├── docs/
│   ├── spec.md             # language specification (source of truth)
│   ├── tutorial.md         # install + quickstart
│   ├── grammar.ebnf        # formal EBNF grammar
│   └── hot-reload-design.md
├── tools/
│   ├── install/            # cross-platform install scripts
│   └── vscode-ridge/       # VS Code extension
└── azure-pipelines.yml     # CI (full multi-platform)
```

## Workflow

Ridge follows [GitHub Flow](https://docs.github.com/en/get-started/quickstart/github-flow):

1. Fork the repo and create a feature branch from `main`.
2. Make your changes, with tests where applicable.
3. Push your branch to your fork.
4. Open a pull request against `ridge-lang/ridge:main`.
5. Wait for CI to pass and a maintainer to review.
6. Address review feedback; the maintainer will squash-merge on approval.

`main` is always releasable. All work happens on feature branches.

## Branch naming

| Prefix | When | Example |
|---|---|---|
| `feat/` | New feature | `feat/lsp-semantic-tokens` |
| `fix/` | Bug fix | `fix/typecheck-row-leak` |
| `docs/` | Documentation only | `docs/tutorial-rewrite` |
| `refactor/` | No behavior change | `refactor/extract-resolver` |
| `test/` | Tests only | `test/codegen-snapshots` |
| `ci/` | CI/build changes | `ci/add-clippy-gate` |
| `chore/` | Tooling, deps, misc | `chore/bump-tower-lsp` |

Use kebab-case after the prefix. Keep it short and descriptive.

## Commit messages

Ridge uses [Conventional Commits](https://www.conventionalcommits.org/). Format:

```
<type>(<scope>): <description>

[optional body]

[optional footer]
```

Examples:

- `feat(lsp): add semantic tokens for capabilities`
- `fix(typecheck): row variable leaked across modules`
- `docs(spec): clarify capability subset rules`
- `chore(deps): bump tower-lsp to 0.21`

Types: `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, `ci`, `build`, `perf`, `style`.

Breaking changes: add `!` after the type (`feat(parser)!: change pipe syntax`)
or include a `BREAKING CHANGE:` footer.

Keep the description lowercase, present tense, no trailing period. Wrap body
lines at ~72 characters.

## Coding conventions

### Rust code

- Format with `cargo fmt --all` before committing. CI enforces this.
- Pass `cargo clippy --workspace --all-targets -- -D warnings`. If clippy is
  wrong, document the exception with a narrow `#[allow(...)]` and a comment
  explaining why.
- **No `panic!` under user input.** The compiler must turn bad input into
  diagnostics, never crashes. See `docs/spec.md` §10.4.
- **No `unsafe`.** Forbidden at the workspace level.
- Prefer `Result<T, Vec<Diagnostic>>` over `Option<T>` for fallible compiler
  phases; accumulate errors where it is safe to do so.

### Ridge code (when writing stdlib or examples)

- Follow the idioms in `docs/spec.md` §3.
- Name things per [`docs/naming-conventions.md`](docs/naming-conventions.md).
- File-level doc comment `---...---` describing the module's purpose.
- Capability prefix lists on every function that needs them.
- No `null`, no exceptions, no user-defined operators.
- Pipes go on their own continuation line.

## Testing

- Every crate has tests. `cargo test --workspace` must stay green on `main`.
- Parser and type-checker phases use **snapshot tests** via `insta`.
  Review snapshot diffs carefully: `cargo insta review`.
- Error messages are first-class output. When you change an error message,
  update the snapshot and eyeball the new rendering.

## Pull requests

- Squash-merged by default — keep the PR title clean (it becomes the squash commit message).
- One concern per PR. Split unrelated changes.
- Fill in the PR template completely.
- Include tests for new behavior; include a regression test for bug fixes.
- Update `CHANGELOG.md` under `## [Unreleased]` if the change is user-visible.

## Pull request checklist

- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] New behavior has tests
- [ ] Spec updated if language semantics change
- [ ] `CHANGELOG.md` updated under `## [Unreleased]` if user-visible
- [ ] No new dependencies without justification in the PR description

## Proposing language changes

Changes to the language itself (syntax, semantics, capability set, stdlib
scope) follow a lightweight proposal process:

1. **Open a GitHub Issue** describing the problem and proposed change.
   Label it `proposal`.
2. **Discuss publicly.** Other contributors weigh in. The maintainer
   makes the call after reasonable discussion.
3. **Once accepted**, open a PR that updates the affected spec sections
   plus the implementation in the same PR.

If the spec is silent on something you need, raise an issue labeled
`spec-gap`.

## Code of Conduct

This project adheres to the [Contributor Covenant](CODE_OF_CONDUCT.md).
Be respectful. Focus on the work. Disagree with ideas, not people. When in
doubt, assume good faith.

## License

By contributing, you agree that your contributions are licensed under the
Apache License 2.0 (see [`LICENSE`](LICENSE)).
