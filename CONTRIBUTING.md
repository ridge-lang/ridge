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

Ridge is **issue-first**: every non-trivial change starts as a GitHub Issue
and only becomes a PR after the issue is accepted. This keeps design
discussion in the open, avoids duplicated effort, and gives every PR a
traceable reason to exist.

1. **Search for duplicates first.** Run
   `tools/dev/issue-dupes.sh <keywords>` (or
   `gh issue list --search "<keywords>"`) and skim the results. If your
   topic already has an issue, comment there instead of opening a new one.
2. **Open an issue** with the right template (bug report / feature
   request). It lands with the `triage` label.
3. **Wait for triage.** A maintainer reviews the issue and either asks
   questions, closes it (with a reason), or marks it **`accepted`** —
   usually with `area:*` and `sev:*` labels attached. `accepted` means:
   approved in principle, a PR is welcome.
4. **Open a PR** that references the issue (`Closes #N` in the PR body).
   The PR template asks for this link explicitly.
5. Wait for CI to pass and a maintainer to review.
6. Address review feedback; the maintainer will squash-merge on approval.

**Threshold:** trivial changes — typo fixes, small docs corrections, CI
tweaks, dependency bumps — may go straight to PR without an issue. When in
doubt, open the issue; it costs a minute and saves a rejected PR.

**Maintainers follow the same rule** for non-trivial work: self-filed issue,
self-triage, then PR. The paper trail applies to everyone.

`main` is always releasable. All work happens on feature branches (forks
for external contributors).

## Labels

| Label | Meaning |
|---|---|
| `triage` | New issue, awaiting maintainer review. Applied by the templates. |
| `accepted` | Approved in principle — a PR is welcome. Required before non-trivial PRs. |
| `proposal` | Language-change proposal (see below). |
| `spec-gap` | The spec is silent or ambiguous on something real code needs. |
| `area:compiler` / `area:lsp` / `area:stdlib` / `area:cli` / `area:docs` / `area:tooling` | Which surface the issue touches. |
| `sev:high` / `sev:medium` / `sev:low` | Maintainer-assessed severity/priority. |

Plus the GitHub defaults (`bug`, `enhancement`, `documentation`,
`good first issue`, `help wanted`, `duplicate`, `question`, `wontfix`,
`invalid`).

## AI-assisted contributions

Using AI tools to write or review contributions is **not prohibited**. What
matters is the change, not how it was produced. Two conditions:

1. **You are the author of record.** The responsibility for reviewing,
   understanding, and verifying the change falls entirely on the developer
   who submits it. "The AI wrote it" is never an answer in review — if you
   cannot explain a line of your own PR, the PR is not ready.
2. **The four pillars are non-negotiable.** Every change must respect
   Ridge's pillars — **developer experience**, **safety from the root**,
   **first-class performance**, and **approachability** (`docs/spec.md` §1)
   — and the project's coding conventions below. AI-generated code that
   bypasses diagnostics with panics, weakens the capability system, or
   degrades error messages will be rejected like any other code that does.

Do not add "generated by" footers, co-authorship trailers, or tool
attributions to commits, code comments, or docs.

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

- **Link the issue.** Every non-trivial PR names its accepted issue
  (`Closes #N` in the body). Trivial docs/chore/ci changes may check the
  "no issue required" box in the template instead.
- Squash-merged by default — keep the PR title clean (it becomes the squash commit message).
- One concern per PR. Split unrelated changes.
- Fill in the PR template completely.
- Include tests for new behavior; include a regression test for bug fixes.
- Update `CHANGELOG.md` under `## [Unreleased]` if the change is user-visible.

## Pull request checklist

- [ ] Linked issue is `accepted` (or the change is trivial and exempt)
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
