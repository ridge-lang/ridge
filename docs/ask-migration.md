# Migrating actor asks

`handle ?> message args` now returns `Result reply AskError`. A deadline or
an unavailable target is a value the caller handles. `Actor.tryAsk` keeps the
same contract, with its timeout supplied as a required function argument.

The examples assume a `Counter` with a zero-parameter handler `on getCount () -> Int`. Write `getCount ()` at each invocation; [explicit-call migration](explicit-calls-migration.md) covers older bare messages.

For a Result-returning function, import `AskError` when naming its error type
and propagate with `?`:

```ridge
import std.actor (AskError)

fn time read (counter: Handle Counter) -> Result Int AskError =
    let count = counter ?> getCount () timeout 1000 ?
    Ok (count + 1)
```

The suffix applies to the entire ask. To compute a deadline with another
fallible operation, keep that propagation inside parentheses:
`counter ?> getCount () timeout (loadDeadline () ?) ?`.

A domain error can wrap `AskError` through a direct conversion:

```ridge
type ReadError = | ActorUnavailable AskError

instance ErrorFrom AskError ReadError =
    errorFrom (error: AskError) -> ReadError = ActorUnavailable error

fn time read (counter: Handle Counter) -> Result Int ReadError =
    let count = counter ?> getCount () ?
    Ok count
```

The editor offers **Propagate this ask's error with `?`** when a mismatched or
discarded ask is inside a compatible Result scope. It uses the compiler's
conversion solver for concrete destination errors. It preserves the signature
and stops at lambda boundaries. An incompatible error type or a callback that
returns a plain value needs an explicit handling decision.

For an unused ask, the action explicitly discards the successful reply with
`let _ =` and propagates its error with `?`. Its title states that policy. It
does not rewrite an ask whose Result is already consumed by another call.

An actor handler's return annotation does not establish a function propagation
scope. Handle its ask with `match`, or put the fallible calculation in a
Result-returning helper function. The editor does not suggest an unsupported
handler-level `?`.

For example, an HTTP callback can match `Ok reply` and turn `Err error` into a
503 response. A worker can log a failure and choose a fallback according to its
own policy. The editor does not silently unwrap or invent such a fallback.

A handler returning `Result Int DomainError` makes the ask return
`Result (Result Int DomainError) AskError`. The first `?` handles transport;
the handler's Result is still a value to match or propagate separately.

Deadlines are unchanged: an omitted `timeout` uses 5000 ms (5 seconds).
Use an explicit count to choose another deadline, for example
`handle ?> message args timeout 1000` for 1000 ms.

`timeout never` removes the deadline; it still returns `Err Noproc` when the
target is unavailable. An `Ok` reply confirms that earlier messages from this
sender to the actor were handled. An error does not prove delivery.
