# Ridge — Tutorial

A guided tour from install to a runnable hello-world to your first
diagnostics in VS Code. Targets Ridge **0.3.0-rc4**.

This tutorial assumes nothing beyond a working Rust toolchain and a
recent Erlang/OTP. For the formal language definition, see
[`spec.md`](spec.md); for runnable sample programs see
[`../examples/`](../examples/).

## What's in this tutorial

1. [Prerequisites](#prerequisites)
2. [Install Ridge](#install-ridge)
3. [Install the VS Code extension](#install-the-vs-code-extension)
4. [Create and run a hello-world project](#create-and-run-a-hello-world-project)
5. [See diagnostics in VS Code](#see-diagnostics-in-vs-code)
6. [Format a Ridge file](#format-a-ridge-file)
7. [Run the test suite](#run-the-test-suite)
8. [Typeclasses](#typeclasses)
9. [Troubleshooting](#troubleshooting)

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
$env:RIDGE_VERSION = 'v0.2.13'
& ([scriptblock]::Create((iwr -useb 'https://raw.githubusercontent.com/ridge-lang/ridge/main/tools/install/install.ps1').Content))
```

```bash
RIDGE_VERSION=v0.2.13 bash -c "$(curl -fsSL https://raw.githubusercontent.com/ridge-lang/ridge/main/tools/install/install.sh)"
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
~/.cargo/bin/ridge-lsp --version
```

Use the explicit path (`~/.cargo/bin/ridge`), not a shell glob. The
installer's success banner prints this path and the suggested next
step.

---

## Install the VS Code extension

The extension is published to the Visual Studio Code Marketplace. Search
for **Ridge** in the Extensions view (`Ctrl+Shift+X` / `Cmd+Shift+X`), or
install from the command line:

```sh
code --install-extension ridge-lang.vscode-ridge
```

You can also install from the Marketplace page:
[marketplace.visualstudio.com/items?itemName=ridge-lang.vscode-ridge](https://marketplace.visualstudio.com/items?itemName=ridge-lang.vscode-ridge)

Restart VS Code after the install completes. The extension activates
on any `.ridge` file and spawns `ridge-lsp` from your PATH over stdio.
See [`tools/vscode-ridge/README.md`](../tools/vscode-ridge/README.md)
for the full extension docs.

Beyond live diagnostics, the language server answers three editor requests:
**hover** shows the inferred type of the symbol under the cursor,
**go-to-definition** jumps to where a name is bound (including across modules),
and **completion** suggests the locals in scope, the module's symbols, imports,
and keywords — plus a module's exported names after you type `Module.`. Edits
recompile only the affected modules, so these stay responsive as a workspace
grows.

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

`ridge test` discovers every function annotated with `@test` and runs it.
Tests return `Result Unit Text`: `Ok ()` passes, `Err msg` fails with `msg`
printed. Annotate with `@test "<display name>"` above any function — the name
is shown in the test report and can be any string:

```ridge
@test "greeting matches expected output"
fn greeting () -> Result Unit Text =
    if "Hello from hello!" == "Hello from hello!" then Ok ()
    else Err "greeting mismatch"
```

The `pub fn test_*` naming prefix is deprecated as of 0.2.8 and removed in
0.3.0. Use `@test` for all new tests; run `ridge fmt --migrate-tests` to
rewrite any old-style tests automatically.

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

## Flattening conditionals

Result-returning code — tests especially — tends to drift rightward into a
nested-`if` "staircase", one level per check:

```ridge
fn validate (r: List Int) -> Result Unit Text =
    if List.length r == 5 then
        if List.head r == Some 1 then
            if List.length r >= 3 then Ok ()
            else Err "too short"
        else Err "head is not 1"
    else Err "length is not 5"
```

Each new check indents the success path further and pushes its failure to a
matching `else` far below. Ridge has four flatter forms; reach for whichever
fits.

**`guard … else return`** exits early when a check fails, so the happy path
stays at one indent level:

```ridge
fn validate (r: List Int) -> Result Unit Text =
    guard List.length r == 5 else return Err "length is not 5"
    guard List.head r == Some 1 else return Err "head is not 1"
    guard List.length r >= 3 else return Err "too short"
    Ok ()
```

**The `?` operator** propagates an `Err` (or `None`) and unwraps an `Ok`, so a
`Result`-returning helper chains without any branching. The `std.test` module
provides those helpers for assertions:

```ridge
import std.list as List
import std.test (ensure, assertEq)

@test "list has the expected shape"
fn validate () -> Result Unit Text =
    let r = [1, 2, 3, 4, 5]
    ensure (List.length r == 5) "length is not 5" ?
    assertEq (List.head r) (Some 1) "head is not 1" ?
    ensure (List.length r >= 3) "too short" ?
    Ok ()
```

`std.test` covers `ensure`, `assertEq`/`assertNe`, `assertTrue`/`assertFalse`,
and `assertOk`/`assertErr`/`assertSome`/`assertNone`. Equality is structural, so `assertEq`
works on any type; the string is the message shown when the check fails.

**`else if`** is the flat form for choosing a value among several conditions
(no early exit):

```ridge
fn label (n: Int) -> Text =
    if n < 0 then "negative"
    else if n == 0 then "zero"
    else if n < 10 then "small"
    else "large"
```

**`match … when`** pairs pattern matching with guards when the branches
inspect a value's shape:

```ridge
fn classify (r: Result Int Text) -> Text =
    match r
        Ok n when n > 0 -> "positive"
        Ok _            -> "non-positive"
        Err _           -> "failed"
```

The language server flags a conditional that nests three or more levels deep in
a `Result`/`Unit` function with a hint, so a staircase is easy to spot and
flatten.

---

## Typeclasses

Typeclasses let you write functions that work across multiple types without
committing to a specific one. The classic example is a `describe` function
that can render any type it's given — as long as that type knows how to
produce a text representation.

### Defining a class

```ridge
class Describable a =
    describe (x: a) -> Text
```

`class Describable a` declares an interface with one method. The body
lists bare signatures — no `fn` keyword, no implementation.

### Writing an instance

```ridge
type Color = Red | Green | Blue

instance Describable Color =
    describe (c: Color) -> Text = match c
        Red   -> "a red pigment"
        Green -> "a green pigment"
        Blue  -> "a blue pigment"
```

`instance Describable Color` provides the implementation for `Color`. Every
method declared in the class must appear in the instance body.

### Constraining a function

A function that needs `describe` to work on its argument declares the
requirement in a `where` clause:

```ridge
fn announce (x: a) -> Text where Describable a =
    $"Announcing: ${describe x}"
```

Call it with any type that has a `Describable` instance:

```ridge
announce Red     -- "Announcing: a red pigment"
```

If you call it with a type that has no `Describable` instance, the compiler
reports `T029 NoInstance` and tells you what to add.

### Deriving common instances

For the three most common classes — `Eq`, `ToText`, and `Ord` — you can ask
the compiler to generate the instance for you:

```ridge
type Priority = Low | Medium | High deriving (Eq, ToText, Ord)
```

`Eq` derives structural equality. `ToText` derives a text rendering (the
constructor name: `"Low"`, `"Medium"`, `"High"`). `Ord` derives an ordering
based on declaration order: `Low < Medium < High`.

For records, `ToText` produces `TypeName { field = value, ... }` in
declaration order:

```ridge
type Point = { x: Int, y: Int } deriving (Eq, ToText, Ord)

-- $"${Point { x = 3, y = 4 }}" renders as "Point { x = 3, y = 4 }"
```

### String interpolation and ToText

`$"..."` interpolation calls `toText` on each interpolated expression. A
type becomes interpolatable as soon as it has a `ToText` instance — either
derived, written by hand as `instance ToText T`, or via a `pub fn toText`
function in the same module (which is promoted automatically).

### A note on equality and secrets

`deriving Eq` uses BEAM structural equality (`=:=`), which is not
constant-time. Do not compare secret values — tokens, password hashes, HMAC
tags — with `==` or derived `Eq`. Use `Crypto.constantTimeEq` from `std.crypto`
instead; it takes two `Text` values and returns a `Bool` using a fixed-time
comparison that does not short-circuit on the first mismatched byte.

```ridge
import std.crypto as Crypto

-- Check a session token without leaking timing information. Both values must
-- be the same length; hash variable-length tokens to a fixed width first.
pub fn sessionMatches (provided: Text) (expected: Text) -> Bool =
    Crypto.constantTimeEq provided expected
```

Also note that `Float` has no `Eq` instance in the prelude. Deriving `Eq`
on a type that contains a `Float` field is a compile error (`T029
NoInstance`). Use an explicit comparison function with appropriate tolerance
if you need float equality.

### Deriving Encode and Decode

`Encode` and `Decode` are the two JSON typeclass instances. Derive both on a
record and the compiler generates a full codec with no boilerplate:

```ridge
type Person = { name: Text, age: Int } deriving (Eq, ToText, Encode, Decode)
```

The idiomatic pattern is a constrained helper — `where Encode a` makes the
`encode` method available inside the function body:

```ridge
fn toJson (x: a) -> Text where Encode a = Json.encode (encode x)
```

Use it to turn a `Person` into a JSON string:

```ridge
let person = Person { name = "Ann", age = 30 }
let json   = toJson person
-- json: "{\"name\":\"Ann\",\"age\":30}"
```

Decoding goes the other way. `Json.decode` parses the text into `JsonValue`,
then `decode` reconstructs the typed value:

```ridge
fn fromJson (s: Text) -> Result Person Error =
    match Json.decode s
        Ok j  -> decode j
        Err e -> Err e

let result = fromJson json
-- result: Ok (Person { name = "Ann", age = 30 })
```

`decode` returns `Result T Error`, so a malformed document or a missing
field produces an `Err` rather than a crash. The two directions round-trip
exactly: `decode (encode x) == Ok x` for any value that has both instances.

Deriving works on unions too. Nullary constructors encode as bare JSON
strings; constructors with payload encode as adjacently-tagged objects
(`{"tag":"…","values":[…]}`). Generic types get constrained instances
automatically — `type Box a = { val: a } deriving (Encode, Decode)` compiles
and round-trips for any element type that itself has `Encode` and `Decode`.

The stdlib ships eight parametric instances out of the box: `List a`,
`Option a`, `Map Text a`, and `Result a e` for both `Encode` and `Decode`,
so nested collections just work.

For the full grammar, coherence rules, and diagnostic reference, see
[`spec.md §5.6`](spec.md#56-typeclasses).

---

## Talking to a database

Ridge has a typed data layer — describe a table as a record, and a repository
reads and writes rows as that type, on SQLite or Postgres. The
[data guide](data.md) walks through connecting, migrations, and the query and
write API; a runnable CRUD tour lives in
[`examples/data/users-crud`](../examples/data/users-crud).

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
Windows). Reload the VS Code window after installing the extension.

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
