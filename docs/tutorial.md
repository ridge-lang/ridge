# Ridge 0.1.0 — Personal Quickstart

Install guide for Ridge 0.2.0-rc1.

---

## Prerequisites

| Requirement | Minimum | Check |
|---|---|---|
| Rust | **1.88** | `rustup show` |
| Erlang/OTP | **26** | `erl -eval 'erlang:display(erlang:system_info(otp_release)), halt().'` |
| git | **2.20** | `git --version` |

These are the values `tools/install/install.ps1` enforces at lines 96, 137, and 173.

---

## (a) Install Ridge

### Option A — Install from the current working tree (dev machine with the repo cloned)

```powershell
cargo install --path crates/ridge-cli
cargo install --path crates/ridge-lsp
```

### Option B — Install from the mirror (second machine, no local clone)

The canonical public org (`ridge-lang/ridge`) is not yet public. To install from the current mirror, override the defaults:

```powershell
$env:RIDGE_REPO   = 'https://github.com/ridge-lang/ridge'
$env:RIDGE_BRANCH = 'main'
& ([scriptblock]::Create((iwr -useb 'https://raw.githubusercontent.com/ridge-lang/ridge/main/tools/install/install.ps1').Content))
```

On Linux/macOS, use `install.sh` instead:

```bash
export RIDGE_REPO='https://github.com/ridge-lang/ridge'
export RIDGE_BRANCH='main'
curl -fsSL https://raw.githubusercontent.com/ridge-lang/ridge/main/tools/install/install.sh | bash
```

To validate on a second machine without a local clone, use Option B. Option A is available if the repo is already cloned there.

### Verify

```powershell
~/.cargo/bin/ridge --version
# expected: ridge 0.1.0
```

Use the explicit path (`~/.cargo/bin/ridge`), not a glob. The installer's success banner (install.ps1 lines 261–269) also prints this and suggests the next step.

---

## (b) Sideload the VS Code Extension

The `.vsix` is not in git (see `tools/vscode-ridge/.gitignore`). Build it from source. pnpm is required — corepack picks pnpm 11.1.1 from the `packageManager` field.

```powershell
cd H:\PROJECTS\jaavila\Ridge\tools\vscode-ridge
pnpm install
pnpm run bundle
pnpm dlx @vscode/vsce package --no-dependencies
code --install-extension ".\vscode-ridge-0.1.0.vsix"
```

Use the literal `.\vscode-ridge-0.1.0.vsix` path — PowerShell does not glob-expand external command arguments, so `*.vsix` fails here.

On Linux/macOS use forward slashes. If `code` is not on PATH on macOS:

```bash
export PATH="$PATH:/Applications/Visual Studio Code.app/Contents/Resources/app/bin"
```

Restart VS Code after the install completes.

---

## (c) Create and Run a Hello-World Project

```powershell
ridge new hello
cd hello
ridge run
```

Expected output (from the generated `Main.rg`):

```
Hello, world!
```

The generated workspace has two files worth noting:

**`ridge.toml`** — project manifest. `kind = "application"`, empty `capabilities.allow` by default.

```toml
[project]
name    = "hello"
version = "0.1.0"
kind    = "application"
```

**`src/Main.rg`** — entry point. Something like:

```ridge
pub fn main () -> Result Unit Text =
  io.println "Hello, world!"
```

`io.println` is the standard output capability. The function returns `Result Unit Text`.

---

## (d) Open in VS Code — Syntax Highlighting and Diagnostics

Use the existing G7 fixture workspace — it already has all three diagnostic triggers in place and does not require any editing.

```powershell
code H:\PROJECTS\jaavila\Ridge\tools\vscode-ridge-test
```

Open the file:

```
apps/g7fixture/src/Sample.rg
```

Open the Problems panel (`Ctrl+Shift+M`). Confirm three diagnostics appear within ~250 ms:

