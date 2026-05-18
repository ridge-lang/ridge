# vscode-ridge

> **Status: initial scaffold — not Marketplace-published.**
> This extension provides the minimal VS Code integration to demonstrate M5 ("VS Code shows errors from Ridge files as they happen"). A future release will deliver the full extension: TextMate grammar, syntax highlighting, and Marketplace publication.

## Prerequisites

- **Node.js** >= 18
- **pnpm** >= 9 (project pins `packageManager: pnpm@11.1.1` for corepack). npm is **not** supported here — see the "Why pnpm" note at the bottom of this file.
- **VS Code** >= 1.85 (required by `vscode-languageclient` ~9.0)
- **`ridge-lsp`** on your `PATH` — install via:
  - Linux / macOS: `bash tools/install/install.sh`
  - Windows: `powershell tools/install/install.ps1`

## Build and sideload

```sh
cd tools/vscode-ridge
pnpm install
pnpm dlx @vscode/vsce package --no-dependencies
code --install-extension vscode-ridge-0.1.0.vsix
```

`pnpm dlx` runs `@vscode/vsce` one-shot without a global install, mirroring the `npx` semantics the original instructions used. `--no-dependencies` skips vsce's post-prepublish `npm list` walk, which is incompatible with pnpm's symlinked `node_modules` layout (see "Why pnpm" below for the full rationale). The flag is harmless because `.vscodeignore` already excludes `node_modules/**` from the `.vsix`.

Reload VS Code after installation (`Developer: Reload Window`).

## What it does

- Registers `.rg` files as the `ridge` language.
- Spawns `ridge-lsp` from your `PATH` via stdio when a `.rg` file is opened.
- Surfaces LSP diagnostics (errors, warnings) in the **Problems** panel (`Ctrl+Shift+M` / `Cmd+Shift+M`) as you edit.
- Enables comment toggling with `--` (Ridge's line-comment marker) via `language-configuration.json`.
- Enables bracket matching and auto-closing for `()`, `[]`, `{}`, and `""`.

## What it does NOT do

- No syntax highlighting (no TextMate grammar) — planned for a future release.
- No completion, hover, go-to-definition, or other LSP features beyond what `ridge-lsp` exposes over the protocol.
- Not published to the VS Code Marketplace — sideload only for now.

## Manual test (M5 acceptance — 1 manual count)

Manual verification steps for the diagnostics round-trip:

1. Build and sideload the extension per the instructions above.
2. Open the repository root in VS Code.
3. Open `examples/log_analyzer.rg`.
4. Introduce a deliberate type error — for example, pass a string where an integer is expected.
5. Save the file (`Ctrl+S` / `Cmd+S`).
6. Open the **Problems** panel (`Ctrl+Shift+M` / `Cmd+Shift+M`).
7. Confirm that a diagnostic for the type error appears within a few seconds.

Expected: the diagnostic is listed with the correct file, line, and column. The error disappears when you fix the type mismatch and save again.

## Edge cases

| Situation | Behaviour |
|---|---|
| `ridge-lsp` not on `PATH` | VS Code shows an error message: _"Ridge: failed to start language server. `ridge-lsp` was not found on PATH. Install it via `tools/install/install.sh` …"_ |
| VS Code < 1.85 | `vscode-languageclient` ~9.0 will not activate correctly; upgrade VS Code. |
| First open before `ridge-lsp` is installed | Extension activates (language is registered), but no diagnostics appear until `ridge-lsp` is installed and VS Code is reloaded. |

## Architecture

```
VS Code (extension host)
  └─ src/extension.ts          — activate() / deactivate()
       └─ LanguageClient       — vscode-languageclient ~9.0
            └─ stdio transport
                 └─ ridge-lsp  — Ridge LSP server (spawned from PATH)
```

**Bundling.** `esbuild-wasm` (devDep, pinned `^0.21.0`) bundles `src/extension.ts` + its `vscode-languageclient` runtime dep into a single `out/extension.js` (CommonJS, target `node18`, `vscode` marked external — provided by the VS Code extension host). The WASM-build of esbuild is chosen over the native `esbuild` package to avoid postinstall scripts entirely — see "Why pnpm" below. The `bundle` script runs esbuild; `vscode:prepublish` (called automatically by `vsce package`) runs `bundle`. The `compile` script (plain `tsc -p ./`) is retained for IDE / type-check use but is no longer the publish path. Bundling is required because `.vscodeignore` excludes `node_modules/**` from the produced `.vsix`; without it, `require('vscode-languageclient/node')` fails at extension-activation time and the LSP never starts. The `out/` directory is a build artefact and is not committed.

## Why pnpm

Switched from npm to pnpm on 2026-05-12 in response to a wave of npm-ecosystem vulnerabilities. pnpm's content-addressable store and strict by-default symlinked `node_modules` eliminate the implicit-hoist class of supply-chain risk that npm 10.x exposed. The `packageManager` field pins `pnpm@11.1.1` so corepack picks the right tool automatically; running `npm install` here will produce a `package-lock.json` that the next `pnpm install` rejects.

The lockfile (`pnpm-lock.yaml`, `lockfileVersion: 9.0`) is committed; `node_modules/` is gitignored as usual. `@vscode/vsce` is invoked via `pnpm dlx` so it does not pollute `devDependencies`.

**vsce + pnpm gotcha — why `--no-dependencies` is required.** After running the `vscode:prepublish` script, vsce shells out to `npm list --production --parseable --depth=99999` to enumerate runtime deps for the `.vsix`. pnpm's strict-install layout stores packages at `node_modules/.pnpm/<pkg>@<ver>/node_modules/<pkg>/` with thin symlinks at the top level and does not install transitive devDeps of runtime deps. `npm list` reads this as "missing dep X" and "invalid dep Y" and exits non-zero, killing `vsce package` before the `.vsix` is produced. Passing `--no-dependencies` skips the walk entirely; since `.vscodeignore` excludes `node_modules/**` from the bundle, the produced `.vsix` is byte-identical to the npm-built version. The runtime resolution of `vscode-languageclient` inside VS Code is unaffected (VS Code does not consult `npm list` — it loads the extension's `out/extension.js` and resolves requires through its own module loader against the bundled `node_modules`).

**pnpm 10+ install-script policy — no whitelist used.** pnpm 10 and later refuse by default to execute install scripts (`postinstall`, etc.) of dependencies, citing supply-chain risk (`ERR_PNPM_IGNORED_BUILDS`). The migration away from npm was driven by exactly this attack class, so re-opening the door with a `pnpm.onlyBuiltDependencies` whitelist — even for a single "trusted" package like `esbuild` — would defeat the security stance: every whitelisted package becomes a trust assumption that survives clone / reinstall and creates a precedent for the next "just one more". **This project chooses bundlers that need no install scripts.** `esbuild-wasm` (the official WebAssembly build of esbuild) ships its WASM blob inside the npm package itself — no `postinstall`, no native binary download, no per-platform fan-out. The `bundle` script invokes `esbuild` from `node_modules/.bin/` exactly as the native esbuild would; esbuild-wasm exposes the same CLI flag surface, so the script body is identical. WASM startup adds ~50% latency vs the native binary, indistinguishable on a single-file bundle. If a future devDep here introduces an install script, the response is to find a pure-JS / pure-WASM alternative, not to whitelist; the only exception would be an industry-critical tool with no scriptless alternative, which must be justified in this section before it lands.
