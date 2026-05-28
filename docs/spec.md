# Ridge — Language Specification & Development Roadmap

**Version:** 0.2.6
**Author:** The Ridge Language Authors
**Last updated:** 2026-05-28

**History:** Supersedes `RILL_SPEC_AND_ROADMAP.md` (v0.1.0-draft, Rill). The language was renamed from *Rill* to *Ridge* after a design refinement pass. The underlying philosophy is preserved; the following are the substantive changes from the prior draft:
- Language name: **Ridge** (was *Rill*). File extension: **`.ridge`** (was `.rill`). Manifest: **`ridge.toml`** (was `rill.toml`).
- Effect system: **9 granular capabilities** with prefix list syntax (was binary `fn`/`fn!`).
- Multi-target strategy: **BEAM-primary with WebAssembly and native (LLVM) as exploratory backends** behind a target-neutral IR (changed from the fixed multi-target schedule of earlier drafts; see §14).
- **Workspace model** with architectural enforcement by the compiler — new first-class feature.
- 0.1.0 scope: **LSP minimum + package manager minimum** included (previously deferred).

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Design Philosophy & Non-Negotiables](#2-design-philosophy--non-negotiables)
3. [Language Overview](#3-language-overview)
4. [Formal Syntax Reference](#4-formal-syntax-reference)
5. [Type System](#5-type-system)
6. [Capabilities System](#6-capabilities-system)
7. [Semantic Model](#7-semantic-model)
8. [Project & Workspace Model](#8-project--workspace-model)
9. [Standard Library Scope](#9-standard-library-scope)
10. [Compiler Architecture](#10-compiler-architecture)
11. [Development Roadmap](#11-development-roadmap)
12. [Milestones & Deliverables](#12-milestones--deliverables)
14. [Multi-Target Strategy](#14-multi-target-strategy)
16. [Open Questions](#16-open-questions)
17. [Appendices](#17-appendices)

---

## 1. Executive Summary

**Ridge** is a general-purpose programming language built around four pillars: **developer experience, safety from the root, first-class performance, and approachability**. It combines immutable data, actor-based concurrency, and a granular effect system visible in function signatures. Ridge compiles to Core Erlang for the BEAM runtime, which is the production target. The intermediate representation is held target-neutral; WebAssembly and native (LLVM) backends remain exploratory work kept feasible by the shared IR (see §14).

The target audience is software developers who want a language that scales from scripts to distributed systems without mode switching — fast to write, easy to reason about, hard to misuse.

**This document defines Ridge 0.1.0**, the first milestone release. It covers language design, compiler architecture, standard library scope, workspace model, and a phased development roadmap with clear milestones.

### Elevator pitch

> Ridge is the only language where your architecture and your effects live in the type system, not in your PR reviews.

### Key characteristics

- **Compiled** to Core Erlang for the BEAM. WebAssembly and native (LLVM) backends are exploratory, gated by a target-neutral IR
- **Statically typed** with full Hindley-Milner inference
- **Immutable by default**, mutable state confined to actors
- **Actor-first concurrency** — millions of lightweight processes on BEAM
- **9 capabilities** (`io`, `fs`, `net`, `time`, `random`, `env`, `proc`, `spawn`, `ffi`) visible in function signatures
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
| N8 | Capabilities are tracked in the type | Fixed set of 9; no user-defined capabilities |
| N9 | Pattern matching is exhaustive | Non-exhaustive match is a compile error, not a warning |
| N10 | Everything is an expression (mostly) | Only `let`, `var`, `const`, `import` are statements |
| N11 | Architecture is enforced by the compiler | `forbid` rules in workspace manifest produce compile errors |
| N12 | IR is target-neutral | No backend-specific leakage in the IR; the shared IR keeps alternative backends feasible without committing to a schedule |

### 2.3. Deliberate trade-offs

- **We lose** fine-grained memory control (no manual allocation, no ownership tracking). Ridge is not for tight-loop numerical code or embedded systems. A native backend would narrow this gap and remains on the exploratory roadmap (§14).
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

A `Unit`-returning `main` is also valid (`fn io main () = Io.println "Hello, World"`), but you lose `?` propagation; see D059.

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

A `fn` declaration inside another function body may declare its own capability prefix. The inner function's capability set must be a subset of the enclosing function's declared set (D058).

```ridge
fn io fs main () -> Result Unit Error =
    fn io log (msg: Text) -> Unit = Io.println msg    -- OK: {io} ⊆ {io, fs}
    log "starting"
    Ok ()
```

Top-level `fn` declarations follow D037 (parameters are `Ident` or `(Ident: Type)` only). Inner `fn` declarations follow the same rule for their parameters but may freely declare capability prefixes up to the enclosing set.

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

The constructor name is **always required** in patterns and construction (D051): write `User { name = n }`, never `{ name = n }`. Shorthand `{ name }` binds to a local variable named `name`, equivalent to `{ name = name }` (D053). Mixed form: `User { name, email = e, age }`.

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

-- `as` patterns (bind the whole and the parts)
match user
    admin @ User { role = Admin } -> handleAdmin admin
    other                         -> handleOther other

-- Shorthand field binding in patterns (D053): `{ name }` ≡ `{ name = name }`
match user
    User { name, age } -> $"${name} is ${age}"

-- Destructuring in let — full patterns including tuples and records (D052)
let (x, y) = point
let (User { name }, count) = pair           -- tuple with nested record pattern
fn distance (x1, y1) (x2, y2) = Float.sqrt ((x2-x1)^2 + (y2-y1)^2)
```

**Pattern scope rules (D052):** `let` bindings and lambda parameters accept full patterns (tuples, records with shorthand, constructor patterns, wildcards, as-patterns). Top-level `fn` declarations are restricted to `Ident` or `(Ident: Type)` per D037 — destructure inside the body via `let` or `match`.

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

`try { ... }` is a **value-producing expression** (D060): it yields `Result`/`Option`. An unused non-`Unit` result produces a compiler warning (`discarded_result`). To explicitly discard, use `Result.discard : Result a e -> Unit` or `Option.discard : Option a -> Unit` from the stdlib, or use `match`. Ridge has no monadic do-notation — `try` + `?` is the idiomatic chaining mechanism.

### 3.8. Capabilities (effects)

Ridge has **9 capabilities** visible in every function signature. They form a closed set — users cannot define new ones.

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

When actor state cannot be given a compile-time default, an `init` block initialises it at spawn time (D061).

- An actor has at most one `init` block.
- Syntax: `init [capList] (params) = body`
- If `init` is present, `spawn ActorName arg1 arg2` passes arguments positionally to `init`.
- If `init` is absent, all `state` fields must have defaults (preserves current behaviour).
- Inside `init`, assign state fields with `<-`. Other expressions are allowed.
- Callers of `spawn` do **not** inherit `init`'s capabilities (consistent with D018 handler encapsulation). Only the `spawn` capability is required in the caller.

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

### 3.10. String interpolation

```ridge
Io.println $"User ${user.name} has ${user.age} years"
Io.println $"Total: ${items |> List.map (.price) |> List.sum}"
```

In 0.1.0, interpolation accepts a **closed set of built-in types**: `Int`, `Float`, `Bool`, `Text`, `Timestamp`. User-defined types must be converted explicitly (e.g., `$"user=${User.toText u}"`). In 0.2.0 this becomes an open `ToText` typeclass. See D038.

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
actor    as       catch    class     const     deriving  else
false    fn       guard    if        import    in        init
instance let      match    on        pub       return    spawn
state    then     true     try       type      var       when
where    with
```

#### Capability keywords (soft-reserved, contextual)

These are keywords only after `fn` or `on`; elsewhere they are ordinary identifiers.

```
io    fs    net    time    random    env    proc    spawn    ffi
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
=>   (reserved, not used in 0.1.0)
@    as-pattern binder
..   (reserved, not used in 0.1.0; see D050)
```

#### Identifiers

- Lowercase-starting: values, functions, type variables, capability keywords
- Uppercase-starting: types, constructors, modules
- Must match: `[a-zA-Z][a-zA-Z0-9_]*`
- Underscore prefix `_` marks private/unused
- 0.1.0 is ASCII-only per D049; source files are UTF-8 (string literals and comments may contain any Unicode). Unicode identifiers reconsidered in 0.3.0+.

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

String escapes in 0.1.0 (D047): `\n`, `\t`, `\"`, `\\`, `\r`, `\0`, `\u{HHHH}`. Multi-line and raw string literals are deferred to 0.2.0.

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

- A block is introduced by `=`, `->`, `then`, or `else`. (`do` was removed in D056.)
- The block's contents must be indented strictly deeper than the opening line.
- Within a block, all items must be at the same indentation level.
- Tabs are forbidden. Only spaces. (Enforced by lexer; error on tab.)
- Indentation unit convention: 4 spaces (not enforced, but the formatter uses it).
- **Layout is partially suppressed inside brackets.** While the bracket-nesting depth (count of open `(`, `[`, `{` not yet matched) is greater than zero, `INDENT` and `DEDENT` tokens are never emitted. However, a `NEWLINE` token _is_ emitted when a logical line begins at column ≤ the baseline column of the first continuation line inside the bracket — this marks a statement boundary inside parenthesised lambda bodies and similar constructs. When depth returns to zero, full layout (including `INDENT`/`DEDENT`) resumes. (D062.)
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
Capability    = "io" | "fs" | "net" | "time" | "random" | "env" | "proc" | "spawn" | "ffi" .
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
              | "(" PatternList ")" | "[" PatternList "]"
              | RecordPattern | Ident "@" Pattern | Ident "::" Pattern .

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
| 10 | `-` (unary negate) | n/a | no prefix `!`; negation is `Bool.not` (D044) |
| 11 | `!` `?>` (send / ask) | left | actor message operators |
| 12 | function application | left | |
| 13 | `?` (postfix propagate), `.` (field access) | left | call-suffix band |

---

## 5. Type System

### 5.1. Foundations

Ridge's type system is based on **Hindley-Milner with extensions**:

- Full type inference (no annotation required anywhere in 0.1.0).
- Algebraic data types (sum and product).
- Parametric polymorphism with let-generalization.
- **Capability inference** alongside type inference (see §5.3).

**Not in 0.1.0 but syntactically reserved:**
- Type classes / traits with constraints (`where t is Comparable`).
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
Timestamp  -- opaque; no literal syntax (D048); see §9.2 std.time for construction
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
3. Typeclasses (open polymorphism, post-0.1.0).

---

## 6. Capabilities System

### 6.1. The 9 capabilities

Ridge has a **closed set** of 9 capabilities. Users cannot define new ones. This is deliberately less expressive than Koka/Eff but radically simpler to teach and to debug.

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

Inner `fn` declarations inside a function body may also declare a capability prefix; the inner function's capability set must be a subset of the enclosing function's declared set (D058; see §3.3 for example).

### 6.3. Propagation rules

1. **Pure functions may only call pure functions.** Calling `Io.print` from `fn f` is a compile error.
2. **`fn X f` may call `fn g` (pure) and `fn Y h` where `Y ⊆ X`.** A caller must have at least the capabilities of the callee.
3. **Inference + verification.** The compiler infers the capability set of a body; if a signature is declared, the body's set must be a subset. If not, the error suggests either adding the missing capability or removing the offending call.
4. **Transitive subset rule for inner functions (D058).** If an inner `fn` declaration inside a function body declares a capability prefix, that inner function's capability set must be a subset of the enclosing function's declared (or inferred) capability set. This rule applies transitively through nested inner functions.

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

### 6.6. 0.1.0 semantics: static flags, manual DI

In 0.1.0, capabilities are **compile-time tags only**. There are no replaceable handlers at runtime. Testing is done via **dependency injection**: pass functions as arguments.

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

Replaceable capability handlers (à la Roc platforms) are evaluated for 0.2+ if demand arises.

### 6.7. Capability polymorphism in higher-order functions

Higher-order stdlib functions like `List.forEach`, `List.map`, `Result.andThen` must not force a single capability on their callback. Ridge 0.1.0 solves this with a **capability variable** in the signature — the caps of the callback flow through to the caller at each call site. This is not a typeclass; it's a single effect variable in the type system.

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

See D041.

---

## 7. Semantic Model

### 7.1. Evaluation order

- **Strict evaluation** (not lazy). Arguments are evaluated before the function is called.
- **Left-to-right** evaluation of function arguments.
- **Pipe** `a |> f` is exactly equivalent to `f a` — same evaluation semantics.

### 7.2. Actor semantics

Each actor is a lightweight process (BEAM process in 0.1–0.3; green thread with M:N scheduler in native 0.4+).

- **`actor ! msg`** (send): asynchronous, returns immediately, returns `Unit`. No capability required beyond having the handle.
- **`actor ?> msg`** (ask): synchronous from caller's perspective, blocks the calling process until reply. Requires `time` in the caller (for the timeout), nothing else.
- Each actor processes one message at a time, FIFO.
- Actor state is private; no direct access from outside.
- Message send is one-way; ask is implemented as send + await reply with a reference.

### 7.3. Memory model

- All values are immutable except actor `state`.
- Sharing structurally-equal data is a compiler optimization (persistent data structures).
- On BEAM: garbage collection is per-process (no global GC pauses); process memory is isolated; messages are copied between processes.
- On native (0.4+): concurrent GC (Go-style) with per-actor heaps where possible.

### 7.4. Error handling model

- **Recoverable errors**: `Result a e` — handled explicitly.
- **Programming errors**: runtime crashes (index out of bounds, match failure at runtime, etc.) — the actor dies. Supervisors (post-0.1.0) can restart.
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
dep6 = { hex = "1.2.3" }                          # from hex.pm (0.2.0+)
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
ridge new <name>          # scaffold a new project
ridge init                # initialize a workspace in the current directory
ridge repl                # interactive REPL
```

**Test discovery (0.1.0).** `ridge test` discovers every `pub fn test_<name> ()` (zero-arity) across the workspace. The return type must be `Result Unit Text` (canonical) or `Bool` (deprecated, accepted with a per-test deprecation warning, **removed in 0.2.0**). Tests run sequentially in a fresh BEAM child process per test (no shared state leaks). FFI-bearing tests are rejected with a compile-time capability error. **0.2.0 evolution (D168):** the canonical form becomes `@test "<free-form name>"` as an attribute on `pub fn`, additive on top of the prefix during a one-minor-version migration window; prefix removed in 0.3.0. The keyword-block form `test "name" { body }` is **explicitly rejected** for losing first-class function semantics and forcing grammar churn on every test modifier.

---

## 9. Standard Library Scope (0.1.0)

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

*Note (D126):* `length` is **reserved** in 0.1.0 for 0.2.0 codepoint-aware semantics. `byteSize` returns the byte count under UTF-8 encoding; for character/grapheme counting, wait for `length` in 0.2.0.

**Convention:** in every stdlib function, the "main data" argument comes **last**, so pipes compose naturally:

```ridge
users |> List.map (.email) |> List.filter isValid |> List.take 10
```

**Logical negation** is `Bool.not : Bool -> Bool` (D044). There is no prefix `!` for negation; `!` is exclusively the actor-send operator.

`Result.discard : Result a e -> Unit` and `Option.discard : Option a -> Unit` (D060) are the explicit way to silence the `discarded_result` compiler warning when a `try` or `?` expression result is intentionally unused.

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

### 9.3. Advanced (0.1.0 scope)

| Module | Purpose | Priority |
|--------|---------|----------|
| `std.json` | JSON encode/decode | High |
| `std.http` | HTTP client + basic server | Medium (defer to 0.2 if time-constrained) |
| `std.regex` | Regular expressions | Low |
| `std.terminal` | Terminal control | Low |

**Decision rule:** enough to write the three canonical example programs (log analyzer, URL shortener, Game of Life) plus the rate limiter in [Appendix A](#appendix-a--canonical-example-programs). Nothing more, nothing less.

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
│   ├── ridge-lsp/              # Language server (0.1.0 minimum, 0.2.0 full)
│   └── ridge-pkg/              # Package manager
├── tests/
├── examples/
└── docs/
    ├── grammar.ebnf
    ├── spec.md
    └── tutorial.md
```

*Per D118 (stdlib path closure): the Ridge `.ridge` sources live at `crates/ridge-stdlib/stdlib/<module>.ridge` (with `net/http.ridge` as the single nested module — see §11.3 Phase 7).*

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

## 11. Development Roadmap

### 11.1. Philosophy

- **Ship vertical slices.** Each milestone produces something runnable.
- **Tests from day one.** Snapshot tests for the parser, golden tests for codegen, integration tests for full pipeline.
- **No premature optimization.** Correctness first, performance when measured.
- **No scope creep.** Every feature not on this roadmap is explicitly deferred.

### 11.2. Prerequisites

Before starting Phase 0:

- [ ] Rust 1.75+ installed
- [ ] Erlang/OTP 26+ installed (`erl`, `erlc` on PATH)
- [ ] Git repo initialized
- [ ] This document committed as `docs/spec.md`
- [ ] `github.com/ridge-lang` org reserved
- [ ] Domain `ridge-lang.org` (or alternative) reserved
- [ ] CI pipeline planned (GitHub Actions or Azure DevOps)

### 11.3. Phase breakdown

Each phase lists: goal, tasks, deliverable, tests, and estimated effort.

---

### **Phase 0 — Foundations** _(≈ 1.5 weeks full-time)_

**Goal:** Lock design decisions, set up infrastructure.

**Tasks:**
1. Write formal EBNF grammar → `docs/grammar.ebnf`.
2. Initialize Cargo workspace with all crates (empty).
3. Set up CI: build + test on push.
4. Set up `rustfmt`, `clippy` strict configs.
5. Write contribution guide.
6. Create `examples/` directory with the four target programs (log analyzer, URL shortener, Game of Life, rate limiter).

**Deliverable:** Empty compiler that builds green, grammar doc, examples written.

**Definition of done:**
- `cargo build --all` succeeds
- `cargo test --all` succeeds
- `docs/grammar.ebnf` committed
- `examples/*.ridge` committed (as parsing targets, not compiled yet)

---

### **Phase 1 — Lexer** _(≈ 1 week)_

**Goal:** Turn source text into a token stream, with correct handling of layout.

**Tasks:**
1. Define `Token` enum in `ridge-lexer/src/token.rs`.
2. Implement basic tokenization with `logos` (keywords, literals, punctuation).
3. Implement **layout algorithm**: convert indentation into `INDENT` / `DEDENT` / `NEWLINE` tokens.
4. Handle string interpolation lexically (tokenize `$"..."` with nested expression segments).
5. Handle doc comments `---...---`.
6. Span tracking on every token for diagnostics.

**Definition of done:**
- All four example programs tokenize without error
- Snapshot tests locked in
- Bad inputs produce helpful errors

---

### **Phase 2 — Parser** _(≈ 1.5 weeks)_

**Goal:** Token stream → AST.

**Tasks:**
1. Define AST types in `ridge-ast/`.
2. Implement parser with `chumsky`, including capability prefix lists.
3. Handle all syntactic constructs: types, functions, actors, patterns, expressions, workspaces-as-types (import parsing).
4. Error recovery: produce partial AST + diagnostics.
5. Integrate `ariadne` for rendering parse errors.

**Definition of done:**
- All four examples parse successfully
- 60+ snapshot tests
- Every syntactic construct has at least one positive and one negative test

---

### **Phase 3 — Name Resolution, Imports, Workspace Rules** _(≈ 1.5 weeks)_

**Goal:** Resolve identifiers, build the module graph, enforce workspace architectural rules.

**Tasks:**
1. Implement workspace manifest parsing (`ridge.toml` root + per-project).
2. Implement module discovery (file path → module name, using project name prefix).
3. Build symbol tables per module.
4. Resolve imports; detect cycles.
5. Enforce `[workspace.rules] forbid` — produce compile errors on forbidden arcs.
6. Enforce visibility (`pub`, `pub(internal)`, `_`, project.exports).
7. "Did you mean X?" suggestions (Levenshtein distance for typos).

**Definition of done:**
- Multi-module, multi-project example compiles through resolution
- Forbid-rule violations produce the structured error from §8.6
- Visibility violations produce clear errors

---

### **Phase 4 — Types & Capabilities** _(≈ 4.5 weeks — the hardest phase)_

**Goal:** Infer and check types and capabilities across the program.

**Tasks:**
1. Implement type representation (monotypes, polytypes, type variables).
2. Implement Algorithm W with union-find.
3. Handle let-generalization correctly.
4. Type-check all expression forms.
5. **Capability inference:** compute each function's capability set from its body.
6. **Capability check:** verify declared set ⊇ inferred set; verify callees ⊆ caller; enforce project-level capability allow/deny.
7. **Actor capability encapsulation** (Model B): asks inherit only `time`, not handler capabilities.
8. **Pattern matching exhaustiveness** using Maranget's algorithm.
9. High-quality type and capability error messages.
10. **Actor handler-name validation** (Phase 3 deferral): every `Send.message` head and `Ask.message` Ident must match a declared `on`-handler on the target actor's type. Phase 3 silently passes these through (`crates/ridge-resolve/src/walker.rs::visit_send_message`, plus the existing `visit_ident` no-op for `Ask`); Phase 4 owns the cross-validation against the actor's `SymbolKind::Actor { handlers }` list. Emit a new `T-error` (e.g. `T0NN UnknownActorHandler`) with "did you mean?" suggestions over the actor's handler names. Cross-check arg arity and types against the handler signature. _Source of deferral:_ Phase 3 has no actor-handler scope to resolve against during the walker pass; the walker's job is name-resolution only.
11. **Qualified record construction** `Module.Type { field = val, ... }` (Phase 3 deferral): currently the parser builds `Expr::Record { constructor: Ident, fields }` and `constructor` is a bare `Ident`, so `Http.Response { ... }` does NOT parse as record construction (it parses as `QualifiedName` followed by something else). To enable this Elm-style ergonomic, change `Expr::Record::constructor` from `Ident` to `QualifiedName` (or add a new `Expr::QualifiedRecord`); update the parser's record-construction recogniser to accept a leading `Module.UPPER`; extend `crates/ridge-resolve/src/qualified.rs` to resolve qualified record constructors; update `walker.rs` and the visitor. D072 (import lists with `UPPER_IDENT`) covers the unqualified-import case so this is purely an ergonomic alternative for users who prefer fully-qualified type references — _not_ blocking for any 0.1.0 example. _Estimated effort:_ ~1 day across AST + parser + resolver + tests.

**Definition of done:**
- All examples type-check with correct capabilities
- 120+ tests (half positive, half negative)
- Error messages reviewed for quality
- Capability-leak test suite (design of workspace + actor encapsulation)
- Handler-name validation for `Send` / `Ask` against actor's `on` list (task 10 above)
- Qualified record construction `Module.Type { ... }` parses + resolves (task 11 above)

---

### **Phase 5 — Lowering to Ridge Core IR** _(≈ 1 week)_

**Goal:** Simplify typed AST into a minimal intermediate representation.

**Tasks:**
1. Define Ridge Core IR (small, explicit, target-neutral).
2. Desugar:
   - `|>` → function application
   - `?` → pattern match
   - `try` blocks → chained `?`
   - `guard` → `match`
   - String interpolation → concatenation with `ToText`
   - `with` updates → record construction
   - Actor handlers → dispatch tables
3. Lower all constructs.

**Definition of done:** Snapshot tests on IR output for each example. IR contains no backend-specific assumptions.

---

### **Phase 6 — Codegen to Core Erlang** _(≈ 1.5 weeks)_

**Goal:** Ridge Core IR → Core Erlang source files.

**Tasks:**
1. Map Ridge types to Erlang representations:
   - `Int`, `Float`, `Bool` → native
   - `Text` → binary
   - Records → maps (`#{ name := "...", ... }`)
   - Union types → tagged tuples
   - `Option` → `{some, X}` / `none`
   - `List` → Erlang lists
   - `Map`, `Set` → Erlang maps / `gb_sets`
2. Map actors to gen_server processes.
3. Map `!` (send) and `?>` (ask) to Erlang message operations.
4. Emit `.core` files.
5. Invoke `erlc` to produce `.beam`.

**Definition of done:** All four examples compile, link, and run correctly on BEAM.

---

### **Phase 7 — Standard Library** _(≈ 1 week)_

**Goal:** Write the stdlib in Ridge, using the compiler to bootstrap it.

**Tasks:**
1. `std.int`, `std.float`, `std.bool`.
2. `std.text` using Erlang's `binary` module.
3. `std.list`, `std.map`, `std.set`.
4. `std.option`, `std.result`.
5. IO modules: `std.io`, `std.fs`, `std.time`, `std.random`, `std.env`, `std.cli`, `std.proc`.
6. `std.json` (MVP: encode/decode).
7. `std.net.http` (minimal client + server).

**Definition of done:**
- All examples run using only stdlib + user code
- Every public stdlib function has at least one test

---

### **Phase 8 — CLI, LSP minimum, Package manager minimum** _(≈ 2 weeks)_

**Goal:** Developer experience floor for 0.1.0.

**Tasks:**
1. CLI subcommands: `ridge {build|run|check|fmt|new|init|test|repl}`.
2. `ridge fmt` basic formatter (opinionated, no options).
3. **LSP minimum** (ridge-lsp crate):
   - Diagnostics on save (runs the compiler in watch mode, streams errors as LSP JSON-RPC).
   - No goto-definition, no hover, no refactor (those are 0.2.0).
4. **Package manager minimum** (ridge-pkg crate):
   - `dependencies = { path = "../foo" }`
   - `dependencies = { git = "github.com/x/y", tag = "v1.0" }`
   - No registry, no semver resolution, no lockfile.
5. Installation script / prebuilt binaries.

**Definition of done:**
- User can install Ridge and run hello-world in < 5 minutes
- VS Code shows errors from Ridge files as they happen
- A project with a git dependency builds successfully

---

### **Phase 9 — Release** _(≈ 0.5 weeks)_

**Goal:** Public 0.1.0 release.

**Tasks:**
1. Build binaries for Linux x86_64, macOS x86_64, macOS arm64, Windows x86_64.
2. SHA256 checksums, GitHub Releases.
3. README with examples and install instructions.
4. Basic tutorial in `docs/tutorial.md`.
5. TextMate grammar for syntax highlighting on GitHub.
6. Announcement on HN, r/ProgrammingLanguages, Lobsters.

**Deliverable:** **Ridge 0.1.0 publicly released.**

---

### 11.4. Total effort estimate

Full-time:

| Phase | Effort |
|-------|--------|
| 0. Foundations | 1.5 weeks |
| 1. Lexer | 1.0 week |
| 2. Parser | 1.5 weeks |
| 3. Resolve + Workspaces | 1.5 weeks |
| 4. Types + Capabilities | 4.5 weeks |
| 5. Lowering | 1.0 week |
| 6. Codegen Erl | 1.5 weeks |
| 7. Stdlib | 1.0 week |
| 8. CLI + LSP min + Pkg min | 2.0 weeks |
| 9. Release | 0.5 weeks |
| **Total 0.1.0** | **≈ 16 weeks full-time** |

At 15 h/week part-time: **≈ 11 months**. At 10 h/week part-time: **≈ 16 months**.

---

## 12. Milestones & Deliverables

Six public checkpoints. Each should be a tagged release so progress is visible.

| Milestone | Covers Phases | Headline demo |
|-----------|---------------|---------------|
| **Parses** | 0, 1, 2 | `ridge parse examples/log_analyzer.ridge` prints a pretty AST |
| **Resolves, Types, Capabilities** | 3, 4 | `ridge check examples/*.ridge` reports OK or typed errors with capability diagnostics |
| **Runs on BEAM** | 5, 6 | `ridge run examples/log_analyzer.ridge` compiles and executes |
| **Complete** | 7 | All four examples run end-to-end using stdlib |
| **Tooled** | 8 | VS Code diagnostics + git-based packages work |
| **Released** | 9 | Public 0.1.0 binaries available |

---

## 14. Multi-Target Strategy

Ridge is **BEAM-primary**. BEAM is the production target; the language, the standard library, the actor model, and the tooling are all designed against it and validated there. Alternative backends (WebAssembly and native via LLVM) remain on the roadmap as **exploratory work**, contingent on user traction rather than a fixed schedule. The intermediate representation is held target-neutral as a design discipline so the option to activate a second backend stays open without dictating when.

This section describes the present target, the exploratory ones, the disciplines that keep them feasible, and the cadence on which they are re-evaluated.

### 14.1. BEAM (production)

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

### 14.2. Exploratory backends

Two alternative backends remain in the roadmap as exploratory work. Neither has a fixed schedule; both are gated on user traction signals — concrete deployment requirements that BEAM cannot serve, expressed as reports against the public tracker.

#### 14.2.1. WebAssembly

WebAssembly would unlock browser playgrounds, edge functions, and embeddable Ridge runtimes. The work splits in two natural phases:

- **WASM limited.** Pure code and deterministic capabilities (`time`, `random` via host-provided shims), single-threaded, no actors, no async I/O. Target use cases: in-browser playground and stateless edge functions.
- **WASM complete.** Actors via the WASM threads proposal, WASI for `fs`/`net`/`proc`, WASM GC where available. Target use cases: production edge computing.

Both phases remain exploratory until user traction justifies the investment. The `ridge-codegen-wasm` crate exists today as a stub guarded by the target-neutrality test (`crates/ridge-lower/tests/neutrality.rs`), preserving the option without committing to a schedule.

Candidate deployment targets, when and if the work activates: Cloudflare Workers, Fastly Compute@Edge, Fermyon Spin, wasmtime, wasmer, browsers.

#### 14.2.2. Native via LLVM

A native backend would unlock compute-bound workloads, fast CLI startup (<10 ms vs. 50–100 ms on BEAM), standalone binaries without the Erlang runtime, and embedded or constrained environments.

It requires a custom runtime — comparable in effort to the existing BEAM backend, effectively a second compiler:

- M:N actor scheduler (Go-style or BEAM-style; decision deferred until activation).
- Garbage collection — per-actor heaps where possible, global concurrent GC as the baseline. Reference-counting and ownership are out by design (the first contradicts persistent immutable cycles; the second contradicts Ridge's promise that the programmer doesn't think about memory).
- Explicit data-layout decisions for each type (List, Map, Union, Text, Record).
- FFI and system integration via C ABI.
- Concurrency primitives — MPMC channels, scheduler synchronization, async I/O.
- Debugging and observability — DWARF, profiler integration.

The `ridge-codegen-llvm` crate exists today as a stub guarded by the same target-neutrality test.

### 14.3. The target-neutrality discipline

The IR is held free of backend-specific assumptions. This is not a hint or a code-review check; it is enforced:

- `crates/ridge-lower/tests/neutrality.rs` asserts the IR carries no backend-specific leakage.
- The stub `ridge-codegen-wasm` and `ridge-codegen-llvm` crates compile against every PR, so any change that breaks target-neutrality fails CI.

The cost of the discipline is small — roughly a 5% tax on lowering and IR design work. It buys the option to activate a second backend later without redoing the frontend.

### 14.4. Re-evaluation

Whether the exploratory backends are ever activated, and on what schedule, is re-evaluated **18 months after the 0.3.0 GA tag**, against three signals:

- **User traction.** Concrete deployment requirements BEAM cannot serve, expressed in public reports against the project tracker.
- **Capacity.** The project is solo-maintained; activating a second backend is a multi-quarter commitment that competes with BEAM-side work.
- **Ecosystem state.** Where the WebAssembly and native ecosystems stand at the time — component model, WASI preview, WASM GC standardization, LLVM IR stability.

A mid-cycle checkpoint at 9 months post-0.3.0 GA is reserved for early signal. If user reports concentrate on a deployment shape that BEAM cannot reach (cold-start-sensitive edge functions, in-browser tooling, standalone CLI distribution), the timeline for the relevant backend can advance.

### 14.5. Strategic principles

1. **BEAM is the production target.** The language and tooling ship against BEAM today; alternative backends do not gate any 0.x release.
2. **The IR is the contract.** Backends consume the IR; they need nothing else from the frontend. The IR stays target-neutral.
3. **Capabilities are target-agnostic.** They are compile-time checks; runtime cost is zero on any target.
4. **Public messaging.** Ridge is a typed functional language for the BEAM. WebAssembly and native (LLVM) backends are exploratory, kept feasible by the shared IR but not committed to a schedule.

---

## 16. Open Questions

All 0.1.0-blocking design questions have been resolved. This section tracks their resolution and lists work deferred to 0.2.0.

### 16.1. Resolved

| ID | Question | Resolution | Decision |
|----|----------|-----------|----------|
| Q-001 | Integer semantics | Fixed 64-bit signed across all targets | D029 |
| Q-002 | Float NaN equality | `NaN == NaN` is false (IEEE 754) | D030 |
| Q-003 | Actor mailbox size | Unbounded in 0.1.0 | D031 |
| Q-004 | Integer overflow | Crash; explicit `wrappingAdd` / `saturatingAdd` for alternatives | D032 |
| Q-005 | Formatter policy | Opinionated, zero config | D033 |
| Q-006 | Test framework | Built into CLI (`ridge test`) | D034 |
| Q-007 | Capability set size | 9 fixed in 0.1.0 | D035 |
| Q-008 | `let ... in` vs block | Block-structured only | D036 |
| Q-009 | Typed lambda parameters | Lambdas use same `Param` grammar as `FnDecl` | D037 |
| Q-010 | String interpolation coercion | Closed set in 0.1.0; `ToText` typeclass in 0.2.0 | D038 |
| Q-011 | `?` operator scope | Inline in `Result`/`Option` contexts; `try` block for narrower scope | D039 |
| Q-012 | Capability inference on private fns | Declared on public, inferred on file-private | D040 |
| Q-013 | Capability polymorphism in HOFs | Capability variable in stdlib signatures | D041 |
| Q-014 | Mailbox observability | Deferred | D042 |
| Q-015 | DI pattern in tests | Convention in 0.1.0; library in 0.2.0 | D043 |
| Q-016 | `Text` Unicode normalization | No lexer-side normalization; `Text` stores raw UTF-8; normalization exposed via `std.text.normalize` | D063 |
| Q-017 | Multi-line and raw string literal syntax | Deferred to 0.2.0; single-line with standard escapes in 0.1.0 | D047 |
| Q-019 | Numeric literal syntax (digit separators, base prefixes) | Supported in 0.1.0: `0b`, `0o`, `0x`, `_` separator | D046 |
| Q-020 | Doc-comment attachment semantics (parser) | Attach to next top-level `Item` (or `Module::doc` at file head); orphan `DOC_COMMENT` → warning `P019` | D067 |
| Q-022 | `guard … else <block>` — single-expression else vs multi-statement block | Multi-statement indented `Block` permitted (final statement must diverge via `return`) | D066 |

### 16.2. Deferred to 0.2.0

These are locked for 0.1.0 but will be revisited:

- **Bounded mailboxes and backpressure** (Q-003)
- **Open `ToText` typeclass** for interpolation of user-defined types (Q-010)
- **Full capability polymorphism** beyond stdlib HOFs, if demand arises (Q-013)
- **Mailbox observability API** (`Actor.mailboxSize`, peek, drain) (Q-014)
- **`ridge.test` DI helpers** for capability stubbing (Q-015)
- **Capability set review** based on 0.1.0 usage data (Q-007)
- **Multi-line and raw string literals** (Q-017 / D047)
- **Range and rest-pattern syntax for `..`** (D050) — concrete semantics chosen in 0.2.0
- **Unicode identifiers** (D049) — reconsidered in 0.3.0+
- **`@test "<free-form name>"` attribute as canonical test-discovery form** (D168 / OQ-C018) — 0.1.0 uses `pub fn test_*` name-prefix discovery; 0.2.0 adds `@test "<name>"` as additive sugar (both accepted; prefix emits `C304 PrefixTestDeprecated` per-test warning); 0.3.0 removes prefix. `ridge fmt --migrate-tests` one-shot migration ships in 0.2.0. The keyword-block form `test "name" { body }` is **explicitly rejected** (loses first-class function semantics, multiplies grammar churn for `@ignore` / `@only` / `@slow` composition).

### 16.3. New questions

New open questions should be appended here as they arise during implementation. Track them as GitHub issues once the repo exists.

**Q-018**: `Map` / `Set` ordering and backing representation — ordered (tree-based, deterministic iteration) or hash (faster, non-deterministic)? Affects stdlib API, serialization determinism, and test reproducibility. _Pending — decide during Phase 4 when stdlib types are lowered._

**Q-021**: `return expr` semantics in Result/Option-returning functions — does `return expr` return `expr` verbatim (requiring it to already be `Result`/`Option`-typed, matching the function's return type), or does it auto-wrap into `Ok expr` / `Some expr` when the function return type is `Result`/`Option`? The spec §3.12 example `return Err (Inactive user.id)` is consistent with the verbatim reading. _Pending — decide during Phase 4 type-checker when `return` is lowered; verbatim reading is the tentative default._

**Q-023**: Benchmark methodology and "first-class performance" measurement strategy — §1 lists performance as a core pillar but the roadmap §11 has no benchmark phase, no choice of bench framework (Criterion-style? `cargo bench`? Ridge-side `ridge bench`?), no golden-number commitments per target (BEAM / WASM / native), and no regression policy. Without a measurement layer, the "first-class performance" pillar is unverified — a stated value with no enforcement. _Pending — decide before Phase 6 (codegen-erl) so 0.1.0 ships with at least a baseline `ridge bench` subcommand and a small benchmark corpus; tentative default = Criterion-style harness inside the compiler crates plus a Ridge-side `bench` block lowered to per-target timing scaffolds, with a starter corpus exercising actor send/receive throughput, list/map ops at 10k/100k elements, and the four Appendix-A example programs end-to-end._

**Q-024**: OWASP web-layer policy and `std.net.http` hardening defaults — language-level safety is strong (capabilities, no null, no exceptions, no reflection, forbid arcs), but framework-level web concerns (XSS escaping in templating, CSRF tokens, parameterized SQL via a future `std.db`, authn/authz primitives, rate-limiting helpers, secure cookie defaults, CSP / HSTS headers in `std.net.http.respond`) have no decision. `std.net.http` 0.1.0 exposes only `get` / `post` / `put` / `delete` / `listen` / `respond` (§9.2); building a real web service requires the user to roll their own security layer, which contradicts the "make the right thing easy" principle (§2.1). _Pending — decide before 0.2.0 stdlib expansion; tentative default = bake the OWASP-Top-10 mitigations Ridge can express via types into stdlib (e.g. `Sql` newtype that requires parameter binding to construct, `Html` newtype that escapes on construction, `SecureCookie` defaults with `Secure` + `HttpOnly` + `SameSite=Lax`, default CSP / HSTS headers on `respond`); defer the rest (full authn/authz primitives, distributed rate-limiting library) to a `std.web` ecosystem package layered on top._

**Q-025**: Large-program acceptance corpus — the four canonical examples (Appendix A) are 50–150 LOC each and exercise individual features in isolation; they do not stress cross-module type inference, deep actor topologies, large `match` trees, long workspace dependency chains, or programs >500 LOC. Without large-program tests, scaling bugs in the type checker, lowering pass, or codegen surface only when a real user hits them — by which point the cost of fixing them is much higher. _Pending — decide during the Phase 7 (stdlib) acceptance gate; tentative default = add 5–10 medium programs (~300–800 LOC each) covering an actor-heavy chat server, a multi-module domain workspace exercising `forbid` rules end-to-end, a streaming log-processing pipeline with backpressure simulation, and one ported toy-compiler exercise; each runs as an end-to-end snapshot test on every PR with timing + binary-size budgets._

**Q-026**: Anonymous record literal syntax — Ridge has anonymous tuples `(Int, Text)` and nominal records (declared via `type` + `Constructor { field = ... }`), but no anonymous record literal `{ name = "x", age = 1 }` whose type is inferred structurally. Workflows that benefit (ad-hoc JSON shapes, intermediate computation states, returning multi-field results without naming the type) currently force a tuple (positional, unreadable past 3 fields) or a one-off type alias (verbose, pollutes the namespace). This is a real DX gap relative to OCaml objects, F# anonymous records, and TypeScript object literals. _Pending — decide during Phase 4 type-checker; tentative default = add `{ field = expr, ... }` literal with structural row-typed inference gated to **expression positions only** (no anonymous record types in function signatures — those still require a `type` alias so error messages point at named types). Trade-off: introduces structural types into an otherwise nominal system, brings row-polymorphism complexity to inference, may interact non-trivially with `with` updates (§4.5)._

**Q-027**: Native GUI / desktop toolkit roadmap — Ridge 0.1.0–0.4.0 targets BEAM, WASM, and LLVM-native, but the roadmap §11 contains no toolkit for desktop UI (analogous to WPF / Tkinter / Qt / SwiftUI). With WASM (0.3.0) a Ridge program can target the browser via a userland framework, but native windowed applications are unaddressed, and "general-purpose" (§1) is therefore narrower than the marketing implies. _Pending — decide during 0.5.0 planning, or sooner if a concrete user demand arises; tentative default = **stay out of the toolkit business** (UI is userland or a dedicated `ridge.ui` ecosystem package layered on `std.ffi` to bind to GTK / Qt / Cocoa); revisit only if a Ridge-native immediate-mode toolkit becomes a strategic priority. Rationale: building a UI toolkit is a multi-year commitment that would dwarf the rest of the language scope; the same effort spent on stdlib + tooling produces more compounding value._

#### Resolve design questions (OQ-R001..OQ-R016)

Raised during resolve design. Each has a provisional answer that the implementation uses by default.

| ID | Question | Impact | Provisional answer |
|----|----------|--------|--------------------|
| OQ-R001 ✅ | Bare `import foo.bar` (no `as`, no item list) — what does it expose? | Import-resolution semantics; affects lookup order and collision errors. | **Qualified-namespace only** (Elm-style `import Foo` — use-sites must write `Foo.bar`). Flooding the importer's scope with every `pub` symbol requires an explicit item list (`import foo.bar (a, b)`); wildcard `(..)` is reserved, not in 0.1.0. Rationale: avoids cross-import identifier collisions, keeps resolve-phase lookup order unambiguous, and aligns with Elm (Ridge's primary UX reference) rather than Rust's discouraged `use foo::*;` idiom. **Resolved: OQ accepted as-is; the bare-import-ambiguous variant deleted.** |
| OQ-R002 ⚠ | Shadowing across scopes — allowed, warned, or forbidden? Spec is silent. | Scope-chain lookup policy; affects `R011` / `R017` severity. | **Cross-scope shadowing allowed silently; same-scope duplicate = `R011` hard error.** Matches Rust / F# / OCaml / Elm; the `_`-prefix convention is the opt-out for intentional shadowing without warning. |
| OQ-R003 | Case sensitivity of module names and imports. | Module-name derivation (§4.1 step 5); filesystem portability. | **Case-sensitive** per §3.11 examples (`Types/Id.ridge` → `acme.shared.Types.Id`, file-name case preserved). Windows filesystems preserve case; collisions emit `R002 DuplicateModule`. |
| OQ-R004 ✅ | `[project.exports].public` / `internal` default when missing — what if the section is absent? | Visibility mapping; whether `pub` leaks without an explicit `[project.exports]` table. | **Absent `public` = all `pub` symbols are externally visible.** Absent `internal` = no additional namespace-internal visibility. Matches the "opt-in restriction" philosophy: a restriction must be declared. A non-blocking lint for `type = "library"` projects lacking `[project.exports].public` is worth adding to nudge library authors toward explicit export lists. **Resolved.** |
| OQ-R005 | Severity of `R017 StateFieldShadowedByLocal`. | Scope-chain UX. | **Warn-level**, not hard error. Shadowing an actor state field with a local is legal but suspect; emit a warning pinned to the local's `def_span` with a note referencing the state-field span. |
| OQ-R006 ✅ | How the builtin stdlib manifest stays in sync with the eventual Ridge-written stdlib. | Long-term maintenance. | **Compile-time constant table now; a later pass replaces it with a generated table.** A regression test re-parses every stdlib `.ridge` file and asserts its `pub` exports match the constant table. **Resolved.** |
| OQ-R007 | `[project.exports].public` glob metasyntax — spec shows `"Models.*"`; what does `*` match? | Glob parser for project-exports. | **Dot-based glob: `*` matches a single segment, `**` matches any number of segments.** Implement via the `globset` crate. Convention matches `gitignore` / Bash globstar / Cargo. |
| OQ-R008 ✅ | Should `NodeId` be promoted to a proper AST field in `ridge-ast` (micro-refactor) rather than the side-table `NodeIdMap`? | Upstream contract; re-snapshot tests. | **Defer.** The import resolver uses the side-table `NodeIdMap`; the type checker may revisit when it also wants stable IDs. _Known risk:_ a future transform that clones AST nodes must preserve `NodeId`s explicitly — the side-table approach has no type-system guarantee. Document this constraint in the `NodeIdMap` rustdoc. **Resolved.** |
| OQ-R009 ✅ | `[dependencies]` table transitive conflicts between workspace members sharing a dep via `workspace-dependencies`. | Manifest resolution. | **Version pinning is the workspace's responsibility.** 0.1.0 has no semver solver; each name resolves to exactly one entry. Conflicts across members emit `M013 UnknownWorkspaceMember` or `M015 WorkspaceDependencyAbsent`. **Resolved.** |
| OQ-R010 ✅ | Should the resolver produce per-module output when earlier parse errors produce a partial AST? | LSP UX. | **Yes.** Run resolution on the partial AST; skip `Expr`/`Pattern`/`Type` nodes that descend into error markers. Goal: LSP users get resolve diagnostics for the parts of a file that did parse, mirroring `rust-analyzer` / `tsc` behaviour. **Resolved.** |
| OQ-R011 | Should an `import` that resolved to `ImportTarget::Unresolved` (R006) suppress downstream identifier errors (R010) for that module's symbols? | UX — cascading errors. | **Suppress.** If `import nonexistent.module` fires `R006`, all uses of `NonexistentModule.foo` in that file return `Binding::Error` silently (no cascade). Avoids the "1 typo → 47 errors" firehose; `Binding::Error` acts as a poison marker analogous to `ErrorT` in type checkers. |
| OQ-R012 ✅ | `spawn ActorName` — does the actor name need to be an unqualified `UPPER_IDENT`, or may it be a qualified path `Mod.ActorName`? | Spawn-expression grammar and resolve walker. | **Unqualified `Ident` only in 0.1.0** per grammar §6.19 (`"spawn" UPPER_IDENT { Expr }`). Cross-module spawn requires an `import` of the actor. Revisit in a future release if qualified-spawn ergonomics demand it. **Resolved.** |
| OQ-R013 ⚠ | Does Ridge have an implicit prelude that auto-imports `Option`/`Result` and their constructors (`Some`, `None`, `Ok`, `Err`) — and possibly `List` constructors — into every module? | All 4 example programs (`log_analyzer.ridge`, `url_shortener.ridge`, `game_of_life.ridge`, `rate_limiter.ridge`) use `Some Info`, `None`, `Ok ()`, `return Err (...)` unqualified. With no prelude, every `.ridge` file would need `import std.option (Some, None)` and `import std.result (Ok, Err)` boilerplate at the top. | **Resolved: implicit prelude.** Auto-import the following into every module's scope: the `Option` type with constructors `Some` and `None`; the `Result` type with constructors `Ok` and `Err`. Matches Haskell's Prelude, Rust's `std::prelude::v1`, Elm's default imports. A prelude pass injects synthetic `EffectiveBinding` entries equivalent to `import std.option (Option, Some, None)` and `import std.result (Result, Ok, Err)` into every module's binding pool. Conservative scope: constructors of `Option` and `Result` only; see OQ-R015 for module aliases. |
| OQ-R014 ⚠ | `let` followed by an indented continuation expression (`\|>` chain) inside a parenthesised lambda body fails to terminate at the next statement. The lexer's bracket-suppression of NEWLINE/INDENT/DEDENT folds subsequent statements into the let value, leaving downstream identifier uses unresolved. | Surfaces in `examples/game_of_life.ridge`: a `let line = row \|> List.map ... \|> List.fold ... ""` followed by `Io.println $"\| ${line} \|"` parses with `${line}` unresolvable because the parser doesn't see a statement boundary inside the enclosing `()`. | **Resolved: option A** — lexer emits NEWLINE inside brackets when col ≤ baseline; parser `parse_branch_body_flat` collects NEWLINE-separated statements as a flat `Expr::Block`. The alternatives were (a) extending the layout-suppression rule (chosen) or (b) requiring an explicit block delimiter for multi-statement lambda bodies (simpler but forces example refactoring). |
| OQ-R015 ⚠ | Should the implicit prelude also pre-bind `ModuleAlias` entries for pure-data stdlib modules (`Int`, `Float`, `Bool`, `Text`, `List`, `Map`, `Set`, `Json`) so that qualified names like `Int.parse`, `Text.padLeft`, `Float.fromInt`, `List.map` work without an explicit `import std.X as X` declaration? | The 4 canonical examples use `Int.parse`, `Int.toText`, `Text.padLeft`, `Float.fromInt` without importing the corresponding stdlib modules. With no prelude alias these produce `R012 UnresolvedQualifiedName`. | **Resolved: pre-bind module aliases for all pure-data stdlib modules; capability-bearing modules require explicit import.** Rationale: (1) preserves Ridge's capability-tracking principle — every side-effecting module remains visible at the import level; (2) matches ML/Haskell precedent; (3) removes boilerplate from data-manipulation programs. _Pure-data modules:_ `std.int` → `Int`, `std.float` → `Float`, `std.bool` → `Bool`, `std.text` → `Text`, `std.list` → `List`, `std.map` → `Map`, `std.set` → `Set`, `std.json` → `Json`. _Capability-bearing modules NOT in prelude:_ `std.io`, `std.fs`, `std.time`, `std.random`, `std.env`, `std.cli`, `std.proc`, `std.net.http`. User imports for the same `local_name` suppress the prelude binding. |
| OQ-R016 ⚠ | Should the `ImportList` grammar accept `UPPER_IDENT` so users can write `import std.net.http (Request, Response, listen, respond) as Http` and reference the imported type / constructor names unqualified? `examples/url_shortener.ridge` uses `Response` and `Request` as bare identifiers but only has `import std.net.http as Http` in scope. With the original `ImportList ::= LOWER_IDENT { "," LOWER_IDENT }` grammar, four `R010 UnresolvedIdent` errors fire on `Response`, and the only workarounds are (a) define the types locally or (b) wait for qualified record-construction `Http.Response { ... }` support which requires AST changes. | Acceptance: `examples/url_shortener.ridge` must resolve cleanly. Affects every Ridge user of `std.net.http`, `std.json`, `std.fs` typed APIs. | **Resolved.** `ImportList` accepts both `LOWER_IDENT` and `UPPER_IDENT`. Rationale: aligns with Ridge's reference languages (Haskell, Elm, Rust all permit type/constructor imports in item lists). Implementation: 1-line grammar amendment; ~5-line parser change accepting `UpperIdent` in the item-list arm; BUILTINS extension (`std.net.http` exports gain `Request`, `Response`). `examples/url_shortener.ridge` rewritten to `import std.net.http (Request, Response, listen, respond) as Http`. The complementary `Module.Type { ... }` qualified record-construction ergonomic is deferred to a future release. |

---

## 17. Appendices

### Appendix A — Canonical example programs

_Note: examples in this appendix reflect decisions D044..D061. In particular, `?>` is the ask operator (D045) and `main` returns `Result Unit Error` (D059)._

Four programs must compile and run correctly by the "Complete" milestone (Phase 7). They serve as acceptance tests for the compiler.

- **A.1. Log analyzer** (`examples/log_analyzer.ridge`) — file IO, text parsing, pattern matching, list ops.
- **A.2. URL shortener** (`examples/url_shortener.ridge`) — actors, concurrency, HTTP, JSON.
- **A.3. Game of Life** (`examples/game_of_life.ridge`) — actors, timers, terminal IO, Set.
- **A.4. Rate limiter per IP** (`examples/rate_limiter.ridge`) — actors, `spawn`, `?>` (ask), state, `time`, `Map`. Reference implementation in session notes; exercises the actor model end-to-end.

### Appendix B — Glossary

- **Actor**: isolated process with private state and a mailbox for messages.
- **Ask**: synchronous message send with reply (`handle ?> msg`).
- **BEAM**: the Erlang virtual machine.
- **Capability**: a tag in a function's signature that names an effect the function may perform (`io`, `fs`, etc.).
- **Core Erlang**: Erlang's intermediate representation, a compilation target for 0.1.0.
- **Forbid rule**: a compiler-enforced architectural constraint declared in `[workspace.rules]` that prevents module-to-module dependencies.
- **HM**: Hindley-Milner type system.
- **IR (Ridge Core IR)**: target-neutral intermediate representation; the contract between frontend and backends.
- **Monomorphization**: compiling a generic function to one concrete version per use.
- **Send**: asynchronous message send (`handle ! msg`).
- **Typeclass**: an interface-like construct for ad-hoc polymorphism (deferred to 0.2.0).
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

_This is a living document. Amend it as the project evolves, but treat every change as a deliberate design decision worth recording in the Decision Log._
