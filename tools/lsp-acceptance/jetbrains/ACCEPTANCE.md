# JetBrains acceptance checklist (LSP4IJ)

JetBrains IDEs cannot be driven headless for a third-party LSP server, so this
is a manual pass. It takes about ten minutes and only needs to be repeated when
the server's capabilities change. Record the outcome at the bottom and paste it
into the pull request or the tracking issue.

## Prerequisites

1. Any JetBrains IDE — IntelliJ IDEA Community, PyCharm Community, GoLand, etc.
   The native LSP API is Ultimate-only, so this checklist uses the free
   [LSP4IJ][lsp4ij] plugin, which works everywhere.
2. `ridge` and `ridge-lsp` on `PATH` (run the install script, or
   `cargo install --path crates/ridge-lsp`).
3. The LSP4IJ plugin installed: **Settings → Plugins → Marketplace → "LSP4IJ"**.

## One-time server registration

1. **Settings → Languages & Frameworks → Language Servers → +** (New Language
   Server).
2. Name: `Ridge`. Command: `ridge-lsp` (or its absolute path).
3. **Mappings → File name patterns**: pattern `*.ridge`, language id `ridge`.
4. Apply.

## Run

Open `tools/lsp-acceptance/fixtures/ws` as the project (or add it as a content
root), then open `app/src/Main.ridge`. Reference line numbers below are
1-based as the editor shows them.

| # | Feature | How to trigger | Expected | Result |
|---|---------|----------------|----------|--------|
| 1 | Diagnostics | Open `app/src/Errors.ridge` | A type-mismatch error is underlined on the `broken` body (`Int` vs `Text`) | |
| 2 | Hover | Hover `u` in `u.age` (line 3) | A popup shows `u` with type `User` | |
| 3 | Go to definition | Ctrl/Cmd-click `age` in `u.age` (line 3) | Jumps to the `age` field in the `User` declaration (line 1) | |
| 4 | Go to type definition | On `u` (line 3), invoke Go to Type Definition | Jumps to the `User` type declaration (line 1) | |
| 5 | Find usages | On `label` (line 5 or 7), Find Usages | Lists the declaration and the use on line 7 | |
| 6 | Rename | Rename the `age` field (Shift-F6) to `years` | Both the field declaration (line 1) and `u.age` (line 3) update; a same-named field on another record would be untouched | |
| 7 | Highlight | Place the caret on `label` (line 7) | Other occurrences of `label` in the file highlight | |
| 8 | Semantic highlighting | Look at `app/src/Main.ridge` | Types, functions, and parameters are distinctly coloured | |
| 9 | Formatting | Reformat the file (Ctrl/Cmd-Alt-L) | No error; the file stays valid | |
| 10 | Completion | Type `u.` inside `ageOf` | `age` and `name` are offered | |
| 11 | UTF-16 columns | Go to definition on `n` after `"café"` (line 7) | Resolves to the `n` parameter — proves multibyte columns are handled | |

## Result

- IDE + version:
- LSP4IJ version:
- `ridge-lsp --version`:
- Date:
- Outcome: PASS / FAIL (note any failing row number and what you saw)

[lsp4ij]: https://plugins.jetbrains.com/plugin/23257-lsp4ij
