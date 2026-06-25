# Ridge naming conventions

How Ridge names its public surface — standard-library modules, types, functions,
parameters, language keywords, and the `ridge` CLI. The goal is a surface that reads
as one design: a name you can guess, that reads the way you would say it aloud, and
that matches its siblings in other modules.

This is the guide for anyone adding to the standard library, the language, or the
tooling. `docs/spec.md` is the contract; this is the naming layer on top of it.

## Casing

- **Types and constructors** are PascalCase: `Int`, `Text`, `Option`, `SqlValue`,
  `LeftJoin`, `Some`, `Ok`.
- **Everything else** — functions, methods, record fields, parameters, locals, module
  names — is camelCase. A one-word name is lowercase (`code`, `path`); a multi-word name
  is lower-camel (`readFile`, `exitCode`, `httpOnly`). snake_case does not appear in the
  public surface.
- **Capabilities** are lowercase single words: `io`, `fs`, `net`, `time`, `random`,
  `env`, `proc`, `spawn`, `ffi`, `db`.

## Argument order: data-last

The value being operated on comes last. Operation-specific arguments — functions,
predicates, keys, separators — come first. This is what makes pipelines read left to
right and partial application natural.

```
xs |> List.map double |> List.filter isEven
users |> Repo.query |> Repo.filter (fn u -> u.active) |> Repo.orderBy Asc (fn u -> u.name) |> Repo.toList
```

The single exception is the database connection at the adapter layer: it is the receiver
of the effect, not the data being transformed, so it comes first.

## Parameter names

Two registers, chosen by role.

Generic structural roles keep short, conventional names — these are vocabulary, and stay
uniform across the library:

- `f` a function, `g` a second function
- `p` or `pred` a predicate
- `xs` / `ys` lists, `x` / `y` elements
- `acc` a fold accumulator
- `k` / `v` a map key and value, `m` a map
- `s` a set or a text, `n` a count or length
- `o` an Option, `r` a Result

Everything domain-specific is spelled out in full: `path`, `content`, `command`,
`connection`, `low` / `high`, `name`, `value`, `status`, `body`, `port`. Not `cmd`, not
`conn`, not `lo` / `hi`.

When two arguments differ in role, the names show it — `base` / `override`,
`left` / `right`, `expected` / `actual` — never `a` / `b`.

## Verbs and affixes

A small, fixed set of prefixes and suffixes carries meaning. Reuse them rather than
inventing a synonym.

- `is…` — boolean tests: `isEmpty`, `isSome`, `isDir`.
- `to…` / `from…` — total conversions: `toText`, `toList`, `fromList`, `fromInt`,
  `toRow` / `fromRow`. The inverse of `fromIso` is `toIso`.
- `parse` — text to `Option`: `Int.parse`, `Float.parse`. A strict variant that fails
  hard is `…Strict`.
- `…By` — a variant taking a selector function: `sortBy`, `groupBy`, `orderBy`.
- `…With` — a variant taking a combiner: `zipWith`.
- `…On` — a variant taking a condition: `joinOn`, `leftJoinOn`.
- `…Of` — a scalar aggregate over a column accessor: `sumOf`, `avgOf`, `minOf`, `maxOf`.
- `with…` — a setter on an opaque builder: `withDefault`, `withSecure`, `withMaxAge`.
- `contains` — membership, the same word for every collection.

The functor family is `map`, `filter`, `flatMap`, and `fold` (left) / `foldRight`. The
same verb is reused across `List`, `Option`, `Result`, and `Map`, even where the callback
shape differs — a `Map` callback receives both key and value.

## Collections

- Element count is `length` — the same word for every collection, and for the grapheme
  length of a text. The byte length of a text is the explicit `byteSize`.
- Set operations are nouns: `union`, `intersection`, `difference`.

## Errors

Every fallible operation returns `Result _ Error`, where `Error` is the standard
`{ code, message }` record. No module returns a bare `Text` on the error side.

## Modules and types

- A type is not prefixed with its module's name — the module already qualifies it. Write
  `proc.Output`, `time.Duration`, `net.http.Request`; not `proc.ProcOutput`.
- Opaque safe-wrapper types are PascalCase nouns — `Sql`, `Html`, `SecureCookie`. Their
  lowercase constructor may share the name (`sql`, `html`).
- A class is named for what it provides, and must not collide with another domain's verb.
  The JSON codec owns `Encode` / `Decode`, so a query-result terminal is named for fetching
  its results (`Fetchable`), not for decoding.
- A prelude union variant takes a prefix only when it would otherwise shadow a primitive:
  `JsonValue` is `JInt | JText | …`, while `Ordering` is plain `Less | Equal | Greater`.

## The data API

The query and persistence layer follows everything above, plus three rules of its own.

- Filtering is `filter`, never `where`. `where` introduces a constraint clause in the
  language, so query narrowing uses `filter`.
- Columns are referenced by typed accessors — `fn (u: User) -> u.email` — never by a string
  name.
- The write path is `insert`, `update`, `delete` for whole entities, and `setWhere` for a
  typed partial update. One vocabulary, not a separate verb for every shape.

## The CLI

- Subcommands are single lowercase verbs: `build`, `run`, `check`, `fmt`, `test`, `new`,
  `init`, `repl`.
- Flags are `--kebab-case`; their values are lowercase (`--emit beam`).

## Established names we keep

A few names stay as they are even though they bend a rule above, because they are widely
understood and changing them would cost more than it gives:

- `fn` and `pub` — abbreviations, but universal.
- `fmt` — the conventional name for a formatter subcommand.
- `Text`, rather than `String`.
- `if … then … else …`, with an explicit `then`.
- `spawn` as both a keyword and a capability; `Timestamp` as a primitive type; `let` and
  `var` as separate bindings.
