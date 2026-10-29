# Explicit calls without arguments

Write `()` when invoking a function or actor handler without arguments. A
parameterless declaration can retain either supported form: `fn ready -> Bool
= true` or `fn ready () -> Bool = true`. These are alternative declarations,
not declarations to put together in the same module.

A bare ordinary function name is its value. Keeping that value does not run
the function, and an expected result type does not invoke it:

```ridge
fn ready -> Bool = true
const callback: fn () -> Bool = ready

pub fn choose () -> Int =
    let f = callback
    if f () then 7 else 0
```

The same rule applies to imported functions, stdlib functions and local
helpers. Actual values such as `Map.empty`, `List.empty`, `Set.empty`,
`Bytes.empty` and nullary union constructors retain their value syntax.

When a callback's parameter is itself a function, an annotation can distinguish
zero parameters from one real `Unit` parameter:

```ridge
pub fn invoke (f: fn () -> Bool) -> Bool = f ()
```

Without a known signature, `fn f -> f ()` infers a function taking `Unit`.
It does not infer a function with zero parameters. Annotate `f` when passing
zero-parameter functions through that callback.

## Actor messages

Both `on ping = ...` and `on ping () = ...` declare a zero-parameter handler.
Its invocation has an explicit marker on every messaging route:

```ridge
import std.actor as Actor

actor Counter =
    state count: Int = 0
    on increment = count <- count + 1
    on get () -> Int = count
    on consume (_: Unit) = ()

pub fn time exercise (h: Handle Counter) -> Unit =
    h ! increment ()
    let _ = h ?> get ()
    let _ = Actor.send h (increment ())
    let _ = Actor.tryAsk h (get ()) 1000
    h ! consume ()
```

The last line supplies a real `Unit` argument. Zero parameters and one `Unit`
parameter remain distinct even though both invocations contain `()`. Messages
with other payloads retain their existing argument syntax. Bare handler names
at messaging sites are not first-class message values. `spawn` and `child`
construction syntax is unchanged.

## Migrating existing source

Preview changes without writing:

```text
ridge fmt --migrate-calls --check
ridge fmt --migrate-calls --check path/to/source.ridge
```

Apply the migration to a workspace, file or directory:

```text
ridge fmt --migrate-calls
ridge fmt --migrate-calls path/to/source.ridge
ridge fmt --migrate-calls path/to/sources
```

The operation uses the selected handler's declared parameters. It preserves
real payloads, comments and line endings, and a second pass makes no changes.
It also keeps function-form messages grouped as one argument, for example
`Actor.send h ping` becomes `Actor.send h (ping ())`.

Resolve other reported errors before migrating. If a receiver's handler
cannot be determined, add a type annotation instead of guessing its arity;
the migration leaves the file unchanged. The operation needs source context,
so it is not combined with `--stdin` or `--migrate-tests`.

Normal `ridge fmt` does not perform this migration. The editor offers an
**Add explicit `()` to this message invocation** quick fix for a resolved
missing marker. Capability, type, payload and class checks still apply after
the edit.

When a function reference is used where its result is required, the type
diagnostic explains the missing invocation. The editor can add `()` to a
known pure function reference, including an `if` condition. It does not add
capability permissions or rewrite valid function values and callbacks.
Bulk migration does not invoke ordinary function references: doing so could
introduce effects that the original program never executed.
