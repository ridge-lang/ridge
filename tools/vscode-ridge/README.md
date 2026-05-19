# Ridge for VS Code

Language support for [Ridge](https://github.com/ridge-lang/ridge), a typed
functional language for the BEAM. Highlights `.ridge` source files and
surfaces diagnostics from `ridge-lsp` as you type.

## Features

- Syntax highlighting for `.ridge` files (TextMate grammar).
- Live diagnostics from `ridge-lsp` — type errors, capability violations,
  parse errors — shown inline and in the **Problems** panel.
- Comment toggling with `--`.
- Bracket matching and auto-closing for `()`, `[]`, `{}`, and `""`.

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

To pin a specific Ridge release, set `RIDGE_VERSION=v0.2.0-rc5` before
running the installer. See the
[tutorial](https://github.com/ridge-lang/ridge/blob/main/docs/tutorial.md)
for the full install walkthrough.

## Settings

| Setting | Default | Purpose |
|---|---|---|
| `ridge.lspPath` | `""` | Absolute path to the `ridge-lsp` binary. Leave empty to auto-resolve (tries `~/.cargo/bin/ridge-lsp` first, then walks `PATH`). |

## Known limitations

- **Capability highlighting**: capabilities (`io`, `fs`, `net`, etc.) render
  as plain identifiers. Distinguishing capability call-sites from
  same-named local variables requires LSP semantic tokens; tracked for a
  future release.
- **Hover, completion, go-to-definition**: not yet provided by `ridge-lsp`.
  Only diagnostics are surfaced today.
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
