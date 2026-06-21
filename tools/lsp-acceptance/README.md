# LSP client acceptance

The `ridge-lsp` unit and replay tests drive the server's request handlers
in-process. They do not exercise a real editor: the stdio framing, the
initialize handshake against a real client's capabilities, position-encoding
negotiation, or how a client renders and applies what the server returns.

This directory closes that gap by validating `ridge-lsp` against real LSP
clients, in three layers.

## Layer 0 — wire smoke (Rust, automated)

`crates/ridge-lsp/tests/stdio_smoke.rs` spawns the built `ridge-lsp` binary and
speaks `Content-Length`-framed JSON-RPC to it over real stdin/stdout: the
`initialize` handshake (asserting the UTF-16 position encoding it advertises),
`didOpen` followed by a pushed `publishDiagnostics`, a `hover` round-trip, and a
clean `shutdown` + `exit`. It runs as part of the normal `cargo test`, so no
editor is required:

```sh
cargo test -p ridge-lsp --test stdio_smoke
```

## Layer 1 — Neovim (automated)

`neovim/driver.lua` starts `ridge-lsp` as a real `vim.lsp` client and runs the
handler matrix against `fixtures/ws`: hover, go-to-definition (including a
target that sits after a multibyte string literal, which only resolves when
UTF-16 columns are handled correctly), typeDefinition, record-field definition
and rename, find-references, documentHighlight, semantic tokens, formatting,
inlay hints, and pushed diagnostics. It needs Neovim 0.10+ on PATH:

```sh
cargo build -p ridge-lsp
RIDGE_LSP="$(pwd)/target/debug/ridge-lsp" bash tools/lsp-acceptance/run.sh
```

On Windows, point `RIDGE_LSP` at `target\debug\ridge-lsp.exe` and run the script
from Git Bash. The suite prints TAP-style output and exits non-zero if any check
fails. CI runs it on every code change (the `LSP acceptance (Neovim)` job).

## Layer 2 — JetBrains (manual)

GUI IDEs cannot be driven headless for third-party LSP servers, so JetBrains is
a documented manual pass: install the free [LSP4IJ][lsp4ij] plugin (works in
Community editions), register `ridge-lsp`, open `fixtures/ws`, and walk the
checklist in [`jetbrains/ACCEPTANCE.md`](jetbrains/ACCEPTANCE.md). Record the
results there.

[lsp4ij]: https://plugins.jetbrains.com/plugin/23257-lsp4ij

## Fixtures

`fixtures/ws` is a two-manifest Ridge workspace. `app/src/Main.ridge` holds the
navigation and rename targets (a record type, a field access, and a reference
that follows a `"café"` literal for the UTF-16 check); `app/src/Errors.ridge`
holds a deliberate type error for the diagnostics check.
