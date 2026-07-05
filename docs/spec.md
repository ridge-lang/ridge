# Ridge — Language Specification

**Version:** 0.3.0-rc4
**Author:** The Ridge Language Authors
**Last updated:** 2026-06-03

**History:** Supersedes `RILL_SPEC_AND_ROADMAP.md` (v0.1.0-draft, Rill). The language was renamed from *Rill* to *Ridge* after a design refinement pass. The underlying philosophy is preserved; the following are the substantive changes from the prior draft:
- Language name: **Ridge** (was *Rill*). File extension: **`.ridge`** (was `.rill`). Manifest: **`ridge.toml`** (was `rill.toml`).
- Effect system: **9 granular capabilities** with prefix list syntax (was binary `fn`/`fn!`).
- Multi-target strategy: **BEAM-primary with WebAssembly and native (LLVM) as exploratory backends** behind a target-neutral IR (changed from the fixed multi-target schedule of earlier drafts; see §11).
- **Workspace model** with architectural enforcement by the compiler — new first-class feature.
- **LSP and a package manager** are part of the toolchain rather than deferred extras.

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Design Philosophy & Non-Negotiables](#2-design-philosophy--non-negotiables)
3. [Language Overview](#3-language-overview)
4. [Formal Syntax Reference](#4-formal-syntax-reference)
5. [Type System](#5-type-system)
   - [5.6 Typeclasses](#56-typeclasses)
6. [Capabilities System](#6-capabilities-system)
7. [Semantic Model](#7-semantic-model)
8. [Project & Workspace Model](#8-project--workspace-model)
9. [Standard Library Scope](#9-standard-library-scope)
10. [Compiler Architecture](#10-compiler-architecture)
11. [Multi-Target Strategy](#11-multi-target-strategy)
12. [Appendices](#12-appendices)

---

## 1. Executive Summary

**Ridge** is a general-purpose programming language built around four pillars: **developer experience, safety from the root, first-class performance, and approachability**. It combines immutable data, actor-based concurrency, and a granular effect system visible in function signatures. Ridge compiles to Core Erlang for the BEAM runtime, which is the production target. The intermediate representation is held target-neutral; WebAssembly and native (LLVM) backends remain exploratory work kept feasible by the shared IR (see §11).

The target audience is software developers who want a language that scales from scripts to distributed systems without mode switching — fast to write, easy to reason about, hard to misuse.

**This document defines Ridge 0.3.0-rc4** — the language as it currently ships: typeclasses with deriving (including `Encode`/`Decode`), parametric instances, actors with bounded mailboxes, exhaustive pattern matching, a JSON codec, and the Core Erlang / BEAM backend. It covers language design, the type and capability systems, the standard library scope, the workspace model, and the compiler architecture.

### Elevator pitch

> Ridge is the only language where your architecture and your effects live in the type system, not in your PR reviews.

### Key characteristics

- **Compiled** to Core Erlang for the BEAM. WebAssembly and native (LLVM) backends are exploratory, gated by a target-neutral IR
- **Statically typed** with full Hindley-Milner inference
- **Immutable by default**, mutable state confined to actors
- **Actor-first concurrency** — millions of lightweight processes on BEAM
- **10 capabilities** (`io`, `fs`, `net`, `time`, `random`, `env`, `proc`, `spawn`, `ffi`, `db`) visible in function signatures
- **Workspace model** with architectural rules enforced by the compiler (forbid-arcs, per-project capability allow/deny)
- **No null** — `Option` and `Result` are the only way to express optionality and failure
- **Pipe-oriented composition** — `|>` is a first-class language construct
- **Pattern matching** everywhere, with exhaustiveness checking

### What distinguishes Ridge in the landscape

Two features are currently unmatched by any mainstream language:

1. **Capabilities are granular and visible in the type**. A function that can hit the network has `net` in its signature; one that can read the clock has `time`. This prevents a category of supply-chain attacks, enables deterministic testing without mocks, and makes refactors provably safe.
2. **Architectural rules are compiler-enforced via the workspace manifest**. `forbid = [{ from = "domain.*", to = "infra.*" }]` produces a compile error if a dependency crosses the line. Hexagonal / clean architecture stops being a convention and becomes a checked property.

---

## 2. Design Philosophy & Non-Negotiables

These are the principles that guide every design decision. When in doubt, refer back here.

### 2.1. Core Principles

1. **Readability over cleverness.** Code is read 10× more than written. Every syntactic choice prioritizes the reader.
2. **Explicitness where it matters, inference where it doesn't.** Types are inferred. Effects are not. Mutation is not. Concurrency is not.
3. **Make the right thing easy and the wrong thing hard.** Immutability is default. `null` doesn't exist. Mutation requires an actor. Effects require capabilities.
4. **One obvious way.** Avoid offering three syntaxes for the same thing.
5. **Learnability in a week, mastery in a year.** Surface area fits in a developer's head.
6. **Composition over inheritance, messages over methods, data over objects.**
7. **Architecture lives in the compiler.** Conventions that can be checked should be checked.

### 2.2. Non-Negotiables

These will not change, even under pressure.

| # | Rule | Rationale |
|---|------|-----------|
| N1 | No `null`, no `undefined`, no nil | All nullability through `Option`; no NPE class of bugs exists |
| N2 | No exceptions in user code | All errors through `Result`; crashes are actor-level only |
| N3 | No class inheritance | Composition + union types + typeclasses cover all legitimate cases |
| N4 | No shared mutable state | Mutation is scoped to actor internals; cross-actor is message-passing |
| N5 | No user-defined operators | Fixed set of built-ins; custom operations are named functions |
| N6 | No macros | Metaprogramming comes from `deriving`, not textual substitution |
| N7 | No reflection | Runtime type introspection is a code smell |
| N8 | Capabilities are tracked in the type | Fixed set of 10; no user-defined capabilities |
| N9 | Pattern matching is exhaustive | Non-exhaustive match is a compile error, not a warning |
| N10 | Everything is an expression (mostly) | Only `let`, `var`, `const`, `import` are statements |
| N11 | Architecture is enforced by the compiler | `forbid` rules in workspace manifest produce compile errors |
| N12 | IR is target-neutral | No backend-specific leakage in the IR; the shared IR keeps alternative backends feasible without committing to a schedule |

### 2.3. Deliberate trade-offs

- **We lose** fine-grained memory control (no manual allocation, no ownership tracking). Ridge is not for tight-loop numerical code or embedded systems. A native backend would narrow this gap and remains on the exploratory roadmap (§11).
- **We lose** familiarity for OO-native developers. The learning curve is steeper than "Java with better syntax."
- **We accept** a fixed capability set with no user-defined effects. Simpler than Koka/Eff; less expressive.
- **We gain** correctness by construction, concurrency without fear, code that survives refactors, and the only language-level architectural enforcement on the market.

---

## 3. Language Overview

### 3.1. "Hello, World"

```ridge
fn io main () -> Result Unit Error =
    Io.println "Hello, World"
    Ok ()
```

A `Unit`-returning `main` is also valid (`fn io main () = Io.println "Hello, World"`), but you lose `?` propagation.

### 3.2. Basic values and bindings

```ridge
-- Immutable binding (local scope)
let name = "Angel"           -- inferred: Text
let age: Int = 30            -- with annotation

-- Top-level constant (module scope)
const maxRetries: Int = 3

-- Mutable binding (only inside actors, or rarely in local scope)
var counter = 0
counter <- counter + 1       -- assignment uses `<-`

-- Shadowing is allowed
let x = 5
let x = x + 1                -- new binding, old x is unreachable
let x = $"Value: ${x}"       -- x is now Text
```

### 3.3. Functions

```ridge
-- Pure function (no capabilities)
fn greet (name: Text) -> Text =
    $"Hello, ${name}"

-- Function with a capability (io)
fn io log (msg: Text) -> Unit =
    Io.println msg

-- Multiple capabilities
fn fs net syncConfig (url: Text) -> Result Unit Error =
    let remote = Http.get url ?
    Fs.write "config.toml" remote ?
    Ok ()

-- Functions are curried by default
fn add (x: Int) (y: Int) -> Int = x + y
let addFive = add 5
let eleven = addFive 6

-- Calling: no parens for single arg, no parens for multiple either
greet "Angel"
add 3 4

-- Parens group expressions, not arguments
add (add 1 2) 3

-- Lambdas
let double = fn x -> x * 2
let sum = fn x y -> x + y

-- Field accessor as function
users |> List.map (.name)
```

#### Inner function declarations

A `fn` declaration inside another function body may declare its own capability prefix. The inner function's capability set must be a subset of the enclosing function's declared set.

```ridge
fn io fs main () -> Result Unit Error =
    fn io log (msg: Text) -> Unit = Io.println msg    -- OK: {io} ⊆ {io, fs}
    log "starting"
    Ok ()
```

Top-level `fn` declarations restrict their parameters to `Ident` or `(Ident: Type)` only. Inner `fn` declarations follow the same rule for their parameters but may freely declare capability prefixes up to the enclosing set.

### 3.4. Pipe operator

The pipe `|>` feeds the left value as the last argument of the right function. All stdlib functions follow the convention **"the main data argument comes last"** so pipes compose cleanly.

```ridge
-- Instead of this:
List.take 10 (List.sortBy (.name) (List.map (.email) (List.filter isActive users)))

-- Write this:
users
  |> List.filter isActive
  |> List.map (.email)
  |> List.sortBy (.name)
  |> List.take 10
```

### 3.5. Types

```ridge
-- Records
type User = {
    name: Text,
    email: Text,
    age: Int
}

-- Construction
let u = User { name = "Angel", email = "a@b.com", age = 30 }

-- Immutable update (LHS may be any expression; type checker verifies it is a record type)
let older = u with { age = 31 }

-- Field access
let n = u.name
```

The constructor name is **always required** in patterns and construction: write `User { name = n }`, never `{ name = n }`. Shorthand `{ name }` binds to a local variable named `name`, equivalent to `{ name = name }`. Mixed form: `User { name, email = e, age }`.

```ridge
-- Union types (algebraic data types)
type Shape =
    | Circle Float
    | Rectangle Float Float
    | Triangle Float Float Float

-- Union with record data
type Event =
    | Login { userId: Int, at: Timestamp }
    | Logout { userId: Int, reason: Text }

-- Generics via lowercase type variables
type Option a = | Some a | None
type Result a e = | Ok a | Err e
type List a = | Empty | Cons a (List a)     -- conceptual; List is built-in
```

### 3.6. Pattern matching

```ridge
-- match as expression
fn area (shape: Shape) -> Float =
    match shape
        Circle r          -> 3.14159 * r * r
        Rectangle w h     -> w * h
        Triangle a b c    ->
            let s = (a + b + c) / 2
            Float.sqrt (s * (s-a) * (s-b) * (s-c))

-- With guards
let category =
    match age
        n when n < 18  -> "Minor"
        n when n < 65  -> "Adult"
        _              -> "Senior"

-- Or-patterns: one arm matches any of several alternatives
match direction
    North | South -> "vertical"
    East | West   -> "horizontal"

-- `as` patterns (bind the whole and the parts)
match user
    admin @ User { role = Admin } -> handleAdmin admin
    other                         -> handleOther other

-- Shorthand field binding in patterns: `{ name }` ≡ `{ name = name }`
match user
    User { name, age } -> $"${name} is ${age}"

-- Destructuring in let — full patterns including tuples and records
let (x, y) = point
let (User { name }, count) = pair           -- tuple with nested record pattern
fn distance (x1, y1) (x2, y2) = Float.sqrt ((x2-x1)^2 + (y2-y1)^2)
```

**Pattern scope rules:** `let` bindings and lambda parameters accept full patterns (tuples, records with shorthand, constructor patterns, wildcards, as-patterns). Top-level `fn` declarations are restricted to `Ident` or `(Ident: Type)` — destructure inside the body via `let` or `match`.

**Or-patterns** (`p1 | p2 | …`) are valid only at the root of a `match` arm, not nested inside another pattern. Every alternative must bind the same variables, and each shared binding must have the same type across alternatives — so `Plus x | Minus x -> x` is allowed while `Some x | None -> …` is rejected. An arm covers the union of its alternatives for exhaustiveness checking.

### 3.7. Implicit prelude

Every Ridge module has a set of names in scope without any `import` declaration.  The prelude is resolved in `prelude_resolutions()` (`crates/ridge-resolve/src/imports.rs`) and is injected before per-module import resolution.  User imports for the same `local_name` take priority and suppress the prelude binding (no collision error).

**Prelude scope:**

| Local name | Source | Kind | Notes |
|------------|--------|------|-------|
| `Option` | `std.option` | `StdlibSymbol` | Type name |
| `Some` | `std.option` | `StdlibSymbol` | Constructor |
| `None` | `std.option` | `StdlibSymbol` | Constructor |
| `Result` | `std.result` | `StdlibSymbol` | Type name |
| `Ok` | `std.result` | `StdlibSymbol` | Constructor |
| `Err` | `std.result` | `StdlibSymbol` | Constructor |
| `Int` | `std.int` | `ModuleAlias` | Enables `Int.parse`, `Int.toText`, … |
| `Float` | `std.float` | `ModuleAlias` | Enables `Float.fromInt`, `Float.round`, … |
| `Bool` | `std.bool` | `ModuleAlias` | Enables `Bool.not`, … |
| `Text` | `std.text` | `ModuleAlias` | Enables `Text.padLeft`, `Text.split`, … |
| `List` | `std.list` | `ModuleAlias` | Enables `List.map`, `List.fold`, … |
| `Map` | `std.map` | `ModuleAlias` | Enables `Map.empty`, `Map.insert`, … |
| `Set` | `std.set` | `ModuleAlias` | Enables `Set.fromList`, `Set.union`, … |
| `Json` | `std.json` | `ModuleAlias` | Enables `Json.encode`, `Json.decode` |

Capability-bearing modules (`std.io`, `std.fs`, `std.net.http`, `std.time`, `std.random`, `std.env`, `std.cli`, `std.proc`) are **not** in the prelude and require an explicit `import` declaration.  This keeps every side-effecting dependency visible at the import level.

#### Option and Result

```ridge
-- ? propagates None or Err upward
fn getUserEmail (userId: Int) -> Result Text Error =
    let user = fetchUser userId ?
    let email = user.email ?
    Ok email

-- try block: all ? inside propagate; no nested callbacks
fn fs net processOrder (orderId: Int) -> Result OrderResult Error =
    try
        let order = fetchOrder orderId ?
        let user = fetchUser order.userId ?
        let payment = chargeCard user.card order.total ?
        Ok { order = order, payment = payment }
```

`try { ... }` is a **value-producing expression**: it yields `Result`/`Option`. An unused non-`Unit` result produces a compiler warning (`discarded_result`). To explicitly discard, use `Result.discard : Result a e -> Unit` or `Option.discard : Option a -> Unit` from the stdlib, or use `match`. Ridge has no monadic do-notation — `try` + `?` is the idiomatic chaining mechanism.

### 3.8. Capabilities (effects)

Ridge has **10 capabilities** visible in every function signature. They form a closed set — users cannot define new ones.

| Capability | Covers |
|------------|--------|
| `io` | stdout, stderr, stdin, logging |
| `fs` | files, directories, metadata, paths |
| `net` | HTTP, TCP, UDP, DNS, sockets |
| `time` | clock, timers, sleep, timeouts |
| `random` | PRNG, UUIDs, crypto entropy |
| `env` | environment variables, argv |
| `proc` | exec of external commands, signals |
| `spawn` | creating actors, workers |
| `ffi` | calls to external code (C, BEAM foreign) |
| `db` | database access (Postgres, SQLite) |

```ridge
-- Pure: no capabilities, deterministic
fn double (x: Int) -> Int = x * 2

-- Uses io
fn io log (msg: Text) -> Unit = Io.println msg

-- Uses fs and net
fn fs net downloadAndCache (url: Text) -> Result Path Error = ...
```

**Rule:** a function may only call other functions whose capability set is a subset of its own. Pure functions may only call pure functions. The compiler enforces this. See [§6](#6-capabilities-system).

### 3.9. Actors

```ridge
actor Counter =
    state count: Int = 0

    on increment =
        count <- count + 1

    on decrement =
        count <- count - 1

    on get -> Int =
        count

-- Spawning returns a Handle
fn spawn main () =
    let c = spawn Counter
    c ! increment                -- send (fire-and-forget)
    c ! increment
    let n = c ?> get             -- ask (synchronous reply)
    ...
```

Handlers may declare capabilities; the actor's effective capability set is the union of its handlers'. Capabilities of handlers are **encapsulated**: callers of `?>` inherit only `time` (for the implicit timeout), not the handler's capabilities. See [§6.4](#64-actor-encapsulation-model-b).

#### §3.9.x. init blocks

When actor state cannot be given a compile-time default, an `init` block initialises it at spawn time.

- An actor has at most one `init` block.
- Syntax: `init [capList] (params) = body`
- If `init` is present, `spawn ActorName arg1 arg2` passes arguments positionally to `init`.
- If `init` is absent, all `state` fields must have defaults (preserves current behaviour).
- Inside `init`, assign state fields with `<-`. Other expressions are allowed.
- Callers of `spawn` do **not** inherit `init`'s capabilities (consistent with handler encapsulation). Only the `spawn` capability is required in the caller.

```ridge
actor Worker =
    state limiter: Handle Limiter
    state count: Int = 0

    init (l: Handle Limiter) =
        limiter <- l

    on time tick () -> Unit =
        match (limiter ?> allow)
            true  -> count <- count + 1
            false -> ()

-- Spawn passes args positionally to init
let w = spawn Worker limiterHandle
```

#### §3.9.x. Mailbox configuration

An actor's mailbox can be configured via an optional `mailbox` member
alongside `state`, `init`, and `on`:

```ridge
actor RateLimiter =
    mailbox bounded 1000 drop newest
    state tokens: Int = 100

    on consume () -> Bool =
        if tokens > 0 then
            tokens <- tokens - 1
            true
        else
            false
```

Three forms are accepted:

| Form | Behaviour |
|------|-----------|
| (omitted) | Unbounded mailbox. The default. |
| `mailbox unbounded` | Unbounded, explicit. |
| `mailbox bounded N drop newest` | Bounded at `N`. On overflow: silently drop the incoming message. |
| `mailbox bounded N error` | Bounded at `N`. On overflow: caller signals failure (see §7.2.1). |

When `bounded N` is specified, an overflow policy is **mandatory**. Writing
`mailbox bounded N` without a policy produces a parse error
(`P022 MailboxPolicyMissing`). `N` must be a literal integer in
`1..=i64::MAX`; zero, negative, or overflowing values produce
`P023 MailboxBoundInvalid`.

A third policy, `drop oldest`, is parsed but not yet implemented;
programs using it produce a type-check error
(`T027 MailboxPolicyDropOldestNotShipped`) until the broker mechanism it
requires ships in a future release. See §7.2.1 for the full semantics — how
overflow is surfaced through `!`, how the bound is enforced under
contention, and how observability composes via `Actor.mailboxSize`.

### 3.10. String interpolation

```ridge
Io.println $"User ${user.name} has ${user.age} years"
Io.println $"Total: ${items |> List.map (.price) |> List.sum}"
```

String interpolation dispatches through the `ToText` class (§5.6). Built-in types (`Int`, `Float`, `Bool`, `Text`, `Timestamp`) have prelude instances. User-defined types become interpolatable by adding `deriving (ToText)` to the type declaration or by writing an explicit `instance ToText T`. See §5.6. Interpolation also has a multi-line block form, `$"""..."""` (§4.1.1).

### 3.11. Modules and imports

```ridge
-- File libs/domain/src/Users.ridge defines module acme.domain.Users

import std.list as List
import std.map (get, insert)
import std.text (trim, split, lines)

import acme.shared.Text
import acme.infra.Postgres as Pg

-- Visibility: lowercase-starting names are module-internal by default
-- Export a symbol with `pub`; make it visible only inside the same
-- namespace with `pub(internal)`; prefix with `_` for file-private
pub type User = { name: Text, email: Text }
pub(internal) fn normalizeEmail (e: Text) -> Text = ...
fn _helper x = ...
```

See [§8](#8-project--workspace-model) for the full visibility model.

### 3.12. Guard clauses

```ridge
fn process (user: User) -> Result Unit Error =
    guard user.active else return Err (Inactive user.id)
    guard user.age >= 18 else return Err (Underage user.id)

    -- main logic with no nesting
    processInternal user
```

---

## 4. Formal Syntax Reference

### 4.1. Lexical grammar

#### Keywords (reserved)

```
actor    as       catch    class    const    deriving else
false    fn       guard    if       import   in       init
instance let      match    on       opaque   pub      return
spawn    state    then     true     try      type     var
when     where    with
```

#### Capability keywords (soft-reserved, contextual)

These are keywords only after `fn` or `on`; elsewhere they are ordinary identifiers.

```
io    fs    net    time    random    env    proc    spawn    ffi    db
```

Note: `spawn` appears both as a top-level keyword (the spawn expression) and as a capability. The parser disambiguates by position; see §4.2 for details.

#### Special tokens

```
|>   pipe
<-   mutation assignment
?    propagate Option/Result (postfix)
?>   actor ask (send-and-reply)
!    actor send (fire-and-forget)
::   list cons
++   concatenation
->   function type arrow, match arm
=>   (reserved, not currently used)
@    as-pattern binder
..   rest pattern element in list and record patterns (see §4.5)
```

#### Identifiers

- Lowercase-starting: values, functions, type variables, capability keywords
- Uppercase-starting: types, constructors, modules
- Must match: `[a-zA-Z][a-zA-Z0-9_]*`
- Underscore prefix `_` marks private/unused
- Identifiers are ASCII-only; source files are UTF-8 (string literals and comments may contain any Unicode).

#### Literals

```
Int:     42, -17, 1_000_000, 0xFF_FF, 0b1010_0101, 0o755
         (0x hex, 0b binary, 0o octal; _ digit separator; prefix letters and hex digits case-insensitive)
Float:   3.14, -0.5, 1.5e10
Text:    "hello", "escape \n \t \" \\ \r \0 \u{1F600}"
Text:    $"interpolated ${expr}"
Bool:    true, false
Unit:    ()
List:    [1, 2, 3], []
Tuple:   (1, "two", 3.0)
```

String escapes: `\n`, `\t`, `\"`, `\\`, `\r`, `\0`, `\u{HHHH}`. Multi-line and raw string literals are described in §4.1.1 below.

#### 4.1.1. Multi-line and raw string literals

**Multi-line strings (`"""..."""`)** extend the single-line `"..."` form, which remains single-line only. The two forms do not overlap.

```ridge
let query = """
    SELECT id, name
    FROM users
    WHERE active = true
    """

let html = """
    <p>Hello, ${user.name}</p>
    """
```

Syntax rules:

- Opening delimiter: `"""` followed immediately by a newline. The newline after the opening `"""` is dropped and does not appear in the value.
- Closing delimiter: a newline, zero or more spaces, then `"""`. The newline before the closing `"""` is also dropped.
- Indentation stripping: the column position of the closing `"""` defines the margin. That many leading spaces are stripped from every interior line. A line with fewer spaces than the margin is a parse error.
- Blank interior lines are allowed and survive as empty lines in the value.
- Standard escape sequences are processed normally — triple-quoted strings are cooked, not raw.
- A plain triple-quoted string does not interpolate: a `${...}` sequence is literal text. For interpolation spanning multiple lines use the `$"""..."""` form below.

**Interpolated multi-line strings (`$"""..."""`)** combine the two: the triple-quote block layout with the `${...}` holes of `$"..."` (§3.10). The dedent rules are identical to `"""` — the opener is followed immediately by a newline, the closing `"""` sets the margin, and interior lines are dedented by it — and each `${...}` hole is evaluated through `ToText` exactly as in the single-line form.

```ridge
let body = $"""
    Dear ${user.name},
    Your balance is ${account.balance}.
    """
```

**Raw strings (`r"..."`, `r#"..."#`, `r##"..."##`)** disable escape processing entirely. Every byte between the delimiters is literal.

```ridge
let pattern = r"\d+\.\d+"          -- no escape processing
let withQuote = r#"say "hello""#    -- interior " balanced by # pairs
let multiline = r"first line
second line"                        -- spans newlines without dedenting
```

Syntax rules:

- `r"..."` — no escape sequences; interior `"` is not permitted.
- `r#"..."#` — interior `"` is allowed as long as it is not followed by `#`. Extend to `r##"..."##` when the content needs `"#` sequences, and so on for deeper nesting.
- Raw strings may span multiple lines. Unlike `"""..."""`, there is no dedenting — the content is taken literally.
- `r` immediately followed by `"` or one or more `#` then `"` is always a raw string. Applying the function `r` to a string requires a space: `r "x"`.

#### Comments

```
-- Line comment

---
   Doc comment (block)
   Supports markdown.
---
```

### 4.2. Indentation rules

Ridge uses **significant indentation** (offside rule), similar to Haskell/F#/Elm.

- A block is introduced by `=`, `->`, `then`, or `else`.
- The block's contents must be indented strictly deeper than the opening line.
- Within a block, all items must be at the same indentation level.
- Tabs are forbidden. Only spaces. (Enforced by lexer; error on tab.)
- Indentation unit convention: 4 spaces (not enforced, but the formatter uses it).
- **Layout is partially suppressed inside brackets.** While the bracket-nesting depth (count of open `(`, `[`, `{` not yet matched) is greater than zero, `INDENT` and `DEDENT` tokens are never emitted. However, a `NEWLINE` token _is_ emitted when a logical line begins at column ≤ the baseline column of the first continuation line inside the bracket — this marks a statement boundary inside parenthesised lambda bodies and similar constructs. When depth returns to zero, full layout (including `INDENT`/`DEDENT`) resumes.
- `spawn` appears both as a top-level keyword (the spawn expression) and as a capability keyword. The parser disambiguates by position: `spawn ActorName args...` is a spawn expression; `fn spawn f ...` declares the `spawn` capability. Arguments to `spawn` are passed positionally to the actor's `init` block if present.

### 4.3. BNF grammar (selected productions)

This is a simplified EBNF. The full grammar lives in `docs/grammar.ebnf`.

```ebnf
Program       = { TopLevel } .
TopLevel      = Import | TypeDecl | FnDecl | ActorDecl | ConstDecl | ClassDecl | InstanceDecl .

Import        = "import" ModulePath [ "as" Ident ] [ "(" IdentList ")" ] .

ConstDecl     = "const" Ident ":" Type "=" Expr .

TypeDecl      = "type" UpperIdent [ TypeParams ] "=" TypeBody .
TypeBody      = RecordType | UnionType | AliasType .
RecordType    = "{" FieldList "}" .
UnionType     = { "|" Constructor } .
Constructor   = UpperIdent { Type } | UpperIdent RecordType .

FnDecl        = [ "pub" [ "(" "internal" ")" ] ] "fn" { Capability } Ident { Param } [ "->" Type ] "=" Expr .
Capability    = "io" | "fs" | "net" | "time" | "random" | "env" | "proc" | "spawn" | "ffi" | "db" .
Param         = Ident | "(" Ident ":" Type ")" .

ActorDecl     = [ "pub" ] "actor" UpperIdent "=" { ActorMember } .
ActorMember   = StateDecl | OnHandler | InitBlock .
StateDecl     = "state" Ident ":" Type [ "=" Expr ] .
OnHandler     = "on" { Capability } Ident { Param } [ "->" Type ] "=" Expr .
InitBlock     = "init" { Capability } "(" ParamList ")" "=" Expr .

Expr          = LetExpr | MatchExpr | IfExpr | TryExpr | GuardExpr
              | LambdaExpr | PipeExpr | AppExpr | Literal | Ident .

MatchExpr     = "match" Expr { MatchArm } .
MatchArm      = Pattern [ "when" Expr ] "->" Expr .

Pattern       = Literal | Ident | "_" | Constructor { Pattern }
              | "(" PatternList ")" | "[" ListPatternList "]"
              | RecordPattern | Ident "@" Pattern | Ident "::" Pattern .

ListPatternList = [ ListPatternElem { "," ListPatternElem } ] .
ListPatternElem = Pattern | RestPattern .
RestPattern     = ".." | Ident "@" ".." .

RecordPattern   = Constructor "{" FieldPatternList [ "," ".." ] "}" .

PipeExpr      = Expr "|>" Expr .
AppExpr       = Expr Expr .
LambdaExpr    = "fn" { Param } "->" Expr .

SpawnExpr     = "spawn" UpperIdent { Expr } .
SendExpr      = Expr "!" Expr .
AskExpr       = Expr "?>" Expr .
```

Note: The full normative grammar lives in `docs/grammar.ebnf`. The productions above are illustrative selections; consult that file for the complete specification.

### 4.4. Operator precedence (low to high)

| Precedence | Operators | Associativity | Notes |
|------------|-----------|---------------|-------|
| 1 | `\|>` | left | |
| 2 | `\|\|` | right | |
| 3 | `&&` | right | |
| 4 | `==` `!=` | none | |
| 5 | `<` `>` `<=` `>=` | none | |
| 6 | `++` `::` | right | |
| 7 | `+` `-` | left | |
| 8 | `*` `/` `%` | left | |
| 9 | `^` | right | |
| 10 | `-` (unary negate) | n/a | no prefix `!`; negation is `Bool.not` |
| 11 | `!` `?>` (send / ask) | left | actor message operators |
| 12 | function application | left | |
| 13 | `?` (postfix propagate), `.` (field access) | left | call-suffix band |

### 4.5. Rest patterns in list and record patterns

**List patterns** match against the `List a` type.

A fixed-length list pattern matches exactly N elements:

```ridge
match xs
    []        -> "empty"
    [x]       -> "one element"
    [x, y]    -> "two elements"
    [x, y, z] -> "three elements"
    _         -> "other"
```

A single `..` in any position matches zero or more remaining elements without binding them:

```ridge
match xs
    [first, ..]       -> first    -- head of a non-empty list
    [.., last]        -> last     -- last element
    [first, .., last] -> (first, last)
```

Bind the rest using the as-pattern operator `@`:

```ridge
match xs
    [first, rest @ ..] -> -- first: a; rest: List a
    [first, mid @ .., last] -> -- mid: List a
```

Constraints:
- At most one `..` per list pattern (`P024 MultipleRestInListPattern`).
- The elements after the rest (`suffix` and `middle` positions) must be simple bindings or wildcards. A refutable element in a suffix or middle position is rejected at the lowering stage (`L009`).
- Matching a trailing element or binding the middle requires traversing the full list, since lists are singly linked. This is ergonomically convenient, not cheap.

**Record rest patterns** match a constructor carrying at least the named fields, ignoring any others:

```ridge
match event
    Login { userId, .. }  -> handleLogin userId
    Logout { userId, .. } -> handleLogout userId
```

The `..` is a modifier on the field set, not a sub-pattern, and cannot be bound. A record pattern without `..` matches exactly the fields named; with `..` it matches any value of that constructor type that carries at least those fields.

---

## 5. Type System

### 5.1. Foundations

Ridge's type system is based on **Hindley-Milner with extensions**:

- Full type inference (no annotation required anywhere).
- Algebraic data types (sum and product).
- Parametric polymorphism with let-generalization.
- Type classes with constraints (see §5.6).
- **Capability inference** alongside type inference (see §5.3).

**Not yet supported:**
- Row polymorphism for records.
- Higher-kinded types.

### 5.2. Built-in types

```
Int        -- 64-bit signed integer
Float      -- 64-bit IEEE 754 double
Bool       -- true | false
Text       -- UTF-8 string (BEAM binary internally)
Unit       -- () — the single-value type
List a     -- immutable linked list
Map k v    -- immutable map (persistent hash map)
Set a      -- immutable set
Option a   -- Some a | None
Result a e -- Ok a | Err e
Handle a   -- reference to a spawned actor of type a
Timestamp  -- opaque; no literal syntax; see §9.2 std.time for construction
```

### 5.3. Type inference algorithm

Algorithm W (Damas-Hindley-Milner) with union-find, generalization at `let`, instantiation at use sites, and the occurs check. Capability sets are inferred in a second pass: the set of a function is the union of capabilities used in its body. If a declared signature is present, the inferred set must be a subset; otherwise, a compile error.

```
Error: function 'f' declared as `fn io` uses capability `fs`
  at src/Main.ridge:12
  |
  12 |  fn io procesarConfig (path: Text) =
     |      ^^ declared here
  |
  The call to `Fs.readFile` requires `fs`.
  Options:
    - Add `fs` to the signature: `fn io fs procesarConfig`
    - Remove the call to `Fs.readFile`
```

### 5.4. Pattern exhaustiveness

Pattern matching must be exhaustive. The compiler implements **Maranget's algorithm** to determine exhaustiveness and report missing cases with examples:

```
Error: non-exhaustive match
  at src/Shape.ridge:12
  |
  12 |   match shape
     |         ^^^^^
  Missing cases:
    Triangle _ _ _
```

### 5.5. No subtyping

Ridge has **no implicit subtyping**. No `Dog <: Animal`. This keeps inference decidable and predictable. Polymorphism is achieved through:

1. Parametric polymorphism (generics).
2. Union types (closed polymorphism).
3. Typeclasses (open polymorphism — see §5.6).

### 5.6. Typeclasses

Ridge has typeclasses: named interfaces that a type can satisfy, with coherence guarantees enforced by the compiler. Dispatch is dictionary-passing — no runtime type tags, no reflection, near-zero overhead at monomorphic call sites.

#### 5.6.1. Class declarations

A class declaration names an interface and lists bare method signatures. The `class` keyword is followed by the class name, one or more type variables, optional functional dependencies, an optional superclass list, and `=`:

```ridge
class ToText a =
    toText (x: a) -> Text

class Eq a =
    eq (x: a) (y: a) -> Bool

class Ord a where Eq a =
    compare (x: a) (y: a) -> Ordering
```

Method signatures inside a class body are **bare**: no `fn` keyword, no body. A class body is a list of contracts. Default method bodies are not supported.

Superclasses are declared with a `where` clause between the type variable and `=`. `class Ord a where Eq a` means every `Ord` instance requires a corresponding `Eq` instance for the same type. An empty class body is rejected (`P030 MalformedClassDecl`).

```ebnf
ClassDecl    ::= "class" UpperIdent TyVar+ [ "|" FunDeps ] [ "where" SuperList ] "=" NEWLINE
                 INDENT MethodSig+ DEDENT
FunDeps      ::= FunDep { "," FunDep }
FunDep       ::= TyVar+ "->" TyVar+
SuperList    ::= ClassConstraint { "," ClassConstraint }
ClassConstraint ::= UpperIdent TyVar+
MethodSig    ::= LowerIdent ParamList "->" Type NEWLINE
```

#### 5.6.2. Instance declarations

An instance declaration provides method bodies for a specific type:

```ridge
instance ToText Color =
    toText (c: Color) -> Text = match c
        Red   -> "red"
        Green -> "green"
        Blue  -> "blue"
```

Each method definition has the same form as a top-level `fn` body — `name (params) -> RetType = body` — without the `fn` keyword. An instance must define every method declared by the class. An empty instance body is rejected (`P031 MalformedInstanceDecl`).

```ebnf
InstanceDecl ::= "instance" UpperIdent Type [ WhereClause ] "=" NEWLINE
                 INDENT MethodDef+ DEDENT
MethodDef    ::= LowerIdent ParamList "->" Type "=" Expr NEWLINE
```

An instance head may be a type constructor applied to a type variable, with the element constraints written in a trailing `where` clause:

```ridge
instance Encode (List a) where Encode a =
    encode (xs: List a) -> JsonValue =
        JList (List.map (fn e -> encode e) xs)
```

`instance Encode (List a) where Encode a` reads "given an `Encode` instance for `a`, here is an `Encode` instance for `List a`." The element instance is supplied at runtime: the compiler passes the element's dictionary to the parametric instance when a concrete `List Int`, `List Text`, etc. is encoded. The `where` clause uses the same syntax as constraints on function signatures (§5.6.3); Ridge has no `=>` arrow.

The element type must be determinable where the method is used. The dictionary for each element is chosen from the full resolved argument type, so `encode (Some 5)` and `encode (Some "hi")` dispatch to the `Int` and `Text` encoders respectively. If the element type is left open — `encode None`, or `encode []` with no annotation — there is no way to choose an element encoder, and the constraint is reported as ambiguous (`T030 AmbiguousConstraint`). Annotate the value (`let xs : List Int = []`) to fix the element type.

The prelude already provides parametric `Encode` and `Decode` instances for `List a`, `Option a`, `Map Text a`, and `Result a e`. These four type constructors are **reserved** for the prelude — a user instance such as `instance Encode (List MyType)` overlaps the prelude instance and is rejected (`T032 OverlappingInstance`). To customise encoding for a contained type, write the instance for that element type, not for the container.

A class may take more than one type variable. The head of each instance then lists one type per parameter:

```ridge
class Convert a b =
    convert (x: a) -> b

instance Convert Celsius Fahrenheit =
    convert (c: Celsius) -> Fahrenheit = ...
```

Coherence is keyed by the whole head tuple, so instances that share a leading type but differ later coexist (`Convert Celsius Fahrenheit` and `Convert Celsius Kelvin` are distinct). A call selects the instance from the types at every head position; a position the caller leaves undetermined is reported as an ambiguous constraint to annotate (`T030 AmbiguousConstraint`).

A **functional dependency** records that some parameters determine others, written after the type variables as `| determining -> determined`:

```ridge
class Tagged q p | q -> p =
    tagWith (tag: p) (x: q) -> q
```

`| q -> p` declares that `q` fixes `p`. No two instances may agree on `q` while differing on `p` — a violation is `T046 ConflictingFunDep` — and once `q` is known the compiler infers `p` from the matching instance. This both resolves a determined position the call site leaves open, so a method whose result type the dependency fixes needs no annotation, and *checks* a determined position the call site supplies: a type the dependency forbids is rejected at compile time rather than dispatched by the head's outer constructor alone. A dependency may name several variables on either side (`| a b -> c`), and a class may list several, comma-separated (`| a -> b, b -> a`). Every name must be one of the class's own type variables, or it is reported as `T045 UnknownFunDepVar`.

#### 5.6.3. Constraints on function signatures

A function can require class instances for its type variables using a `where` clause after the return type:

```ridge
fn describe (x: a) -> Text where ToText a =
    $"value: ${x}"

fn sortPair (a: a) (b: a) -> (a, a) where Ord a =
    if compare a b == Less then (a, b) else (b, a)
```

Multiple constraints are comma-separated: `where Ord a, Eq a`. At every call site, the compiler checks that the concrete type has the required instances; if not, it emits `T029 NoInstance`.

```ebnf
WhereClause ::= "where" ClassConstraint { "," ClassConstraint }
```

The `where` clause is appended to the function signature between the return type and `=`.

#### 5.6.4. Deriving

The `deriving` clause on a type declaration generates instances automatically. The clause is written after the type body:

```ridge
type Color = Red | Green | Blue deriving (Eq, ToText, Ord)

type Point = { x: Int, y: Int } deriving (Eq, Ord)

type Person = { name: Text, age: Int } deriving (Encode)
```

The derivable classes are `Eq`, `ToText`, `Ord`, `Encode`, and `Decode`. `Show` is accepted as an alias for `ToText` in `deriving` (and elsewhere); both refer to the same class.

```ebnf
Deriving ::= "deriving" "(" UpperIdent { "," UpperIdent } ")"
```

**Derived `Eq`** generates a structural equality check using BEAM `=:=`. For records (represented as Erlang maps) and unions (tagged tuples or bare atoms), BEAM structural equality is correct. No `Eq` instance exists for `Float` in the prelude — floating-point equality is a footgun. A type with a `Float` field is rejected when `Eq` is derived (`T029 NoInstance`).

**Security note:** `=:=` is not constant-time. Do not derive `Eq` on types carrying secret data (tokens, password hashes, HMACs, session keys). Use `std.crypto.constantTimeEq` instead:

```ridge
import std.crypto as Crypto

-- Compare two equal-length byte sequences in constant time.
-- Both inputs must have the same length.
let safe = Crypto.constantTimeEq tokenA tokenB
```

`constantTimeEq (a: Text) (b: Text) -> Bool` wraps `crypto:hash_equals/2` from the OTP `crypto` application and takes the same amount of time regardless of how many bytes match.

**Derived `ToText`** generates a human-readable rendering. For record types, fields are rendered in declaration order:

```
Point { x = 3, y = 4 }
```

For union types, nullary constructors render as their name; constructors with payload render as `CtorName(field1, field2, ...)`:

```
Red
Some(42)
```

Each field's rendering dispatches through the `ToText` instance for that field's type; if a field type has no `ToText` instance, the compiler emits `T029 NoInstance` at derive time.

**Derived `Ord`** generates a `compare` method returning `Ordering`. For record types, fields are compared in declaration order; the first field that is not `Equal` determines the result. For union types, variants are compared by their declaration position first (`Red < Green < Blue`); if both values have the same constructor and it carries payload, payload fields are compared in order. `Ord` requires `Eq` for the same type (checked by coherence; missing → `T033 MissingSuperclassInstance`).

**Derived `Encode`** generates an `encode` method that converts a value to `JsonValue`. The encoding follows a DX-first hybrid wire format:

- **Record** → a JSON object whose keys are the field names (in declaration order) and whose values are the recursively-encoded fields. `Person { name = "Ann", age = 30 }` encodes to `{"name":"Ann","age":30}`.
- **Nullary union constructor** → a bare JSON string. `Admin` encodes to `"Admin"`.
- **Payload union constructor** → an adjacently-tagged JSON object. `Circle 3.0` encodes to `{"tag":"Circle","values":[3.0]}`. This shape round-trips cleanly with `deriving (Decode)`.
- **`Option T`** → `T | null`. `Some "Bob"` encodes to `"Bob"`; `None` encodes to `null`.
- **`List T`** → a JSON array. `["a", "b"]` encodes to `["a","b"]`.
- **`Map Text T`** → a JSON object whose keys are the map's `Text` keys and whose values are the recursively-encoded map values.
- **`Result T E`** → adjacently-tagged, same as a payload union: `Ok x` → `{"tag":"Ok","values":[encode x]}`; `Err e` → `{"tag":"Err","values":[encode e]}`.
- **Nested derived type** → calls that type's `encode` method recursively.

The deriver recurses over the concrete field type, so `List Text`, `Option Int`, and `Map Text Bool` fields are all supported without any extra constraints.

A generic type — one with a type parameter, such as `type Box a = { val: a } deriving (Encode)` — derives a **constrained** instance. The compiler synthesises `instance Encode (Box a) where Encode a`: a field whose type is the parameter `a` encodes through the element's `Encode` instance, supplied at runtime, exactly like the prelude container instances. A field that mentions a type variable which is *not* one of the type's own parameters is still rejected (`T029 NoInstance`), since there is no parameter through which to thread its dictionary.

**Derived `Decode`** generates a `decode` method that converts a `JsonValue` back into a value of the derived type. The method signature is `decode : JsonValue -> Result T Error`, the inverse of `encode`. It consumes exactly the same wire format that `deriving (Encode)` produces, so `encode` and `decode` round-trip: `decode (encode x) == Ok x`.

The decoding rules mirror the encoding rules above:

- **Record** → expects a `JObject`. Each declared field is looked up in the JSON object by name; a missing field short-circuits with `Err { code = "decode.missing_field", … }`. A field value of the wrong JSON kind short-circuits with `Err { code = "decode.expected_int"` (or `"decode.expected_string"`, etc.), … }`.
- **Nullary union constructor** → expects `JText "CtorName"`. An unknown tag short-circuits with `Err { code = "decode.unknown_tag", … }`.
- **Payload union constructor** → expects a `JObject` with `"tag"` and `"values"` keys. The tag string selects the constructor; the `values` array must have exactly as many elements as the constructor expects (`decode.bad_arity` otherwise).
- **`Option T`** → `JNull` decodes to `None`; any other JSON value is decoded as `T` and wrapped in `Some`.
- **`List T`** → expects a `JArray`. Each element is decoded individually; the first failure short-circuits (fail-fast, not accumulate-all).
- **`Map Text T`** → expects a `JObject`. Each value is decoded individually; the first failure short-circuits.

Decoding is fail-fast: the first error encountered is immediately returned. Use `Err` values from the `Error` record (`{ code: Text, message: Text }`) to inspect what went wrong. A generic type derives a constrained `Decode` instance the same way `Encode` does, so `type Box a = { val: a } deriving (Encode, Decode)` round-trips over any element type that itself has both instances.

#### 5.6.5. The `Ordering` type

`Ordering` is a prelude type with three constructors, used as the return type of `compare`:

```ridge
pub type Ordering = Less | Equal | Greater
```

It has prelude `Eq`, `Ord`, and `ToText` instances. `Less`, `Equal`, and `Greater` are available without any import.

#### 5.6.6. `ToText` and string interpolation

`pub fn toText` functions are automatically promoted to `ToText` instances. Any top-level `pub fn toText (x: T) -> Text` in a module is treated exactly as if the user had written `instance ToText T`. This means existing code with hand-written `toText` functions continues to work without changes.

If a type has both a `pub fn toText` and an explicit `instance ToText T`, the compiler emits `T034 ToTextConflict` and stops. Remove one or the other.

`Show` is an alias for `ToText`. Writing `instance Show T` or `deriving (Show)` is identical to writing `instance ToText T` or `deriving (ToText)`.

#### 5.6.7. Coherence

The compiler enforces three coherence rules across the whole workspace:

**One instance per (class, type) pair.** Declaring a second `instance C T` for the same class and type — whether explicit or via `deriving` — is a compile error (`T032 OverlappingInstance`).

**Orphan rule.** An `instance C T` must be defined in the module that declares `C` or the module that declares `T`. An instance written elsewhere is an orphan and rejected (`T031 OrphanInstance`). This prevents any module from silently changing the behaviour of a security-critical class for a type it doesn't own.

**Superclass requirement.** When an instance `C T` is declared or derived, every superclass of `C` must also have an instance for `T`. Missing → `T033 MissingSuperclassInstance`. The class hierarchy must be acyclic; a cycle is caught at class collection time (`T035 SuperclassCycle`).

#### 5.6.8. Diagnostic codes

| Code | Name | Trigger |
|------|------|---------|
| P030 | MalformedClassDecl | A `class` declaration is structurally invalid (empty body, `fn` keyword in signature, method body inside class, etc.) |
| P031 | MalformedInstanceDecl | An `instance` declaration is structurally invalid (empty body, missing method body, etc.) |
| T029 | NoInstance | A constrained call site has no instance for the required class, or `deriving` cannot produce one (e.g. `Float` field with `Eq`) |
| T031 | OrphanInstance | Instance defined outside the class's module and the type's module |
| T032 | OverlappingInstance | A second instance for the same (class, type) pair |
| T033 | MissingSuperclassInstance | Instance declared but required superclass instance is absent |
| T034 | ToTextConflict | Type has both a `pub fn toText` auto-instance and an explicit `instance ToText T` |
| T035 | SuperclassCycle | The class hierarchy contains a cycle |

#### 5.6.9. Multi-parameter classes

A class may take more than one type parameter:

```ridge
class Convert a b =
    convert (x: a) -> b

instance Convert Celsius Fahrenheit =
    convert (c) = ...
```

An instance head supplies one type per class parameter, written as a sequence
of type atoms (parenthesise an applied type, e.g. `(List a)`). Coherence is
keyed by the whole head tuple, so `Convert Celsius Fahrenheit` and
`Convert Celsius Kelvin` are distinct instances and coexist. A call resolves
the instance from the type at every head position; when a position is left
undetermined — for example a result type the caller never fixes — the
constraint is ambiguous and must be annotated.

#### 5.6.10. Quotation

A lambda passed where a `Quote` is expected is **captured as an expression
tree** rather than compiled to a closure — the model C# uses for
`Expression<Func<>>`. This is how a query predicate is written in native
syntax yet compiled to something other than a function:

```ridge
fn showUserPred (q: Quote (User -> Bool)) -> Text = Query.debugShow q

-- the lambda is captured, not called:
showUserPred (fn u -> u.age >= 18 && u.active)
```

`Quote f` carries the quoted shape in its phantom parameter (here
`User -> Bool`) so the surrounding code can keep a value and its predicate in
agreement. Inside the quote the parameter stands for the entity's columns:
`u.age` resolves to the `age` column of `User`, and the body is checked by a
small dedicated pass — not the ordinary operator typing — that accepts column
references, literals, the six comparisons, and `&&`/`||`. A boolean column is a
predicate on its own (`fn u -> u.active`).

The captured body becomes a `QExpr` value. Field accesses are recorded by their
SQL column name (a `signupYear` field becomes `signup_year`), so the tree is
ready to compile to SQL. Diagnostics: `T039` (a field that is not a column),
`T040` (a form the quote does not support), `T041` (a comparison whose sides
disagree), `T042` (the entity cannot be determined — annotate the parameter).

#### 5.6.11. What is not yet supported

The following are deferred to future releases:

- Default method bodies in class declarations
- Functional dependencies
- Newtype deriving

---

## 6. Capabilities System

### 6.1. The 10 capabilities

Ridge has a **closed set** of 10 capabilities. Users cannot define new ones. This is deliberately less expressive than Koka/Eff but radically simpler to teach and to debug.

| Capability | Covers | Why separate |
|------------|--------|--------------|
| `io` | stdout, stderr, stdin, logging | Console I/O is distinct from disk or network |
| `fs` | files, directories, metadata, paths | Local data attack surface differs from network |
| `net` | HTTP, TCP, UDP, DNS, sockets | Supply-chain security: flags exfiltration |
| `time` | clock, timers, sleep, timeouts | Determinism boundary for tests |
| `random` | PRNG, UUIDs, crypto entropy | Determinism boundary |
| `env` | environment variables, argv | Configuration surface; matters in tests |
| `proc` | exec of external commands, signals | Worst-of-worst: arbitrary code execution |
| `spawn` | creating actors or workers | Concurrency visible in signatures |
| `ffi` | calls to external code (C, BEAM foreign) | Escape hatch — breaks all guarantees |
| `db` | database queries and transactions | Narrow, auditable grant; the adapters bridge it to `net`/`fs` so query sites never hold raw network or filesystem access |

### 6.2. Syntax: prefix list

```ridge
fn double (x: Int) -> Int = x * 2

fn io log (msg: Text) -> Unit = Io.println msg

fn fs net syncConfig (url: Text) -> Result Unit Error =
    let remote = Http.get url ?
    Fs.write "config.toml" remote ?
    Ok ()
```

Capabilities appear between `fn` (or `on`) and the function name, in any order but conventionally alphabetical.

Inner `fn` declarations inside a function body may also declare a capability prefix; the inner function's capability set must be a subset of the enclosing function's declared set (see §3.3 for example).

### 6.3. Propagation rules

1. **Pure functions may only call pure functions.** Calling `Io.print` from `fn f` is a compile error.
2. **`fn X f` may call `fn g` (pure) and `fn Y h` where `Y ⊆ X`.** A caller must have at least the capabilities of the callee.
3. **Inference + verification.** The compiler infers the capability set of a body; if a signature is declared, the body's set must be a subset. If not, the error suggests either adding the missing capability or removing the offending call.
4. **Transitive subset rule for inner functions.** If an inner `fn` declaration inside a function body declares a capability prefix, that inner function's capability set must be a subset of the enclosing function's declared (or inferred) capability set. This rule applies transitively through nested inner functions.

### 6.4. Actor encapsulation (Model B)

Capabilities of an actor's handlers are **encapsulated** within the actor. A caller of `?>` (ask) inherits only `time` — for the implicit timeout — not the capabilities that the handler uses internally.

```ridge
actor Cache =
    state data: Map Text Text = Map.empty

    -- This handler uses fs (reads from disk on miss)
    on fs time get (key: Text) -> Option Text = ...

-- Caller only needs `spawn` (for spawn) and `time` (for the ask timeout),
-- NOT `fs`, even though the handler uses it.
fn spawn time main () =
    let cache = spawn Cache
    let v = cache ?> get "k"
    ...
```

Rationale: actors are mental models of separate processes with their own effects. Internal capabilities should not leak through the handle. This preserves the conceptual isolation of actors as independent runtime units.

The compiler still verifies that the actor itself has the right capabilities (its declared set is the union of its handlers'), within the project where the actor is defined.

#### §6.4.1. Handles as effect tokens

Operations that take a `Handle a` and act on the corresponding actor's
local state or mailbox do not require additional capabilities beyond
possessing the handle. The handle itself is the proof of access: it was
produced by `spawn` (which carries the `spawn` capability) or returned
from another function that already accounted for the access.

Cap-free actor-local operations:

- `actor ! msg` — send (operator). Fire-and-forget.
- `Actor.mailboxSize actor` — read mailbox occupancy. See §9.

Operations that produce additional effects beyond the actor itself still
carry their capabilities:

- `actor ?> msg [timeout T]` — ask. Requires `time` for the timeout
  primitive (not for the actor access).
- `spawn ActorName` — requires `spawn` capability. Produces the handle.

The principle generalises: any future actor-local primitive that takes a
`Handle a` and observes or mutates only that actor's queue or state is
cap-free. Primitives that reach outside the handle's actor — runtime
introspection that does not key on a specific handle, cross-actor
coordination — keep whichever capability already classifies them.

### 6.5. Project-level capabilities

The workspace manifest can restrict capabilities per project:

```toml
# libs/domain/ridge.toml
[capabilities]
allow = []                 # domain is 100% pure — not even io

# apps/api/ridge.toml
[capabilities]
allow = ["io", "fs", "net", "spawn", "time"]
# proc and ffi absent → denied
```

Enforcement happens **before** type-check. A function declaring an unpermitted capability produces:

```
Error: capability 'net' not allowed in project 'acme.domain'
  archivo: libs/domain/src/UseCase.ridge:12
  12 | fn net calcular (u: User) -> Result Money Error = ...
     |    ^^^
  The project 'acme.domain' declares allow = [] in ridge.toml
  Options:
    - Move this function to a project with capability 'net' (e.g. acme.infra)
    - Refactor to inject the network call as a dependency
```

### 6.6. Semantics: static flags, manual DI

Capabilities are **compile-time tags only**. There are no replaceable handlers at runtime. Testing is done via **dependency injection**: pass functions as arguments.

```ridge
-- Production code — fn time, clock is Time.now
fn time calcularVencimiento (t: Ticket) -> Timestamp =
    Time.now () + t.duration

-- Testable version — dependency injection
fn calcularVencimientoCon (clock: Fn () -> Timestamp) (t: Ticket) -> Timestamp =
    clock () + t.duration

-- Production:
calcularVencimientoCon Time.now ticket

-- Test:
calcularVencimientoCon (fn () -> fakeTime) ticket
```

Replaceable capability handlers (à la Roc platforms) remain under consideration if demand arises.

### 6.7. Capability polymorphism in higher-order functions

Higher-order stdlib functions like `List.forEach`, `List.map`, `Result.andThen` must not force a single capability on their callback. Ridge solves this with a **capability variable** in the signature — the caps of the callback flow through to the caller at each call site. This is not a typeclass; it's a single effect variable in the type system.

```ridge
-- Stdlib signature (c is a capability-set variable):
pub fn c List.forEach (xs: List a) (f: fn c a -> Unit) -> Unit = ...

-- Call site 1 — pure callback: forEach is pure.
[1, 2, 3] |> List.forEach (fn x -> x * 2)

-- Call site 2 — io callback: forEach is fn io at this site.
fn io printAll (items: List Text) -> Unit =
    items |> List.forEach Io.println

-- Call site 3 — fs callback: forEach is fn fs at this site.
fn fs writeAll (paths: List Path) -> Unit =
    paths |> List.forEach (fn p -> Fs.write p "")
```

The compiler unifies `c` with the callback's inferred capability set at each use and propagates it to the caller's capability set. This keeps the stdlib single-variant and keeps user code free of capability-suffixed helpers.

---

## 7. Semantic Model

### 7.1. Evaluation order

- **Strict evaluation** (not lazy). Arguments are evaluated before the function is called.
- **Left-to-right** evaluation of function arguments.
- **Pipe** `a |> f` is exactly equivalent to `f a` — same evaluation semantics.

### 7.2. Actor semantics

Each actor is a lightweight process — a BEAM process on the production target. A native backend would map actors to green threads under an M:N scheduler.

- **`actor ! msg`** (send): asynchronous, returns immediately, returns `Unit`. No capability required beyond having the handle.
- **`actor ?> msg`** (ask): synchronous from caller's perspective, blocks the calling process until reply. Requires `time` in the caller (for the timeout), nothing else.
- Each actor processes one message at a time, FIFO.
- Actor state is private; no direct access from outside.
- Message send is one-way; ask is implemented as send + await reply with a reference.

#### §7.2.1. Mailbox configuration

By default, an actor's mailbox is **unbounded**: senders never block,
never fail; the only limit is the underlying runtime's per-process memory
budget. This is the default when the `mailbox` actor member is omitted
(see §3.9).

An actor can opt into a **bounded** mailbox by declaring a `mailbox`
member with a capacity `N` and an explicit overflow policy.

**Overflow policies.**

| Policy | On overflow (via `!`) |
|--------|----------------------|
| `drop newest` | Silently drop the incoming message. `!` returns `Unit` as always. |
| `error` | Raise an exit signal in the sender (`{mailbox_full, Pid}` on BEAM). Let-it-crash; if supervised, the supervisor decides what happens next. |

Choosing between the two is a value judgement, not a structural one:
`drop newest` favours the actor's liveness over delivery guarantees;
`error` favours backpressure visibility over fire-and-forget ergonomics.
Neither is a default; an `error`-policied actor must be paired with a
caller (or supervisor) that knows how to respond to the signal.

The `drop oldest` policy (silently drop the head-of-queue message on
overflow) is **parsed but not yet implemented**. Programs using it
produce `T027 MailboxPolicyDropOldestNotShipped` until the policy ships
in a future release. Implementing it requires a broker process
intermediary, because the BEAM does not permit a sender to mutate
another process's mailbox; the broker holds the bounded queue and
forwards under the cap. Reserving the syntax now keeps the eventual
broker rollout from re-shaping the surface grammar.

**Order, fairness, FIFO.** Independently of the mailbox configuration,
an actor processes one message at a time in FIFO order. Bounded
mailboxes do not introduce priority queueing or selective receive.

**Best-effort bound under contention.** The bound is enforced at send
time by sampling the receiver's queue length. Under concurrent senders
the queue may briefly exceed the declared cap by a small margin between
the sample and the cast; the bound is therefore a *soft* invariant, not
a strict one. Use cases that need a strict bound need the broker-based
policies that are still planned.

**Observability.** A live actor's mailbox occupancy is read via
`Actor.mailboxSize : Handle a -> Option Int`. `Some n` is the queue
length at the moment of the call; `None` means the actor is no longer
alive (crashed, never existed, or pending restart). `Actor.peek` and
`Actor.drain` are planned for a future release.

**Capabilities.** All mailbox operations (`!`, `Actor.mailboxSize`) are
**cap-free**: the handle is the effect token (see §6.4.1). `?>` (ask)
keeps `time` because of the timeout primitive, not because of the
mailbox access.

### 7.3. Memory model

- All values are immutable except actor `state`.
- Sharing structurally-equal data is a compiler optimization (persistent data structures).
- On BEAM: garbage collection is per-process (no global GC pauses); process memory is isolated; messages are copied between processes.
- A native backend would use concurrent GC (Go-style) with per-actor heaps where possible.

### 7.4. Error handling model

- **Recoverable errors**: `Result a e` — handled explicitly.
- **Programming errors**: runtime crashes (index out of bounds, match failure at runtime, etc.) — the actor dies. Supervisors can restart it.
- **No exceptions** in user code. Period.

### 7.5. Module semantics

- One file = one module.
- Module name derived from project name + file path: `apps/api/src/handlers/Users.ridge` in project `acme.api` → `acme.api.handlers.Users`.
- Circular imports are a compile error.
- Visibility has two layers: manifest (per-module) and code (`pub`, `pub(internal)`, `_`). See [§8.4](#84-visibility-model).

---

## 8. Project & Workspace Model

One of Ridge's defining features: the build system is a first-class language concept, and architectural rules are enforced by the compiler.

### 8.1. Layout

```
acme-platform/
├── ridge.toml                    ← workspace root (analogous to .sln)
├── libs/
│   ├── shared/
│   │   ├── ridge.toml
│   │   └── src/
│   │       ├── Text.ridge            → module acme.shared.Text
│   │       └── Types/Id.ridge        → acme.shared.Types.Id
│   ├── domain/
│   │   ├── ridge.toml
│   │   └── src/
│   │       ├── Models/User.ridge
│   │       └── UseCases/RegisterUser.ridge
│   └── infra/
│       ├── ridge.toml
│       └── src/Postgres.ridge
├── apps/
│   ├── api/
│   │   ├── ridge.toml
│   │   └── src/Main.ridge
│   └── cli/
│       ├── ridge.toml
│       └── src/Main.ridge
└── tests/
    └── domain_test/
        ├── ridge.toml
        └── src/RegisterUserTest.ridge
```

### 8.2. Workspace manifest (`ridge.toml` root)

```toml
[workspace]
name = "acme-platform"
version = "0.1.0"
members = [
    "apps/*",
    "libs/*",
    "tests/*"
]

# Shared dependencies
[workspace.dependencies]
json = { version = "1.0" }
http = { version = "2.3" }
postgres = { git = "github.com/ridge-lang/postgres", tag = "v0.2" }

# Architectural rules enforced by compiler
[workspace.rules]
forbid = [
    { from = "acme.domain.*", to = "acme.infra.*" },
    { from = "acme.domain.*", to = "acme.api.*" },
    { from = "acme.shared.*", to = "acme.*" }
]

# Globally-denied capabilities
[workspace.capabilities]
deny = ["ffi"]
```

### 8.3. Project manifest (`ridge.toml` per project)

```toml
[project]
name = "acme.domain"
version = "0.1.0"
kind = "library"           # library | app | service | test
# entry = "src/Main.ridge"    # required when kind = app | service

[project.src]
root = "src"               # default: "src"

[project.exports]
public = ["Models.*", "UseCases.*"]   # glob of modules visible outside project
internal = []                          # visible only inside same namespace
# everything else is project-private by default

[dependencies]
shared = { workspace-member = "shared" }
json = { workspace = true }             # inherited from workspace.dependencies
helpers = { path = "../helpers" }
extra = { git = "github.com/x/y", tag = "v1.0" }

[capabilities]
allow = []                 # domain = 100% pure
# deny = ["ffi"] inherited from workspace
```

### 8.4. Visibility model

Two layers combined:

**Layer 1 — manifest (module-level):** `[project.exports].public` lists modules importable from outside the project; `.internal` lists modules importable only from other projects in the same namespace. Everything else is project-private.

**Layer 2 — code (symbol-level):**
- `pub type Foo = ...` — exported from the module.
- `pub(internal) fn helper = ...` — visible inside the same namespace only.
- `fn _private = ...` or `type _Internal = ...` (underscore prefix) — file-private.
- No modifier → project-private (visible within the project but not outside).

### 8.5. Dependency kinds

```toml
dep1 = { path = "../lib" }                        # local path
dep2 = { git = "github.com/x/y", tag = "v1.0" }   # git, pinned by tag
dep3 = { git = "...", branch = "main" }           # git, floating
dep4 = { workspace = true }                       # from workspace.dependencies
dep5 = { workspace-member = "domain" }            # another member in the same workspace
dep6 = { hex = "1.2.3" }                          # from hex.pm
```

### 8.6. Forbid rules

`[workspace.rules] forbid` is a list of `{ from, to }` pairs where `from` and `to` are module globs. The compiler builds a module dependency graph and produces a compile error on any forbidden edge:

```
Error: forbidden dependency
  file:   libs/domain/src/UseCases/RegisterUser.ridge:5
  rule:   acme.domain.* cannot depend on acme.infra.*
  source: workspace ridge.toml, [workspace.rules]

  5 | import acme.infra.Postgres as Pg
      |        ^^^^^^^^^^^^^^^^^^^^^^^

  Suggestion: define a port (trait/interface) in acme.domain and inject it
              from acme.api when composing the graph.
```

### 8.7. CLI

```bash
ridge build               # build the whole workspace respecting dependencies
ridge build --member api  # only apps/api and its dependencies
ridge check               # type-check without codegen (fast feedback)
ridge run --member api    # build and run
ridge test                # run all tests
ridge test --member X     # run tests of member X
ridge test --filter G     # run tests whose qualified name matches glob G
ridge fmt                 # format the whole workspace (opinionated, no config)
ridge fmt --migrate-tests # rewrite prefix-style test functions to @test form
ridge new <name>          # scaffold a new project
ridge init                # initialize a workspace in the current directory
ridge repl                # interactive REPL
```

**Test discovery.** `ridge test` recognises two forms:

1. `@test "<name>"` attribute on any function, regardless of name or visibility (canonical). See §8.8.
2. `pub fn test_<name> ()` function-name prefix (deprecated, `C304 PrefixTestDeprecated`).

Both forms must return `Result Unit Text`. Tests run in a fresh BEAM child process per test; no shared state leaks between runs. FFI-bearing tests are rejected with a compile-time capability error. When both forms apply to the same function the attribute wins and the test registers once.

### 8.8. Test declaration with `@test`

`@test "<name>"` marks a function as a test regardless of its name or visibility:

```ridge
@test "returns greeting for known user"
fn greetingForKnownUser () -> Result Unit Text =
    let result = greet "Angel"
    if result == "Hello, Angel" then Ok ()
    else Err $"expected Hello, Angel; got ${result}"

@test "login event carries user id"
pub fn loginEventShape () -> Result Unit Text =
    match Login { userId = 1, at = Time.epoch () }
        Login { userId, .. } ->
            if userId == 1 then Ok ()
            else Err "wrong userId"
        _ -> Err "wrong variant"
```

Rules:

- The argument to `@test` must be a string literal (`P027 TestAttrArgNotString` otherwise).
- The string is the display name shown by `ridge test`.
- The function must return `Result Unit Text`.
- Visibility does not matter — private functions can be tests.
- When both `@test` and the `test_` prefix apply, the attribute takes precedence and the test registers once.
- The `test_` prefix convention is deprecated (`C304 PrefixTestDeprecated`).

`ridge fmt --migrate-tests` rewrites prefix-style tests to the `@test` form in place. It inserts `@test "<derived-name>"` above each `pub fn test_<name>` and does not rename the function, so existing references remain valid. The derived name is the function name with its `test_` prefix removed (e.g. `test_empty_list` → `@test "empty_list"`). The rewrite is idempotent: a function already carrying `@test` is left untouched.

---

## 9. Standard Library Scope

### 9.1. Core modules

| Module | Purpose | Key functions |
|--------|---------|---------------|
| `std.int` | Integer ops | `toText`, `parse`, `abs`, `min`, `max` |
| `std.float` | Float ops | `toText`, `parse`, `round`, `floor`, `ceil`, `sqrt` |
| `std.bool` | Boolean helpers | `not`, `and`, `or` |
| `std.text` | Text ops | `byteSize`, `concat`, `split`, `splitN`, `splitAny`, `lines`, `trim`, `toUpper`, `toLower`, `startsWith`, `endsWith`, `contains`, `replace`, `padLeft`, `padRight`, `isEmpty` |
| `std.list` | List ops | `empty`, `length`, `isEmpty`, `head`, `tail`, `map`, `filter`, `filterMap`, `fold`, `foldRight`, `reverse`, `sort`, `sortBy`, `take`, `drop`, `groupBy`, `flatMap`, `zip`, `zipWith`, `contains`, `find`, `any`, `all`, `range`, `rangeExclusive`, `forEach` |
| `std.map` | Persistent map | `empty`, `fromList`, `toList`, `insert`, `remove`, `get`, `contains`, `keys`, `values`, `map`, `filter`, `size`, `merge`, `update` |
| `std.set` | Persistent set | `empty`, `fromList`, `toList`, `insert`, `remove`, `contains`, `union`, `intersect`, `difference`, `size` |
| `std.option` | Option helpers | `withDefault`, `map`, `flatMap`, `orElse`, `isSome`, `isNone`, `discard` |
| `std.result` | Result helpers | `map`, `mapErr`, `flatMap`, `withDefault`, `isOk`, `isErr`, `discard` |

*Note:* `length` is **reserved** for future codepoint-aware semantics. `byteSize` returns the byte count under UTF-8 encoding; character/grapheme counting will arrive with `length` in a later release.

**Convention:** in every stdlib function, the "main data" argument comes **last**, so pipes compose naturally:

```ridge
users |> List.map (.email) |> List.filter isValid |> List.take 10
```

**Logical negation** is `Bool.not : Bool -> Bool`. There is no prefix `!` for negation; `!` is exclusively the actor-send operator.

`Result.discard : Result a e -> Unit` and `Option.discard : Option a -> Unit` are the explicit way to silence the `discarded_result` compiler warning when a `try` or `?` expression result is intentionally unused.

### 9.2. Capability-bearing modules

| Module | Capability | Purpose |
|--------|-----------|---------|
| `std.io` | `io` | `print`, `println`, `readLine`, `eprint` |
| `std.fs` | `fs` | `readFile`, `writeFile`, `append`, `exists`, `lines` |
| `std.time` | `time` | `now : fn time () -> Timestamp`, `epoch : Timestamp`, `fromIso : Text -> Result Timestamp Error`, `diff`, `diffMs`, `sinceMs`, `sleep`, `Duration`, `parse`, `iso` |
| `std.random` | `random` | `int`, `float`, `alphanumeric`, `choice`, `seed` |
| `std.env` | `env` | `get`, `set`, `all` |
| `std.cli` | `env` | `args`, `exit` (built on top of env) |
| `std.proc` | `proc` | `exec`, `run` |
| `std.net.http` | `net` | `get`, `post`, `put`, `delete`, `listen` |

### 9.3. JSON

`std.json` ships as part of the standard library. It is a thin module over the prelude `JsonValue` type (§9.4): a parser, a serializer, and the bridge to `deriving (Encode, Decode)`.

| Function | Signature | Purpose |
|----------|-----------|---------|
| `Json.encode` | `JsonValue -> Text` | Serialize a `JsonValue` tree to a JSON string |
| `Json.decode` | `Text -> Result JsonValue Error` | Parse a JSON string into a `JsonValue` tree |

For typed values, `deriving (Encode)` produces an `encode : T -> JsonValue` method and `deriving (Decode)` produces `decode : JsonValue -> Result T Error` (§5.6.4). The usual round-trip is `Json.encode (encode value)` to produce a string and `decode value |> Result.flatMap ...` from a parsed tree.

```ridge
type Person = { name: Text, age: Int } deriving (Encode, Decode)

let p = Person { name = "Ann", age = 30 }
let text = Json.encode (encode p)            -- {"name":"Ann","age":30}

let parsed = Json.decode text ?              -- JsonValue
let back = decode parsed ?                   -- Person
```

### 9.4. The `JsonValue` prelude type

`JsonValue` is a first-class prelude type — in scope in every module without an import — that models a parsed JSON document:

```ridge
pub type JsonValue =
    | JNull
    | JBool Bool
    | JInt Int
    | JFloat Float
    | JText Text
    | JList (List JsonValue)
    | JObject (Map Text JsonValue)
```

It is the canonical intermediate representation between Ridge values and JSON text. `Json.decode` produces a `JsonValue`; `Json.encode` consumes one. Derived `Encode`/`Decode` methods convert between user types and `JsonValue` (§5.6.4), so the typical flow is `T → JsonValue → Text` on the way out and `Text → JsonValue → T` on the way in. Building a `JsonValue` by hand is also supported — pattern-match and construct its variants directly when a value's shape is dynamic.

### 9.5. Additional modules

| Module | Purpose | Status |
|--------|---------|--------|
| `std.regex` | Regular expressions | Planned |
| `std.terminal` | Terminal control | Planned |

---

## 10. Compiler Architecture

### 10.1. High-level pipeline

```
Source (.ridge)
    |
    v
+------------+
|   Lexer    |  tokens, including INDENT/DEDENT
+------------+
    |
    v
+------------+
|   Parser   |  AST (untyped)
+------------+
    |
    v
+---------------------+
| Resolve & Imports   |  symbol tables, module graph, workspace rules
+---------------------+
    |
    v
+----------------------+
| Type & Cap Checker   |  typed AST, capability info, exhaustiveness
+----------------------+
    |
    v
+----------------------+
| Lowering             |  simplified IR (Ridge Core IR)
+----------------------+
    |
    +------------------+------------------+
    |                  |                  |
    v                  v                  v
+----------+    +----------+       +----------+
| codegen  |    | codegen  |       | codegen  |
|   erl    |    |   wasm   |       |   llvm   |
| (active) |    | (explor.)|       | (explor.)|
+----+-----+    +----+-----+       +----+-----+
     |               |                   |
     v               v                   v
   erlc           (direct)            clang+lld
     |               |                   |
     v               v                   v
  .beam           .wasm              native binary
```

### 10.2. Crate layout (Cargo workspace)

```
ridge/
├── Cargo.toml                  # workspace
├── crates/
│   ├── ridge-lexer/            # Tokenization + layout
│   ├── ridge-parser/           # AST construction
│   ├── ridge-ast/              # AST types (shared)
│   ├── ridge-resolve/          # Name resolution, imports, workspace rules
│   ├── ridge-stdlib/           # Ridge stdlib (.ridge sources under stdlib/<module>.ridge)
│   ├── ridge-types/            # Type checker (HM) + capability checker
│   ├── ridge-ir/               # Ridge Core IR
│   ├── ridge-lower/            # AST → IR
│   ├── ridge-codegen-erl/      # IR → Core Erlang (active backend)
│   ├── ridge-codegen-wasm/     # IR → WASM (exploratory; stub today)
│   ├── ridge-codegen-llvm/     # IR → LLVM IR (exploratory; stub today)
│   ├── ridge-diagnostics/      # Error formatting
│   ├── ridge-driver/           # Compilation orchestration
│   ├── ridge-cli/              # Binary entry point
│   ├── ridge-lsp/              # Language server
│   └── ridge-pkg/              # Package manager
├── tests/
├── examples/
└── docs/
    ├── grammar.ebnf
    ├── spec.md
    └── tutorial.md
```

The Ridge `.ridge` stdlib sources live at `crates/ridge-stdlib/stdlib/<module>.ridge`, with `net/http.ridge` as the single nested module.

### 10.3. Key dependencies

| Crate | Purpose | Justification |
|-------|---------|---------------|
| `logos` | Lexer generator | Fast, well-documented, handles layout with custom logic |
| `chumsky` | Parser combinators | Excellent error recovery, great diagnostics, idiomatic Rust |
| `ariadne` | Diagnostics rendering | Beautiful error output, works well with chumsky |
| `la-arena` | Arena allocation for AST | Cheap IDs, cache-friendly |
| `rustc-hash` | Fast hashmaps | Used throughout the compiler |
| `insta` | Snapshot testing | Core to parser/typechecker test strategy |
| `clap` | CLI parsing | Standard choice for CLI tools |
| `tower-lsp` | LSP framework | For ridge-lsp |
| `toml` + `serde` | Manifest parsing | For ridge-pkg and workspace manifest |

### 10.4. Error handling in the compiler

- Every phase produces `Result<Output, Vec<Diagnostic>>`.
- Diagnostics are accumulated; compilation continues to collect multiple errors when safe.
- `ariadne` renders diagnostics with source spans, labels, and suggestions.
- No `panic!` in the compiler under any user input. Panics are bugs.

---

## 11. Multi-Target Strategy

Ridge is **BEAM-primary**. BEAM is the production target; the language, the standard library, the actor model, and the tooling are all designed against it and validated there. Alternative backends (WebAssembly and native via LLVM) remain on the roadmap as **exploratory work**, contingent on user traction rather than a fixed schedule. The intermediate representation is held target-neutral as a design discipline so the option to activate a second backend stays open without dictating when.

This section describes the present target, the exploratory ones, the disciplines that keep them feasible, and the cadence on which they are re-evaluated.

### 11.1. BEAM (production)

BEAM is the runtime the language was designed against. Benefits inherited for free:
- Preemptive M:N scheduler with the BeamAsm JIT (35+ years of optimization).
- Per-process GC, no global stop-the-world.
- OTP: supervisors, gen_server, gen_statem, distributed BEAM.
- Live tracing, `observer`, `recon`.
- Production-grade networking and crypto.

Mapping:
- Ridge actors → gen_server processes.
- Ridge types → erased at runtime (BEAM is dynamically typed).
- Ridge stdlib → thin wrappers over Erlang/OTP primitives.

### 11.2. Exploratory backends

Two alternative backends remain in the roadmap as exploratory work. Neither has a fixed schedule; both are gated on user traction signals — concrete deployment requirements that BEAM cannot serve, expressed as reports against the public tracker.

#### 11.2.1. WebAssembly

WebAssembly would unlock browser playgrounds, edge functions, and embeddable Ridge runtimes. The work splits in two natural phases:

- **WASM limited.** Pure code and deterministic capabilities (`time`, `random` via host-provided shims), single-threaded, no actors, no async I/O. Target use cases: in-browser playground and stateless edge functions.
- **WASM complete.** Actors via the WASM threads proposal, WASI for `fs`/`net`/`proc`, WASM GC where available. Target use cases: production edge computing.

Both phases remain exploratory until user traction justifies the investment. The `ridge-codegen-wasm` crate exists today as a stub guarded by the target-neutrality test (`crates/ridge-lower/tests/neutrality.rs`), preserving the option without committing to a schedule.

Candidate deployment targets, when and if the work activates: Cloudflare Workers, Fastly Compute@Edge, Fermyon Spin, wasmtime, wasmer, browsers.

#### 11.2.2. Native via LLVM

A native backend would unlock compute-bound workloads, fast CLI startup (<10 ms vs. 50–100 ms on BEAM), standalone binaries without the Erlang runtime, and embedded or constrained environments.

It requires a custom runtime — comparable in effort to the existing BEAM backend, effectively a second compiler:

- M:N actor scheduler (Go-style or BEAM-style; decision deferred until activation).
- Garbage collection — per-actor heaps where possible, global concurrent GC as the baseline. Reference-counting and ownership are out by design (the first contradicts persistent immutable cycles; the second contradicts Ridge's promise that the programmer doesn't think about memory).
- Explicit data-layout decisions for each type (List, Map, Union, Text, Record).
- FFI and system integration via C ABI.
- Concurrency primitives — MPMC channels, scheduler synchronization, async I/O.
- Debugging and observability — DWARF, profiler integration.

The `ridge-codegen-llvm` crate exists today as a stub guarded by the same target-neutrality test.

### 11.3. The target-neutrality discipline

The IR is held free of backend-specific assumptions. This is not a hint or a code-review check; it is enforced:

- `crates/ridge-lower/tests/neutrality.rs` asserts the IR carries no backend-specific leakage.
- The stub `ridge-codegen-wasm` and `ridge-codegen-llvm` crates compile against every PR, so any change that breaks target-neutrality fails CI.

The cost of the discipline is small — roughly a 5% tax on lowering and IR design work. It buys the option to activate a second backend later without redoing the frontend.

### 11.4. Re-evaluation

Whether the exploratory backends are ever activated is re-evaluated periodically against three signals:

- **User traction.** Concrete deployment requirements BEAM cannot serve, expressed in public reports against the project tracker.
- **Capacity.** The project is solo-maintained; activating a second backend is a substantial commitment that competes with BEAM-side work.
- **Ecosystem state.** Where the WebAssembly and native ecosystems stand at the time — component model, WASI preview, WASM GC standardization, LLVM IR stability.

If user reports concentrate on a deployment shape that BEAM cannot reach (cold-start-sensitive edge functions, in-browser tooling, standalone CLI distribution), the relevant backend moves up the priority list.

### 11.5. Strategic principles

1. **BEAM is the production target.** The language and tooling ship against BEAM today; alternative backends do not gate any 0.x release.
2. **The IR is the contract.** Backends consume the IR; they need nothing else from the frontend. The IR stays target-neutral.
3. **Capabilities are target-agnostic.** They are compile-time checks; runtime cost is zero on any target.
4. **Public messaging.** Ridge is a typed functional language for the BEAM. WebAssembly and native (LLVM) backends are exploratory, kept feasible by the shared IR but not committed to a schedule.

---

## 12. Appendices

### Appendix A — Canonical example programs

_Note: in these examples, `?>` is the ask operator and `main` returns `Result Unit Error`._

Four programs compile and run correctly and serve as acceptance tests for the compiler.

- **A.1. Log analyzer** (`examples/log_analyzer.ridge`) — file IO, text parsing, pattern matching, list ops.
- **A.2. URL shortener** (`examples/url_shortener.ridge`) — actors, concurrency, HTTP, JSON.
- **A.3. Game of Life** (`examples/game_of_life.ridge`) — actors, timers, terminal IO, Set.
- **A.4. Rate limiter per IP** (`examples/rate_limiter.ridge`) — actors, `spawn`, `?>` (ask), state, `time`, `Map`. Exercises the actor model end-to-end.

### Appendix B — Glossary

- **Actor**: isolated process with private state and a mailbox for messages.
- **Ask**: synchronous message send with reply (`handle ?> msg`).
- **BEAM**: the Erlang virtual machine.
- **Capability**: a tag in a function's signature that names an effect the function may perform (`io`, `fs`, etc.).
- **Core Erlang**: Erlang's intermediate representation; Ridge's production compilation target.
- **Forbid rule**: a compiler-enforced architectural constraint declared in `[workspace.rules]` that prevents module-to-module dependencies.
- **HM**: Hindley-Milner type system.
- **IR (Ridge Core IR)**: target-neutral intermediate representation; the contract between frontend and backends.
- **Monomorphization**: compiling a generic function to one concrete version per use.
- **Send**: asynchronous message send (`handle ! msg`).
- **Typeclass**: an interface-like construct for ad-hoc polymorphism (§5.6).
- **Workspace**: a tree of related projects, rooted at a `ridge.toml` with `[workspace]`.

### Appendix C — References

- **Gleam**: https://gleam.run — closest existing language to Ridge in spirit; proved BEAM + static types works; reference for hex.pm integration.
- **Elm**: https://elm-lang.org — reference for error messages and simplicity.
- **Roc**: https://www.roc-lang.org — reference for effects and platform model; Ridge's capabilities are simpler by choice.
- **Rust**: https://www.rust-lang.org — compiler implementation language.
- **Koka**: https://koka-lang.github.io — reference for algebraic effects; Ridge deliberately uses a closed set of capabilities instead.
- **Crafting Interpreters** by Bob Nystrom — recommended companion reading.
- **Types and Programming Languages** by Benjamin Pierce — type system reference.
- **"Generalizing Hindley-Milner Type Inference Algorithms"** — algorithm reference.
- **Maranget, "Warnings for pattern matching"** — exhaustiveness algorithm.

---

**End of document.**

_This is a living document. Amend it as the language evolves, treating every change as a deliberate design decision._
