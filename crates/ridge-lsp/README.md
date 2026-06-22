# ridge-lsp

Language server for the Ridge programming language.

Implements the [Language Server Protocol](https://microsoft.github.io/language-server-protocol/)
over **stdio transport only** (no `--tcp`, no named pipe, no Unix socket).

## Capabilities

The server advertises these capabilities in its `initialize` response:

```jsonc
{
  "capabilities": {
    "textDocumentSync": {
      "openClose": true,
      "change": 2,             // TextDocumentSyncKind.Incremental
      "save": { "includeText": false }
    }
    // No diagnosticProvider: the server publishes diagnostics via
    // `client.publish_diagnostics` (push). The LSP 3.17 pull endpoint
    // `textDocument/diagnostic` is not implemented.
    // No completionProvider, hoverProvider, or definitionProvider in 0.1.0.
  }
}
```

## Diagnostic triggers

- **`textDocument/didSave`** — unconditional re-check of the workspace.
- **`textDocument/didChange`** — debounced 250 ms. Rapid edits within the
  debounce window collapse into one compile. In-flight compiles are cancelled when a
  new change arrives before the previous compile finishes.

## Edge cases

| Code | Name | Behaviour |
|------|------|-----------|
| — | Standalone mode | No `[workspace]` manifest at or above the root (or no root at all) → each open `.ridge` file is type-checked on its own, so a loose file still gets diagnostics, hover, and navigation |
| `L802` | `LspMultiRootUnsupported` | Multi-root workspace → one-time warning; only the first root is used |
| `L803` | `LspFileOrphan` | File outside any workspace member → one-time warn-once, skipped (reserved for 0.2.0) |
| `L804` | `LspInternal` | Driver internal error → `tracing::error!`, single LSP error, server stays alive |

## 0.1.0 ceiling

Ridge 0.1.0 compiles the entire workspace on every `didSave` / `didChange` event
with no incremental compilation.  For workspaces over approximately 100 modules
this means visible compile latency (> 10 s).  The in-flight cancellation
prevents stale compiles from queuing up under fast typing, but does not accelerate
single-compile latency.

Ridge 0.2.0 will introduce incremental compilation, at which point the LSP server
will scale to larger workspaces without observable latency.

## Usage

```bash
ridge-lsp
```

The server reads JSON-RPC over stdin and writes responses to stdout (LSP framing with
`Content-Length` headers).  Logs are written to stderr at the level set by the
`RIDGE_LSP_LOG` environment variable (default: `info`).

## Manual VS Code attestation

Deferred.  The VS Code extension scaffold that wires `ridge-lsp` as its language
server is not yet available.