| # | Line in Sample.rg | Expected diagnostic |
|---|---|---|
| 1 | `import std.fs as Fs` | **R013 ForbidViolation** — workspace forbid rule `g7fixture.** -> std.fs` |
| 2 | `pub fn io needs_io () -> Int = 42` | **R016 CapabilityNotAllowed** — project `g7fixture` does not allow capability `io` |
| 3 | `pub fn bad_add (a : Int) (b : Int) -> Int = a + "hello"` | **T001 TypeMismatch** — `Int` vs `Text` |

Also confirm syntax highlighting is active: keywords (`pub`, `fn`, `import`, `as`) should render in the keyword colour; string literals in string colour; `--` comments greyed out.

### Known Limitation

All three diagnostics will appear in the Problems panel attributed to **`<unknown>` file at line 1:1**. The error message text is correct; only the file attribution and line number are wrong.

Root cause: `crates/ridge-driver/src/check.rs` lines 106 and 112 hardcode `WorkspaceSourceCache::unknown_source_id()` instead of resolving the actual module source identifier.

Fix: Strategy B (envelope `ModuleId` in the driver layer).

This is accepted behavior for 0.1.0. The diagnostic content is what matters for writing code; file attribution and navigation will be fixed in 0.2.0.

---

## (e) Format a Ridge File

Break the formatting of `hello/src/Main.rg` by adding extra whitespace or indentation, then:

```powershell
ridge fmt ./src/Main.rg
```

Or to format the whole project:

```powershell
ridge fmt .
```

Observe whitespace normalised back to canonical form. The file is formatted in-place.

---

## (f) Run the Test Suite

Add a simple test function to `hello/src/Main.rg` (or a new file):

```ridge
pub fn test_greeting () -> Result Unit Text =
    if "Hello, world!" == "Hello, world!" then Ok ()
    else Err "greeting mismatch"
```

Note: Ridge's `let` is indentation-based with no `in` keyword (see `crates/ridge-stdlib/stdlib/int.test.rg:20-27` for canonical multi-line `let` shape). The Haskell-style `let ... in body` form does not parse.

Run:

```powershell
ridge test
```

Expected output: a passing test summary. Individual test functions named `test_*` are discovered and executed by the runner.

---

## (g) Confirm `ridge run` Still Works

After making any edits in the previous steps, verify the hello-world program still runs:

```powershell
ridge run
# expected: Hello, world!
```

If the output is missing or garbled, check that `main` still has a valid `Result Unit Text` return and that no capability was added without a corresponding `capabilities.allow` entry in `ridge.toml`.

---

## Troubleshooting

**`ridge` not found after install.**
Add `~/.cargo/bin` to PATH, or use the explicit path `~/.cargo/bin/ridge`.

**LSP not starting in VS Code.**
Check the VS Code output channel "Ridge Language Server". Verify `ridge-lsp` is on PATH: `~/.cargo/bin/ridge-lsp --version`. If missing, run `cargo install --path crates/ridge-lsp` (Option A) or re-run the install script (Option B).

**Diagnostics do not appear at all (not even at `<unknown>:1:1`).**
Confirm the `ridge-lsp` binary is installed (separate from `ridge`). Confirm the extension is active (`code --list-extensions | findstr ridge`). See `docs/T15_G7_MANUAL_ATTESTATION.md` Steps 1–4 for the full G7 debug procedure.

**`pnpm dlx @vscode/vsce package` emits an icon warning.**
The placeholder icon at `tools/vscode-ridge/images/icon.png` or the `"icon"` field in `package.json` is missing. This was addressed in T5 — if it reappears, the `images/` directory or `package.json` root `icon` field was not committed.

**R013 does not fire on a fresh `ridge new` workspace.**
R013 requires a workspace-level `forbid` rule. The `tools/vscode-ridge-test/` workspace has this configured; a fresh `ridge new hello` workspace does not. Use the G7 fixture (Path A in this tutorial) to observe R013.
