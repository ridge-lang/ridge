# Ridge — Tutorial

A guided tour from install to a runnable hello-world to your first
diagnostics in VS Code. Targets Ridge **0.2.4**.

This tutorial assumes nothing beyond a working Rust toolchain and a
recent Erlang/OTP. For the formal language definition, see
[`spec.md`](spec.md); for runnable sample programs see
[`../examples/`](../examples/).

## What's in this tutorial

1. [Prerequisites](#prerequisites)
2. [Install Ridge](#install-ridge)
3. [Sideload the VS Code extension](#sideload-the-vs-code-extension)
4. [Create and run a hello-world project](#create-and-run-a-hello-world-project)
5. [See diagnostics in VS Code](#see-diagnostics-in-vs-code)
6. [Format a Ridge file](#format-a-ridge-file)
7. [Run the test suite](#run-the-test-suite)
8. [Troubleshooting](#troubleshooting)

---

## Prerequisites

| Requirement | Minimum | Check |
|---|---|---|
| Rust | **1.88** | `rustup show` |
| Erlang/OTP | **26** | `erl -eval 'erlang:display(erlang:system_info(otp_release)), halt().'` |
| git | **2.20** | `git --version` |

Both install scripts enforce these versions before touching the
filesystem, so a missing prereq fails fast with a clear message.

---

## Install Ridge

### Option A — install from a cloned repo (developer setup)

```sh
cargo install --path crates/ridge-cli
cargo install --path crates/ridge-lsp
```

Useful when you're hacking on the compiler itself.

### Option B — install from the published release

Downloads a pre-built `ridge` + `ridge-lsp` binary from the latest
GitHub release, verifies its SHA-256, and extracts to `~/.cargo/bin`.

```powershell
# Windows (PowerShell)
& ([scriptblock]::Create((iwr -useb 'https://raw.githubusercontent.com/ridge-lang/ridge/main/tools/install/install.ps1').Content))
```

```bash
# Linux / macOS — pass the script as an argument; do NOT pipe to a shell
bash -c "$(curl -fsSL https://raw.githubusercontent.com/ridge-lang/ridge/main/tools/install/install.sh)"
```

To pin a specific release tag, set `RIDGE_VERSION`:

```powershell
$env:RIDGE_VERSION = 'v0.2.3'
& ([scriptblock]::Create((iwr -useb 'https://raw.githubusercontent.com/ridge-lang/ridge/main/tools/install/install.ps1').Content))
```

```bash
RIDGE_VERSION=v0.2.3 bash -c "$(curl -fsSL https://raw.githubusercontent.com/ridge-lang/ridge/main/tools/install/install.sh)"
```

> **Why not `curl … | sh` (or `| bash`)?** The installer's Erlang
> prerequisite check calls `erl -noshell -eval …`, which reads stdin.
> When the script is piped through a shell, stdin *is* the script body;
> `erl` consumes the still-unread bytes, the shell hits EOF, and the
> installer exits silently with zero output. Passing the script as an
> argument (`bash -c "$(curl …)"`) or downloading first
> (`curl -o /tmp/ridge.sh … && bash /tmp/ridge.sh`) gives `erl` a clean
> stdin.

### Verify the install

```sh
~/.cargo/bin/ridge --version
# expected: ridge 0.2.3
~/.cargo/bin/ridge-lsp --version
# expected: ridge-lsp 0.2.3
```

Use the explicit path (`~/.cargo/bin/ridge`), not a shell glob. The
installer's success banner prints this path and the suggested next
step.

---

## Sideload the VS Code extension

The `.vsix` isn't published to the Marketplace yet; build it from the
repo. pnpm is required — corepack picks `pnpm@11.1.1` from the
`packageManager` field in `package.json`.

```sh
cd <repo-root>/tools/vscode-ridge
pnpm install
pnpm run bundle
pnpm dlx @vscode/vsce package --no-dependencies
code --install-extension ./vscode-ridge-0.2.1.vsix
```

On Windows, use the literal path `.\vscode-ridge-0.2.1.vsix` —
PowerShell doesn't glob-expand external command arguments, so
`*.vsix` won't match.

If `code` isn't on PATH on macOS:

```bash
export PATH="$PATH:/Applications/Visual Studio Code.app/Contents/Resources/app/bin"
```

Restart VS Code after the install completes. The extension activates
on any `.ridge` file and spawns `ridge-lsp` from your PATH over stdio.
See [`tools/vscode-ridge/README.md`](../tools/vscode-ridge/README.md)
for the full extension docs.

---

## Create and run a hello-world project

```sh
ridge new hello
cd hello
ridge run
```

Expected output:

```
Hello from hello!
```

The generated workspace has two files worth knowing about:

**`ridge.toml`** — the project manifest. A single-project workspace
with one app member, the `io` capability allowed, and an entry point
under `src/`.

```toml
[workspace]
name = "hello"
version = "0.1.0"
members = ["."]

[project]
name = "hello"
version = "0.1.0"
kind = "app"
entry = "src/Main.ridge"

[project.src]
root = "src"

[capabilities]
allow = ["io"]
```

**`src/Main.ridge`** — the entry point.

```ridge
import std.io as Io

pub fn io main () -> Unit =
  Io.println $"Hello from hello!"
```

The `io` prefix on `fn` declares the capability the function uses.
Capabilities are part of the function's signature, so the workspace
manifest can grant or revoke them at compile time. `Io.println` is
the standard-output entry point in `std.io`.

---

## See diagnostics in VS Code

A pre-wired sample workspace ships with the repo at
`tools/vscode-ridge-test/`. It has three deliberate problems set up so
you can confirm the language server is alive and producing real
diagnostics.

```sh
code <repo-root>/tools/vscode-ridge-test
```

Open the file:

```
apps/vscode-diag-fixture/src/Sample.ridge
```

Open the Problems panel (`Ctrl+Shift+M` / `Cmd+Shift+M`). Three
diagnostics should appear within about 250 ms:

| # | Line in `Sample.ridge` | Expected diagnostic |
|---|---|---|
| 1 | `import std.fs as Fs` | **R013 ForbidViolation** — workspace `forbid` rule blocks `std.fs` for this project |
| 2 | `pub fn io needs_io () -> Int = 42` | **R016 CapabilityNotAllowed** — the project's `capabilities.allow` does not include `io` |
| 3 | `pub fn bad_add (a : Int) (b : Int) -> Int = a + "hello"` | **T001 TypeMismatch** — `Int` vs `Text` |

Also confirm syntax highlighting is active: keywords (`pub`, `fn`,
`import`, `as`) render in the keyword colour, string literals in the
string colour, and `--` line comments are greyed out.

### A known rough edge

In the current release, all three diagnostics show up in the Problems
panel attributed to `<unknown>` at `1:1`. The diagnostic *messages*
are correct — only the file attribution and line number are wrong.
Tracked for fix in a follow-up release; the diagnostic content is
already enough to find and fix the underlying problem in the editor.

---

## Format a Ridge file

Break the formatting of `hello/src/Main.ridge` by adding stray whitespace
or indentation, then run:

```sh
ridge fmt ./src/Main.ridge
```

Or to format the whole project:

```sh
ridge fmt .
```

Files are rewritten in place. The formatter is deterministic — running
it twice produces the same output as running it once.

---

## Run the test suite

`ridge test` discovers every `pub fn test_*` (arity 0) in the workspace
and runs it. Tests return `Result Unit Text`: `Ok ()` passes, `Err msg`
fails with `msg` printed.

Add a test to `hello/src/Main.ridge`:

```ridge
pub fn test_greeting () -> Result Unit Text =
    if "Hello from hello!" == "Hello from hello!" then Ok ()
    else Err "greeting mismatch"
```

A note on `let`: Ridge's `let` is indentation-based and has no `in`
keyword. The Haskell-style `let … in body` form does not parse. For
the canonical multi-line shape, see
[`crates/ridge-stdlib/stdlib/int.test.ridge`](../crates/ridge-stdlib/stdlib/int.test.ridge)
around lines 20–27.

Run:

```sh
ridge test
```

You should see a passing test summary.

---

## Troubleshooting

**`ridge` not found after install.**
Add `~/.cargo/bin` to your PATH, or invoke the explicit path
`~/.cargo/bin/ridge`. The installer banner prints both.

**LSP not starting in VS Code.**
Open the "Ridge Language Server" output channel from the VS Code
output panel. Verify `ridge-lsp` is on PATH:
`~/.cargo/bin/ridge-lsp --version`. If it's missing, install it via
Option A (`cargo install --path crates/ridge-lsp`) or re-run the
release installer (Option B).

**Diagnostics don't appear at all.**
Confirm the `ridge-lsp` binary is installed (it's a separate binary
from `ridge`) and that the extension is active
(`code --list-extensions | grep ridge`, or `findstr ridge` on
Windows). Reload the VS Code window after sideloading.

**`pnpm dlx @vscode/vsce package` warns about a missing icon.**
The placeholder icon at `tools/vscode-ridge/images/icon.png` or the
`"icon"` field in `package.json` is missing from the working tree.

**R013 doesn't fire on a fresh `ridge new` workspace.**
R013 needs a workspace-level `forbid` rule, which `ridge new` doesn't
emit by default. The pre-wired `tools/vscode-ridge-test/` workspace
has one configured; use it to observe R013.

**Non-ASCII output renders as mojibake on Windows (`°` → `Â°`, `é` → `Ã©`).**
`Io.println` writes UTF-8 to stdout, but the default Windows console
codepage is the system locale (cp1252 on most English/Spanish installs),
so the bytes are misdecoded. Switch the active codepage to UTF-8 before
running Ridge programs that print non-ASCII text:

```powershell
chcp 65001
ridge run
```

The change lasts for the lifetime of the current console window. To make
it permanent, add `chcp 65001 > $null` to your PowerShell profile
(`$PROFILE`). Windows Terminal users can also enable
*Settings → Defaults → Advanced → Use Unicode UTF-8 for worldwide
language support* in the system Region settings; the change applies to
new console sessions after a reboot.
