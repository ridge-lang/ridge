# Ridge for VS Code

Language support for [Ridge](https://github.com/ridge-lang/ridge), a typed
functional language for the BEAM. Highlights `.ridge` source files and
connects to `ridge-lsp` for diagnostics, hover, go-to-definition, and
completion as you type.

## Features

- Syntax highlighting for `.ridge` files (TextMate grammar).
- Live diagnostics from `ridge-lsp` — type errors, capability violations,
  parse errors — shown inline and in the **Problems** panel.
- Hover for inferred types; go-to-definition across the workspace and into
  the standard library — including jumping from a `record.field` use to the
  field's declaration; go-to-type-definition from a value to the `type` that
  declares it; find-references, rename, signature help, document formatting,
  document and workspace symbols, inlay hints, and identifier completion —
  all served by `ridge-lsp`.
- Semantic highlighting that colours identifiers the grammar cannot tell
  apart — function vs variable vs type vs constructor vs stdlib symbol — and
  surfaces capability annotations (`io`, `fs`, `net`, `db`, ...) as their own
  token type.
- Comment toggling with `--`.
- Bracket matching and auto-closing for `()`, `[]`, `{}`, and `""`.
- Auto-indentation for the offside layout: blocks opened by `=`, `->`,
  `<-`, `then`, `else`, `try`, or a `match` head indent the next line.

## Requirements

This extension launches `ridge-lsp` over stdio. Install the Ridge toolchain
first; both `ridge` and `ridge-lsp` need to be on your `PATH`.

**Linux and macOS:**

```sh
bash -c "$(curl -fsSL https://raw.githubusercontent.com/ridge-lang/ridge/main/tools/install/install.sh)"
```

**Windows (PowerShell):**

```powershell
& ([scriptblock]::Create((iwr -useb 'https://raw.githubusercontent.com/ridge-lang/ridge/main/tools/install/install.ps1').Content))
```

To pin a specific Ridge release, set `RIDGE_VERSION=v0.2.1` before
running the installer. See the
[tutorial](https://github.com/ridge-lang/ridge/blob/main/docs/tutorial.md)
for the full install walkthrough.

## How it works (and how to update the language)

The extension is a thin LSP client. It does not bundle the Ridge compiler
or analyzer. When you open a `.ridge` file, the extension spawns the
`ridge-lsp` binary from your `PATH` and proxies JSON-RPC messages
between it and the editor; all type checking, diagnostics, and
formatting come from that binary.

```
┌─────────────────────────────┐     ┌─────────────────────────────┐
│  VS Code                    │     │  ridge-lsp binary           │
│  + this extension           │     │  (in PATH, separate install)│
│  • syntax grammar           │ JSON│  • lexer, parser            │
│  • language config          │ RPC │  • resolver, typechecker    │
│  • LSP client               │◀───▶│  • diagnostics              │
│  • spawns ridge-lsp         │stdio│  • formatter                │
└─────────────────────────────┘     └─────────────────────────────┘
        installed once via              installed via install script,
        the Marketplace                  re-run for each release
```

The practical consequence: **compiler and diagnostic fixes ship with
the `ridge-lsp` binary, not with the extension**. A Ridge maintenance
release that improves diagnostics or fixes a codegen bug only requires
re-running the install script to update the binary; the extension you
installed from the Marketplace keeps working as-is.

If something stops behaving the way the [tutorial](https://github.com/ridge-lang/ridge/blob/main/docs/tutorial.md)
says it should, re-run the install script first to make sure your
`ridge-lsp` matches the latest release; reinstalling the extension
itself rarely helps for compiler-side issues.

## Settings

| Setting | Default | Purpose |
|---|---|---|
| `ridge.lspPath` | `""` | Absolute path to the `ridge-lsp` binary. Leave empty to auto-resolve (tries `~/.cargo/bin/ridge-lsp` first, then walks `PATH`). |

## Known limitations

- **First open before `ridge-lsp` is installed**: the extension activates
  and registers the language, but diagnostics only appear once
  `ridge-lsp` is available and VS Code is reloaded.

## Development

Building the extension from source for local sideloading:

```sh
cd tools/vscode-ridge
pnpm install
pnpm dlx @vscode/vsce package --no-dependencies
code --install-extension vscode-ridge-<version>.vsix
```

The project uses pnpm (`packageManager: pnpm@11.1.1` via corepack) and
`esbuild-wasm` to keep the toolchain free of postinstall scripts. The
`--no-dependencies` flag on `vsce package` is required because pnpm's
strict-install layout is incompatible with `vsce`'s internal `npm list`
walk; the produced `.vsix` is byte-identical to the npm-built version
since `node_modules/**` is excluded from the bundle.

## Links

- [Repository](https://github.com/ridge-lang/ridge)
- [Tutorial](https://github.com/ridge-lang/ridge/blob/main/docs/tutorial.md)
- [Language specification](https://github.com/ridge-lang/ridge/blob/main/docs/spec.md)
- [Report an issue](https://github.com/ridge-lang/ridge/issues)

## License

[Apache-2.0](https://github.com/ridge-lang/ridge/blob/main/LICENSE)
