# Changelog

All notable changes to the Ridge VS Code extension will be documented
in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The extension version tracks the Ridge language version, but is not
coupled to a specific Ridge binary: it locates `ridge-lsp` on PATH (or
at the path set via the `ridge.lspPath` setting) and works with
whatever Ridge release is installed.

## [Unreleased]

### Added

- Per-lens settings `ridge.codeLens.references`, `ridge.codeLens.implementations`,
  `ridge.codeLens.run`, and `ridge.codeLens.runTest` (all on by default) to turn
  individual code lenses on or off. Changes are pushed to the server and applied
  live — toggling a lens no longer needs a reload.
- Go to Type Definition: from any value, jump to the `type` declaration of
  its inferred type. Go-to-definition, find-references, rename, and document
  highlight now also resolve record fields, so a `record.field` use
  navigates to the field's declaration, gathers and renames every use of
  that field across the workspace, and highlights its occurrences in the
  current file. A field rename is scoped to its owner record, so a field of
  the same name on a different record is left untouched.
- Semantic highlighting from `ridge-lsp`, colouring identifiers the
  grammar cannot tell apart — function vs variable vs type vs constructor
  vs stdlib symbol — and surfacing capability annotations (`io`, `fs`,
  `net`, `db`, ...) as their own token type, mapped to a `storage.modifier`
  scope so default themes pick it up.
- Auto-indentation for Ridge's offside layout. Pressing Enter after a
  line that opens a block — one ending in `=`, `->`, `<-`, `then`,
  `else`, or `try`, or a `match` head — indents the new line, and the
  lone `else` of an `if ... then ... else` dedents to line up with the
  `if`.
- Syntax highlighting for the `opaque` keyword, raw string literals
  (`r"..."`, `r#"..."#`), and triple-quoted strings (`"""..."""`).

### Fixed

- The README and Marketplace description listed hover, completion, and
  go-to-definition as unavailable. `ridge-lsp` provides them, so the docs
  now describe what actually ships.

## [0.2.1] - 2026-05-21

First publication to Open VSX. No source changes since 0.2.0; the bump
exists to record the new distribution channel.

### Added

- Published to the [Open VSX Registry](https://open-vsx.org/extension/ridge-lang/vscode-ridge)
  for VSCodium, Cursor, and other VS Code derivatives.

## [0.2.0] - 2026-05-20

First publication to the VS Code Marketplace as
[`ridge-lang.vscode-ridge`](https://marketplace.visualstudio.com/items?itemName=ridge-lang.vscode-ridge),
shipping alongside the Ridge language v0.2.0 release.

### Added

- Marketplace listing metadata: `galleryBanner`, `keywords`, `categories`
  (`Programming Languages`, `Linters`), `homepage`, `bugs`, `license`.
- Ridge brand icon (128×128 PNG, with SVG vector source under
  `images/source/`).
- Apache-2.0 `LICENSE` shipped inside the `.vsix` package.
- README rewritten as a Marketplace listing (Features / Requirements /
  Settings / Known limitations / Development / Links).

### Changed

- Source-file extension recognised by the language grammar is now
  `.ridge` (was `.rg`). Matches the language-side rename in Ridge v0.2.0;
  resolves a Linguist registry collision with Rouge.
- Version bumped from `0.1.0` to `0.2.0` so the extension tracks the
  Ridge language release line.

[Unreleased]: https://github.com/ridge-lang/ridge/compare/vscode-v0.2.1...HEAD
[0.2.1]: https://github.com/ridge-lang/ridge/compare/vscode-v0.2.0...vscode-v0.2.1
[0.2.0]: https://github.com/ridge-lang/ridge/releases/tag/vscode-v0.2.0
