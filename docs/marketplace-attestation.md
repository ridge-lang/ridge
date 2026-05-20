# VS Code Marketplace Attestation

The Ridge extension is published to the VS Code Marketplace as
`ridge-lang.vscode-ridge`. This document records the three-platform
install verification.

## Publication

- Marketplace listing: <https://marketplace.visualstudio.com/items?itemName=ridge-lang.vscode-ridge>
- Publisher: <https://marketplace.visualstudio.com/publishers/ridge-lang>
- First version: `0.2.0` (published 2026-05-20)
- Packaged from: `tools/vscode-ridge/` at commit `6023523`
- `.vsix` contents: `LICENSE.txt`, `package.json`, `readme.md`,
  `language-configuration.json`, `images/{icon.png,icon.svg}`,
  `syntaxes/ridge.tmLanguage.json`, `out/extension.js` — 10 files, 134.6 KB.

## Install verification

| Platform | Date | VS Code | Install via `code --install-extension` | Syntax highlighting | Diagnostics from `ridge-lsp` |
|---|---|---|---|---|---|
| Windows 11 | 2026-05-20 | 1.120.0 | OK (`ridge-lang.vscode-ridge@0.2.0`) | OK | OK |
| Linux | _pending_ | _pending_ | _pending_ | _pending_ | _pending_ |
| macOS | _pending_ | _pending_ | _pending_ | _pending_ | _pending_ |

Toolchain on the Windows row was `ridge 0.2.0-rc5` / `ridge-lsp 0.2.0-rc5`
at `~/.cargo/bin/`. The Marketplace install replaces the prior local
sideload (`vscode-ridge-0.1.0.vsix`) cleanly — VS Code prompts to remove
the old version on next restart. Highlighting and diagnostics were
attested against the same TextMate grammar and LSP client that the
sideload version has been carrying through routine development.

## Verification recipe (per platform)

Prerequisite: `ridge` and `ridge-lsp` are on `PATH`. Install via the
one-liners at the top of [the README](../README.md) if they are not.

```sh
# 1. Install the extension from the Marketplace.
code --install-extension ridge-lang.vscode-ridge

# 2. Confirm the installed version matches the published version.
code --list-extensions --show-versions | grep ridge-lang.vscode-ridge
# expected: ridge-lang.vscode-ridge@0.2.0
```

Then, in VS Code:

3. Open `examples/log_analyzer.ridge`.
4. Confirm syntax highlighting is active — keywords (`let`, `match`,
   `with`), type names, and string literals should be colored.
5. Wait a couple of seconds for `ridge-lsp` to attach. Open the Problems
   panel (`Ctrl+Shift+M` / `Cmd+Shift+M`).
6. Introduce a deliberate type error (e.g. pass a `Text` where an `Int`
   is expected), save the file. A diagnostic from `ridge-lsp` should
   appear in the Problems panel attributed to the correct file and line.

Tick the corresponding row in the table above when all six steps pass on
that platform.

## Known limitations on this release

- Capabilities (`io`, `fs`, `net`, ...) render as plain identifiers.
  Semantic-tokens-based capability highlighting is planned for a future
  release.
- Only diagnostics are surfaced today — `ridge-lsp` does not yet
  implement hover, completion, or go-to-definition.
