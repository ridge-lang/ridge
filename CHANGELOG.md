# Changelog

All notable changes to Ridge will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Standard-library union types can be imported and used from user code. `import std.m (T, MkT)` brings the type and its constructors into scope, so the type resolves in annotations and its constructors build and match by name — with no hand-written accessor functions in between. The first one is `std.query`'s `SortOrder` (`Asc | Desc`); a constructor of any stdlib `pub type` union now lowers the same way a user-defined type's constructor does.
- Quotation: a lambda passed where a `Quote` is expected is captured as an expression tree instead of being compiled to a closure — the same idea as C#'s `Expression<Func<>>`. A predicate like `fn u -> u.age >= 18 && u.active` is type-checked against the entity's columns and reified into a `QExpr` value the program can walk at runtime. Field accesses become column references by their SQL name (a `signupYear` field reads back as `signup_year`), boolean columns work as predicates on their own, and an unknown column (`T039`), an unsupported form (`T040`), a mismatched comparison (`T041`), or an entity that can't be determined (`T042`) are reported at compile time. This is the mechanism the query layer builds on.
- Multi-parameter type classes: a class can take more than one type parameter, e.g. `class Convert a b = convert (x: a) -> b`. Instance heads list one type per parameter (`instance Convert Celsius Fahrenheit`), coherence is keyed by the whole head tuple so instances that share a leading type but differ later coexist, and a call selects the instance from the types at every head position. A head position the caller leaves undetermined is reported as an ambiguous constraint to annotate.
- The query layer compiles a captured body to SQL. `Query.toSql` turns a quoted predicate into a parameterized statement and its bind values (`Quote f -> (Sql, List SqlValue)`): each literal becomes a `?` placeholder and its value is collected left to right, so a predicate reaches the database parameterized, never interpolated. `Query.orderSql` compiles a quoted ordering key (`fn u -> u.createdAt`) together with an `Asc`/`Desc` direction into an `ORDER BY` fragment. `Query.selectSql` compiles a quoted projection — a record of columns like `fn u -> { id = u.id, year = u.signupYear }` — into a select-list, emitting `column AS alias` when a field is renamed from its source column. A quoted body now type-checks as a projection when it returns a record and as an ordering key when it returns a single column, not only a boolean predicate.
- `std.sql` gains the monomorphic bind constructors `sqlInt`, `sqlText`, `sqlBool`, and `sqlFloat`, which build a `SqlValue` of a known base type without going through the `SqlType` class.
- A function parameter can destructure in the binder: `fn area (Point { x, y }: Point) -> Int = x + y` reads the record's fields directly, and `fn diff ((a, b): (Int, Int)) -> Int = a - b` unwraps a tuple — no `let` in the body. The pattern must be irrefutable, since the function is applied to every value of its type; a refutable one such as `fn f (Some x: Option Int)` is rejected with `T043`. The annotation is required, so an un-annotated pattern parameter still reports `P012`.
- A type class can be instanced over a function type, so a bare function satisfies a class constraint without a wrapper. `instance Run (Int -> Int)` makes any one-argument function a `Run`, and a plain `fn x -> x + 1` passed where `Run` is required dispatches to it — at a direct call or forwarded through a constrained consumer (`fn useRun (f: a) … where Run a`). Dispatch keys on the function's arity; the capability annotation is not part of the key yet, so a pure and an effectful handler of the same arity share an instance. A function whose arity has no instance reports the usual `T029`. This is the groundwork for passing bare functions where a handler is expected.
- `deriving (Row)` reads a database row back into a record. A row is a `Map Text SqlValue` keyed by column name; the derived `Row` instance's `fromRow` reads each field from its column — the same snake_cased spelling the `Table` derive uses, so `createdAt` reads `created_at` — runs the field type's `SqlType.fromSql`, and returns the assembled record or the first decoding error. Field types must have a `SqlType` instance (`Int`, `Text`, `Bool`, `Float` today); a missing column or a value whose type does not match the field yields an `Err`, and deriving `Row` on a union or on a record with an unsupported field type is rejected. This is the read half of the data layer's row mapping.
- A storage `Adapter` and an in-memory adapter — the start of the data layer's backend seam (`std.data`). `Adapter` is the class every backend implements: `insert` appends a row to a table and `all` reads a table back, with rows crossing as `Map Text SqlValue`, so they decode straight through `deriving (Row)`. `memAdapter` opens a process-backed in-memory store for tests and local development; repository code written against `Adapter` runs unchanged against it and, later, against a real database. Opening an adapter takes the `db` capability, and the handle it returns is the proof of access — the query methods carry no capability of their own, the same handle-as-proof model an actor uses.
- The `Adapter` seam gains `select`, `get`, and `delete`. `select` and `delete` take a quoted predicate written against the queried record — `select conn "users" (fn (u: User) -> u.age >= 18)` — captured as a `QExpr` tree; the in-memory adapter walks it against each stored row, and a real database compiles the same tree to a `WHERE` clause. `select` returns the matching rows, `delete` removes them and answers how many went, and `get` looks a single row up by an exact column match. The predicate's row type is pinned by its parameter annotation, so columns and their types are checked at compile time while the rows themselves stay untyped maps that decode through `deriving (Row)` one layer up.
- A typed repository over the storage `Adapter` — the data layer's end-user query surface (`std.repo`). A `Repo e a` binds an entity type `e` to a table on an adapter `a`; its read verbs run the adapter and decode each row back into the entity through `deriving (Row)`, so a query answers `List User` rather than a list of column maps. `repo conn "users"` builds one — the entity is pinned once by the binding's annotation (`let users: Repo User MemAdapter = ...`) and flows to every call — and the verbs compose as a pipeline: `users |> Repo.findBy (fn (u: User) -> u.age >= 18)`. `all` and `findBy` answer decoded entities, `find` and `getBy` at most one, `count`/`countBy`/`exists` an aggregate, `insertRow` appends a column map and `deleteWhere` removes the rows a predicate matches. Predicates are quoted exactly as at the adapter seam, so columns and their types are checked at compile time. The repository is adapter-agnostic: the same code runs against the in-memory adapter today and a real database later.
- The repository gains a composable query builder. `Repo.query` lifts a repository into a `Query e a`; `filter` narrows it with a quoted predicate (AND-combined with any filter already set), `orderBy Desc (fn (u: User) -> u.age)` adds an ordering key — several compose, major to minor — and `limit`/`offset` page it, each pushed into one backend query rather than applied in memory. `toList` and `first` run the query and decode the matching rows into the entity. `selectList` and `selectFirst` instead project into a different shape: a projection names its result record with Ridge's record-construction syntax — `users |> Repo.query |> Repo.filter (fn (u: User) -> u.age >= 18) |> Repo.selectList (fn (u: User) -> Summary { name = u.name, year = u.signupYear })` — which fixes the decode target and lists its columns, so the backend pushes a `SELECT name, signup_year AS year …` select-list down and only those columns cross. The shape is an ordinary `deriving (Row)` record; a projection field that is not a column of the queried entity, or a projection that does not name its shape, is rejected at compile time.
- The query builder gains two-table inner joins. `Repo.joinOn` pairs a query with a second repository on a condition written against both entities at once — `users |> Repo.query |> Repo.joinOn posts (fn (u: User) (p: Post) -> u.id == p.authorId)` — and two terminals run it. `toPairs` decodes each matched row pair into both entities, answering `List (User, Post)`; `selectJoin` projects columns from across both sides into a named record — `fn (u: User) (p: Post) -> Line { who = u.name, title = p.title }` — and pushes that select-list down so only the named columns cross. Both the condition and the projection are checked against both records: a column neither entity declares, a comparison between mismatched column types, or a projection that does not name its shape is rejected at compile time. The left query's filter, ordering, and page still apply, and the same code runs against the in-memory adapter and Postgres.
- The join builder gains a left outer join. `Repo.leftJoinOn` reads exactly like `joinOn` but keeps every left row: a row with no matching right row survives instead of being dropped. Its terminal `toLeftPairs` answers `List (User, Option Post)`, where the right entity is `Some` for a matched row and `None` for an unmatched one — the optionality is in the type, so a result paired as a plain `(User, Post)` is rejected at compile time. On Postgres it compiles to a `LEFT JOIN`; an unmatched row is told apart from a matched row whose columns happen to be NULL by a sentinel column on the right side, so an all-NULL right entity still reads as `Some`, not `None`.
- Nullable columns: a `deriving (Row)` record may declare an `Option` field over one of the base column types (`Option Int`, `Option Text`, `Option Bool`, `Option Float`). A SQL NULL — or a column that is absent from the row — decodes to `None`, and a present value to `Some`; a `SqlType (Option a)` instance also writes `None` back as NULL through `toSql`. This is the read path for any table whose columns are nullable, and the groundwork a left-join projection needs to decode the right side's columns where the row had no match.
- The left join gains a projection terminal. `Repo.selectLeftJoin` projects a left join into a named shape, the left-outer analogue of `selectJoin`. Its projection's right parameter is `Option`, so a column read off it is `Option` of its type — `fn (u: User) (p: Option Post) -> Line { who = u.name, title = p.title }` fixes `Line` to `{ who: Text, title: Option Text }`, and an unmatched left row projects `title` as `None`. The result record's right-derived fields must therefore be `Option`; declaring one as a plain `Text` is rejected at compile time. The backend compiles a `LEFT JOIN` with the select-list pushed down, and a NULL right column decodes straight into the shape's `Option` field, the same on the in-memory adapter and Postgres.

### Changed

- The safe-SQL text wrapper `Sql` — with its `sql` factory and `sqlValue` accessor — moved from `std.net.http` to `std.sql`, where it sits next to `SqlValue` and the `SqlType` codec. `std.net.http` keeps the HTML and cookie hardening helpers. Import `Sql`/`sql` from `std.sql` going forward.

### Fixed

- A type annotation that named an imported standard-library type constructor applied to arguments — `Repo User MemAdapter`, `Query e a` — silently dropped its arguments and resolved to a fresh type variable, because the conversion only consulted the current module's own type names and treated an imported name as unknown. The arguments are kept now, so `let users: Repo User MemAdapter = ...` (and a function that returns one) pins the entity type as written rather than leaving it free to over-generalise. The visible symptom was a query that decoded into the wrong shape once a module derived `Row` for more than one record at a time.
- A function constrained over two instances of the same class — `where Row e, Row f`, the shape a pair decoder needs — forwarded the first instance's dictionary to every method call, so decoding the second value read the first type's fields and failed at runtime. Each call now forwards the dictionary for the type it actually produces, picked from that call's result type. The visible symptom was a two-table join's `toPairs` always erroring while the single-shape `selectJoin` worked.
- A few ordinary mistakes around union types reported an internal error (`T999`, "this is a compiler bug") instead of something useful. Using a type as a value constructor — the symptom of a single-variant union written without its leading `|`, which parses as a type alias — now reports `T044` with a hint toward the fix. Matching a record-style union variant, which isn't supported yet, reports `T044` as well. A genuinely unknown constructor is left to the resolver's `R010` rather than also raising `T999`.

## [0.3.0-rc4] - 2026-06-03

### Added

- Prelude typeclass methods are now callable by bare name from user code: `encode`, `decode`, `toText`, `eq`, and `compare` resolve without the caller needing to redeclare the class. `deriving (Encode, Decode)` now works through its intended API — `encode x` and `decode j` call the derived instance directly.

## [0.3.0-rc3] - 2026-06-03

### Added

- `JsonValue` is now a first-class prelude type. The constructors `JNull`, `JBool`, `JInt`, `JFloat`, `JText`, `JList`, and `JObject` are available without an explicit import, replacing the opaque tagged-tuple representation that previously required going through the `Json` module accessors.
- `deriving (Encode)` and `deriving (Decode)` for user records and unions. `Encode` serialises a value to `JsonValue`; `Decode` deserialises in the reverse direction. Both work on named records, unions with positional and named-field constructors, and types that mix the two.
- Generic/parametric derived instances: `type Box a = { val: a } deriving (Encode)` produces a constrained instance that propagates `Encode a` automatically. Parametric derivation works for any number of type parameters.
- `where`-constrained instance heads: `instance Encode (List a) where Encode a` — the full parametric-instance grammar is now accepted, resolved, and dictionary-passed to the BEAM.
- Eight `Encode` / `Decode` instances in the standard library covering the four generic prelude containers in both directions: `List a`, `Option a`, `Map Text v`, and `Result a b`.

## [0.3.0-rc2] - 2026-06-01

### Changed

- Language server: edits recompile incrementally. A single-file change re-checks only the edited modules and the modules that transitively import them, instead of the whole workspace, so diagnostics and editor features stay responsive as a workspace grows. A class, instance, or deriving change re-runs the workspace-wide coherence checks; a save reseeds from disk.

### Fixed

- Language server: diagnostics, hover, and go-to-definition resolve against the editor's buffer rather than the last-saved file on disk, so they reflect unsaved edits.

## [0.3.0-rc1] - 2026-06-01

### Added

- Language server: hover shows the inferred type of the symbol under the cursor.
- Language server: go-to-definition jumps to a name's binding site, including across modules.
- Language server: scope-aware, context-sensitive completion — locals in scope, this module's symbols, imports, and keywords, plus a module's exported names after `Module.`.

### Changed

- Language server: positions are exchanged as UTF-16 code units, so diagnostics and editor features land on the correct column in lines containing non-ASCII text.
- The type checker reuses the resolver's node-id map instead of rebuilding its own, and `CheckTypedArtefacts` now exposes the full resolved workspace.

## [0.2.11] - 2026-05-30

### Fixed

- A record `with` update inside a closure whose parameter type is not annotated — for example `fn acc -> acc with { v = acc.v + 1 }` passed to `List.fold` — now works. It previously compiled to a unit value and crashed at run time, because the update was reconstructed from the record's declared fields and that shape was unknown at the closure body. `with` now lowers to a direct map update of only the touched fields, so it no longer depends on the record type being known at the update site.

## [0.2.10] - 2026-05-30

### Added

- A benchmark suite (`ridge-bench`) for tracking compile performance over time. A native layer measures the pipeline from lexing through Core Erlang emission with criterion; a BEAM layer times generated code through a micro-benchmark harness. A workflow records the native layer on every pull request.

### Fixed

- Editor diagnostics now land on the correct file and line. A diagnostic for a source file that was not open in the editor previously collapsed to `<unknown>` at line 1; spans are now resolved against the text the compiler read, and the document is identified from its workspace-relative path.
- The feature list in `README.md` no longer advertises LSP hover and go-to-definition, which are not implemented yet.

## [0.2.9] - 2026-05-30

### Added

- The parser bounds expression nesting depth at 256 levels (`P028 ExpressionTooDeep`). Pathologically nested input — thousands of nested parentheses, lists, or operator chains — now reports a diagnostic instead of overflowing the native stack and aborting the compiler with no message.

### Changed

- The release installer requires cosign signature verification by default when a release is signed. A missing `cosign` previously continued with an advisory, but the SHA256 sidecar is fetched from the same origin as the archive, so it guards transport integrity without attesting provenance. The installer now refuses to install a signed release it cannot verify; set `RIDGE_SKIP_SIGNATURE=1` to opt out. Unsigned (older) releases are unaffected.

### Security

- `@ffi` declarations are now rejected in user code at build time (`R022 FfiOutsideStdlib`). FFI is a standard-library-only privilege; the gate existed but was never invoked during a normal build. In the same pass, the standard library's own `@ffi` declarations are validated against the capability audit table as it builds — an FFI target with missing capabilities or an unknown callee now fails the build — closing the gap between a documented safety check and one that actually runs.

## [0.2.8] - 2026-05-29

### Added

- Multi-line string literals (`"""..."""`): triple-quoted strings strip leading indentation guided by the closing delimiter's column, drop the opening and closing newlines, and process standard escape sequences normally. The single-line `"..."` form is unchanged and still stays single-line.
- Raw string literals (`r"..."`, `r#"..."#`, `r##"..."##`): no escape processing; every byte between the delimiters is literal. Extra `#` pairs balance embedded quotes. Raw strings may span multiple lines without indentation stripping, complementing the cooked `"""` form.
- List patterns with rest: a single `..` in any position — `[first, ..]`, `[.., last]`, `[first, .., last]`. Bind the rest through the existing as-pattern operator: `[first, rest @ ..]`. At most one `..` per list pattern (`P024 MultipleRestInListPattern`); suffix and middle elements are restricted to bindings or wildcards in 0.2.8 (`L009`).
- Record patterns in `match`: a record-body pattern such as `User { name, age }` binds each named field, and a trailing `..` (`User { name, .. }`) matches the named fields while ignoring the rest. Naming an unknown field is an error, and omitting a field without `..` is reported as missing. The `..` itself cannot be bound.
- `@test "<name>"` attribute for marking test functions regardless of name or visibility. The string is the display name shown by `ridge test`. Return type is unchanged (`Result Unit Text`). When both `@test` and the `test_` prefix apply to the same function, the attribute wins and the test registers once.
- `ridge fmt --migrate-tests` rewrites `pub fn test_*` functions to the `@test` form in place. It inserts `@test "<derived-name>"` above each matching function without renaming it (derived name is the function name with `test_` stripped). The rewrite is idempotent — a function already carrying `@test` is left untouched.
- Design decisions covering the string literal syntax choices, rest pattern semantics, and test-attribute design are documented in `docs/spec.md §15`.

### Deprecated

- The `test_` function-name prefix for test discovery is deprecated (`C304 PrefixTestDeprecated`). Both `@test "<name>"` and `test_` continue to work in 0.2.x; the prefix is removed in 0.3.0. Use `ridge fmt --migrate-tests` to update existing test files.

### Resolved

- Open question Q-017 (multi-line and raw string literal syntax), carried since 0.1.0, is now resolved. See `docs/spec.md §16.1`.

## [0.2.7] - 2026-05-28

### Added

- Actors can now declare a `mailbox` configuration member alongside `state`, `init`, and `on`. Three forms are accepted: `mailbox unbounded` (the explicit default), `mailbox bounded N drop newest` (silently drop the incoming message on overflow), and `mailbox bounded N error` (raise `{mailbox_full, Pid}` in the sender on overflow, so a supervisor can decide what happens next). The capacity `N` is a positive integer literal; the overflow policy is mandatory when bounded, with `P022 MailboxPolicyMissing` if it is missing and `P023 MailboxBoundInvalid` if `N` is zero, negative, or out of range. The configuration keywords (`bounded`, `unbounded`, `drop`, `newest`, `oldest`, `error`) stay reserved only inside the `mailbox` member; everywhere else they remain ordinary identifiers. The historical unbounded default carries through unchanged when the member is omitted, so existing programs keep their behaviour byte-for-byte.
- New `std.actor` module shipping `Actor.mailboxSize : Handle a -> Option Int`. `Some n` is the queue length at the moment of the call; `None` means the actor is no longer alive. The function is cap-free: the handle itself is the proof of access, formalised in the new §6.4.1 "Handles as effect tokens" subsection. Reading the queue length is best-effort under concurrent senders — the value reflects what the next sender would see, which may briefly diverge from the strict bound under contention; the spec §7.2.1 documents this as a soft-bound invariant.
- Design decisions for the mailbox feature are collected into a new §15 of the spec, covering the syntax choice, the overflow-policy set, the let-it-crash semantics for `error` via `!`, the observability scope, and the "handle as effect token" capability rule that motivates the cap-free design.

### Changed

- The actor handle's runtime representation changed from a bare `Pid` to the opaque tuple `{ridge_handle, Pid, MailboxConfig}` so that `!` can honour the mailbox configuration carried by the handle without an extra dispatch hop per send. The Ridge surface is unaffected — the spec already treated `Handle a` as opaque — but any hand-written Erlang glue that assumed `is_pid(Handle)` needs to update to `is_tuple(Handle) andalso element(1, Handle) =:= ridge_handle`. The runtime exposes `ridge_rt:send_op/2` (the new target of `!`) plus `ridge_rt:mailbox_size/1` (read by `Actor.mailboxSize`) for direct integration paths. `ridge_rt:send/2` stays as a backward-compatible cast bridge for tooling built before this cut.
- `import` paths now accept the `actor` keyword as a module-path segment. The keyword stays reserved everywhere else; without this contextual exception, `import std.actor as Actor` collapsed silently to `import std` and `Actor.mailboxSize` at use sites failed to resolve. The change is purely additive — no existing import shape parses differently.

### Resolved

- Open questions Q-003 (actor mailbox size) and Q-014 (mailbox observability) from the 0.1.0 deferred-to-0.2.0 list are now closed. See `docs/spec.md §16.1`.

### Deferred

- The `drop oldest` mailbox policy is parsed but type-check-rejected (`T027 MailboxPolicyDropOldestNotShipped`); programs using it get a precise diagnostic listing the two policies that do ship. Implementing the policy needs a broker process intermediary because BEAM does not permit a sender to mutate another process's mailbox; it is deferred pending a broker implementation.
- The result-returning `Actor.send (h: Handle a) (msg) -> Result Unit MailboxFull` waits on first-class message values, which depend on the typeclass infrastructure not yet in scope. Until then `!` covers fire-and-forget and `bounded N error` surfaces overflow at the call site through a let-it-crash exit signal.
- `Actor.peek` and `Actor.drain` are deferred for the same reason — they require the message-shape typeclass infrastructure to be tipable. `drain` is additionally destructive and waits for a concrete use case before shipping.

## [0.2.6] - 2026-05-26

### Added

- `std.net.http` gains four web-hardening surfaces. `Http.sql raw` returns a `Sql` wrapper around a value with embedded single quotes doubled (`'` -> `''`); `Http.html raw` returns an `Html` wrapper around a value with the five HTML special characters (`&`, `<`, `>`, `"`, `'`) escaped to entity references, with `&` processed first so existing entities double-encode predictably. Both are pure (no capability). The newtypes are records with a single `value` field; pattern matching is via field access. Direct record-syntax construction (`Sql { value = "..." }`) is technically possible but bypasses the escape pass — the type wrappers are a hint to the type checker and to code review, not a runtime-enforced boundary. The SQL escape is not a substitute for parameterized queries, and the doc comment says so.
- `std.net.http.SecureCookie` record + the `Http.secureCookie name value` factory. The factory returns a value with `secure = true`, `httpOnly = true`, `sameSite = "Lax"`, `maxAge = None`, `path = None` — the three boolean / string defaults are the Q-024 mitigations for XSS / CSRF / network-eavesdropping. `Http.secureCookieHeader c` flattens the record to a `Set-Cookie:` header value, emitting `; Attribute` segments only for the attributes that are set. Override individual fields with the record-update form when a workflow needs `SameSite=Strict`, an explicit `Max-Age`, or a non-root `Path`.
- String interpolation `${x}` now dispatches to a user-defined `toText` when the hole's type is a user `TyCon` whose owning module exports `pub fn toText (x: T) -> Text`. The dispatch is opt-in by naming: no new syntax, no global typeclass table. The closed `ToText` set (Int / Float / Bool / Text / Timestamp) keeps its existing path; user types now get a third path that synthesizes a `Call(External, toText, [arg])` to the type's owning module. Missing `toText` falls back to the existing `L007 ToTextLowering` defensive path.

### Changed

- The CI workflow now runs `cargo build`, `cargo test`, `cargo fmt --check`, and `cargo clippy -D warnings` on Ubuntu 22.04, macOS 14, and Windows 2022 on every PR and push to `main`, not just Linux. `fail-fast: false` so a problem on one runner does not mask issues on the others. The Linux-only disk-cleanup step is gated to the Linux job; the macOS and Windows runners have headroom and use different filesystem layouts. Rust toolchain install, Erlang/OTP install via `erlef/setup-beam`, and the cargo cache key all work unchanged across platforms.
- HTTP server responses built through `ridge_rt:http_build_response/1` now carry two security headers by default: `Content-Security-Policy: default-src 'self'` and `Strict-Transport-Security: max-age=31536000`. CSP `default-src 'self'` blocks third-party script/style/image sources at the browser level; HSTS `max-age=31536000` asks the browser to upgrade subsequent same-host requests to HTTPS for one year. `includeSubDomains` and `preload` are deliberately omitted from the HSTS value because they are deployment-policy decisions that depend on whether the operator owns every subdomain. Per-response override is deferred to a future release once `Response` gains a `headers` field — that change would be breaking for the record's shape and does not fit a 0.2.x patch.
- The VS Code Marketplace attestation gets a headless lane. A new `.github/workflows/marketplace-attest.yml` workflow installs `ridge-lang.vscode-ridge` from the Marketplace on Ubuntu 22.04 and macOS 14 runners on every `release: [published]` event (plus `workflow_dispatch` and PRs that touch the workflow itself), then asserts the published version appears in `code --list-extensions --show-versions`. Per-run evidence (date, runner, VS Code version, installed extension version) is uploaded as a build artefact. The visual slice (syntax highlighting + live diagnostics rendered in the editor) still requires a human on real hardware; the Linux and macOS rows in `docs/marketplace-attestation.md` are split accordingly.

## [0.2.5] - 2026-05-25

### Fixed

- Multi-step type alias chains now resolve all the way to their terminal body. `type IntList = List Int` followed by `type Numbers = IntList` used to leave `Numbers` interned as `Type::Con(IntList, [])` because pass 2 of user-tycon collection reads `ctx.tycon_decls` before that table has been synced from the arena, so every later alias saw its earlier siblings only as their pass-1 placeholders. A dedicated third pass walks every alias body after pass 2 and expands any embedded `Type::Con(alias_id, args)` to the alias's resolved body, chasing through the chain until it lands on a non-alias terminal type. A visited set breaks cycles defensively.
- Parametric type aliases (`type Stack a = List a`, `type Pair a b = (a, b)`) now substitute through to their bodies at every use site. The previous shape `TyConKind::Alias(Type)` held only the body, with no record of the alias's own type-parameter vids, so `Stack Int` at a use site fell through to an opaque `Type::Con(Stack, [Int])` that never unified with `List Int`. `TyConKind::Alias` now carries `{ params, body }`; the body keeps the alias's parameters as `Type::Var(p_i)` placeholders and use sites substitute them with the call-site argument types before wrapping in `Type::Alias { name, body }`. The chain pass also substitutes when crossing parametric aliases, so `type Stack a = List a; type IntStack = Stack Int` resolves `IntStack` directly to `List Int` at collection time.

### Added

- `Text.slice (start: Int) (len: Int) (s: Text) -> Text` extracts a substring counted in grapheme clusters, completing the text-API set that already shipped `byteSize`, `length`, and `join`. Indexing follows the rest of the text surface (graphemes, not bytes), so `slice 0 2 "café"` is `"ca"` and `slice 3 1 "café"` is `"é"`. Both bounds saturate — a start past the end gives `""`, a length larger than what remains returns the tail, and negatives clamp to zero, so arithmetic results can be passed without guards. Bridged through `ridge_rt:text_slice/3`, which wraps `string:slice/3`.

### Changed

- `ridge test` now prints per-test results live as workers finish, with an `[N/M]` prefix aligned to the width of `M`. Long suites no longer look frozen until the last worker joins. Output stays in input order — the same deterministic shape CI logs depend on — because each worker drains a shared print cursor and only prints contiguous-ready outcomes from there forward; the worker that finishes the next head-of-queue test takes over the drain. The tally now reads from atomic counters incremented at print time rather than a post-scope iteration.

## [0.2.4] - 2026-05-25

### Fixed

- `ridge run` now aborts when the compile pipeline emits error-severity diagnostics instead of silently launching the BEAM module on top of stale artefacts. When a project's `[capabilities].allow` list omitted a capability the source actually used (for example `Io.println` against `allow = []`), `ridge build` correctly emitted `R016` but `ridge run` re-executed a `.beam` left over from a previous good compile, bypassing the capability contract declared in `ridge.toml`. The driver now returns a new `RunError::CompileDiagnostics` whenever the pipeline produced any `Severity::Error` diagnostic; the CLI's `execute_plain` matches the variant and renders the inner diagnostics via the same `render_diagnostics` path `ridge run --observer` and `--watch` already used. Warnings still do not gate run.
- User-defined union types with positional constructors (`type Shape = Circle Int | Rectangle Int Int`) now parse and run correctly end-to-end. Two bugs were blocking this: the parser's type-body disambiguation only looked one token ahead, so a `|` beyond the first constructor's arguments caused `P002 expected top-level decl`; and the lowering pass wrapped a union constructor call in `IrExpr::Call` instead of folding the arguments into the `IrExpr::Construct` node, which made the BEAM crash with `{badfun, 'Circle'}`. Nullary unions (`type Color = Red | Blue`) and type aliases (`type Wrapper = Inner Int`) are unaffected.
- `E001 StateField Assign requires actor-handler context` now distinguishes the two cases it covered.  When the assign reaches the regular expression-lowering path from inside an actor handler (i.e. via a nested `fn`/lambda body), the diagnostic names the offending state field and points at the canonical workaround: extract the inner-fn loop to a top-level helper that returns the accumulated values as a record, then assign once in the handler body from the record's fields.  The plain "requires actor-handler context" wording — accurate, but actionable only for someone who already knows the codegen — stays for the genuine "no actor context at all" case.
- Forward references to actor types inside the same source file now typecheck without spurious diagnostics. An actor declared earlier whose `state target: Handle SomeActor` mentioned an actor `SomeActor` defined further down used to resolve `SomeActor` to a fresh `Type::Var`, so the subsequent `target ! msg` raised `T020 send (\`!\`) on non-actor / found type Con(_, [Var(_)])` and lowering failed with `E001 StateField Assign requires actor-handler context` higher up. The fix is a proper two-pass user-`TyCon` collector: pass 1 interns a placeholder `TyConDecl` for every `TypeDecl` and `ActorDecl` so all names have a stable `TyConId` before any field types are resolved; pass 2 builds the real `TyConKind` schemas and writes them back via a new `TyConArena::replace_kind`. The previous code interned placeholders, discarded them, and rebuilt incrementally — so the `name_to_id` map was empty for forward references, the typechecker fell through to `Type::Var(fresh)`, and the placeholders survived in the arena as zombie entries that the snapshot tests had baked into their counts (now updated). One regression test (`forward_actor_type_reference_typechecks_cleanly`) pins the case.
- `spawn ActorName` lowered from inside another actor's handler now resolves to the same sub-module name the actor was emitted under. Before this fix the handler scope carried `own_module_beam_name: None`, so the spawn lowering fell through to its test-only fallback (`ridge_actor_<id>_<name>`); the spawned gen_server then crashed at startup with `undefined function ridge_actor_*:init/1` because nothing in the compiled output exports that atom. The `with_actor_parent` scope constructor now carries the parent module's BEAM name through, mirroring what `with_arity_and_module` already did at the top level. Apps with a supervisor that respawns workers from inside a handler (`on respawnAt (i: Int) = let fresh = spawn Worker in …`) work without the registry-pattern workaround that previously moved every spawn out into main.
- Non-parametric type aliases (`type Bag = List Int`, `type Row = Map Text Text`) now unify with their body at every use site. Annotating a parameter as `b: Bag` and then calling `List.length b` reads as the obvious "alias means equal" promise, but the alias used to intern as its own opaque `Type::Con(bag_id, [])` and never unified with `List Int`, surfacing `T001 expected #6 (?0), got #15` at every alias use site. The infrastructure for transparent aliases already existed in `InferCtx::shallow_resolve` (which peels `Type::Alias` through to its body before unification); the missing piece was on the conversion side. `ast_type_to_ridge_type` now looks the resolved `TyConId` up in `ctx.tycon_decls` and wraps as `Type::Alias { name, body }` whenever the kind is `TyConKind::Alias`. Parametric aliases (`type Stack a = List a`) and multi-step alias chains (`type IntList = List Int; type Numbers = IntList`) remain unsupported for now — their fix needs `TyConKind::Alias` to carry the alias's own type-parameter vids and a second-pass alias-of-alias resolution after `collect_user_tycons` respectively.

### Added

- `Fs.readDir` and `Fs.isDir` open the directory-walking path that previous releases left to FFI workarounds. `Fs.readDir path` returns `Result (List Text) Text` with each entry as a bare basename (no leading path component); the underlying `file:list_dir/1` makes no order guarantees, so callers that need a deterministic ordering should sort the result. `Fs.isDir path` returns `Bool` — true iff the path resolves to a directory. Both require the `fs` capability. The new shims unblock the canonical "tree", "markdown-todo aggregator", and static-site-generator app shapes.
- `List.concat (xs: List a) (ys: List a) -> List a` fills the missing leaf for the `++` operator on lists. `crates/ridge-lower/src/operators.rs` already routed `BinOp::Concat` to `std.list.concat` when the left operand resolved to a `List`, but no stdlib symbol carried that name — `xs ++ ys` lowered cleanly to `call 'std.list':'concat'/2` and then hit `E002 no stdlib bridge for std.list.concat` at codegen. Bridged to `lists:append/2` (the 2-arg BEAM equivalent of `L1 ++ L2`). Apps that wanted the natural list-append idiom had been rendering to text with a separator or rolling a recursive fold.
- `Text.length (s: Text) -> Int` returns the number of grapheme clusters in a text value, distinct from the byte count `Text.byteSize` already provided. Bridged to `string:length/1`, so multibyte UTF-8 sequences are counted as one grapheme each — `length "café"` is `4` while `byteSize "café"` stays `5`. The 0.1.0 comment in `text.ridge` had reserved the name for this codepoint-aware semantics and the typecheck signature table already carried the corresponding arm; only the declaration and the export entry were missing.
- `Text.join (sep: Text) (xs: List Text) -> Text` interleaves a separator between elements of a list and concatenates the result. Bridged to a new `ridge_rt:text_join/2` helper that wraps `lists:join/2` + `iolist_to_binary/1`. Empty list yields the empty string; empty separator concatenates. Apps had been rolling their own via `List.fold (fn acc x -> acc <> sep <> x) "" xs`, which has a leading-separator bug on the head element.
- `P020 ReservedKeywordAsIdent` diagnostic when a reserved keyword (`init`, `state`, `on`, `actor`, …) appears in a position that expects a plain identifier — a `let` pattern, a `fn` parameter name, a lambda parameter. The historical surfaces were `P002 unexpected token \`init\` in pattern position` and `P012 tuple and constructor patterns are not allowed in top-level fn parameters`, both technically true and structurally misleading. The new diagnostic names the keyword and the position directly and tells the user to rename the binding: `reserved keyword \`init\` cannot be used as an identifier in a function parameter; rename the binding`.
- `P021 InlineRecordTypeInTypePosition` diagnostic when an inline record body `{ … }` appears where a type is expected — `-> Result { name: Text } Text`, `(x: { id: Int })`, etc. Inline record types in type positions are not part of the 0.2.x surface grammar; record types are first-class only through a named `type Foo = { … }` declaration. The historical surface was `P001 expected = but found \`{\`` or a downstream parser cascade pointing at the line after the offending one. P021 names the cause directly and points to the canonical fix: declare a named type and use it here.
- `ridge test --jobs N` / `-j N` flag pins the number of concurrent BEAM children used by the test runner. Defaults to `std::thread::available_parallelism()`; `-j 1` reproduces the pre-0.3.0 sequential behaviour and is the canonical knob for debugging output ordering; `-j 0` is treated as auto so a misconfigured invocation cannot deadlock.

### Changed

- `ridge test` now runs the discovered `pub fn test_*` functions through a counting-semaphore worker pool instead of one sequential `erl` process at a time. Tests are independent (each is a pure `Result Unit Text` function and the BEAM child it runs in is isolated by construction), so there is no correctness reason to serialise them. A dx-test suite with 11 cases that previously wall-clocked at ~2.2 seconds now finishes in ~540 ms on an 8-core box. Output stays in input order (per-test `ok` / `FAIL` lines, the trailing summary, and the BoolDeprecated migration banner all match the slice index, not completion order) so diff-based assertions and existing integration tests keep matching. Validation-failure classifications (`ArityInvalid`, `CapabilityForbidden`, `InvalidReturnType`) and the `BoolDeprecated` notice are emitted up front before any worker spins up.

## [0.2.3] - 2026-05-24

### Fixed

- Unresolved identifier errors no longer ship with a misleading `T999 internal type error / This is a compiler bug. Please report it.` companion. The typechecker used to re-report any `Expr::Ident` that the resolver had already rejected, framing a known unresolved-name as a compiler invariant violation; for users hitting `not x` or any other shadowed-prelude shorthand, the screen showed both a clean `R010` *and* a "report a bug" message about the same name. The typechecker now absorbs the unresolved-Ident silently — the resolver's `R010` (with its suggestion list) is the canonical user-facing diagnostic for that path.
- The "did you mean?" list for an unresolved identifier now leads with the well-known prelude-shorthand a developer most likely meant. Bare `not`, `and`, `or` get `Bool.not` / `Bool.and` / `Bool.or` ahead of the Levenshtein-noise suggestions; bare `print` / `println` get `Io.println`. The base Levenshtein candidates still follow. Previously the suggestion list for `not` was `Int / Io / Set` — none of them helpful, and the qualified `Bool.not` was too far away in edit distance to surface.
- `P009 non-associative chain` no longer fires on `(arith) <comparison> rhs` expressions such as `a + b == c` or `acc + rej != total`. The chain detector compared `non_assoc_level(prev_op)` against `non_assoc_level(op)`, but `non_assoc_level` ignored its argument and returned `0` unconditionally — so any `Binary` left-hand side followed by a non-associative comparison reported `P009`, with a misleading "operator `!=` cannot be chained" message that pointed nowhere near the actual code. The detector now requires the previous op to itself be non-associative before applying the level check; legitimate chains like `a == b == c`, `a < b < c`, and the cross-level `a < b == c` continue to error.
- `Text.replace from to s` now replaces every occurrence of `from`, not just the first. The public bridge in `crates/ridge-stdlib/stdlib/text.ridge` used to call `binary:replace/4` with an empty options list, which Erlang interprets as first-occurrence-only; the function name promises global semantics and matches what users coming from Python's `str.replace`, JavaScript's `replaceAll`, Rust's `str::replace`, or Go's `strings.ReplaceAll` expect. The bridge now routes through `ridge_rt:text_replace_all/3` (which already passes `[global]` and is the same shim used by `Text.split`), so the canonical pipeline `s |> Text.replace "\n" " " |> Text.replace "\t" " "` collapses every newline and every tab as intended. Two regression tests pin the multi-occurrence and pass-through cases.
- `Net.Http.get` / `post` / `put` / `delete` now work end-to-end against HTTPS URLs and real-world APIs. Three bugs in the client path were resolved together in `ridge_rt`:
  - `application:ensure_all_started(ssl)` is invoked alongside `inets`, so the first `https://` request no longer crashes with `{failed_connect, [{inet, [inet], ssl_not_started}]}`.
  - The success path returns `{ok, #{status => …, body => …}}` and the error path returns `{error, #{code => …, message => …}}` — atom-keyed maps that match the Ridge `Response` and built-in `Error` records. The previous wire emitted `{response_record, S, B}` and `{error_record, C, M}` tagged tuples, which crashed any caller touching `resp.status` or `e.message` with `badmap`. (Same root cause as the `http_listen` server-side fix in 0.2.2.)
  - A default `User-Agent: ridge-lang/0.2` header is sent on every request. `httpc`'s built-in `User-Agent: httpc/X.Y` is rejected by several production APIs (GitHub returns HTTP 403 "User-Agent header required"), so the default would not get a beginner past their first real call. Custom headers remain deferred per the std.net.http scope guard.
- 0-arity stdlib symbols used as a value (not a call) now lower to a direct `M:F()` invocation instead of being wrapped in a `fun () -> M:F() end` thunk. `Map.empty`, `Set.empty`, and `List.empty` are typed as values (`Map k v`, `Set a`, `List a`) but the codegen always emitted a function reference, so `state table = Map.empty` leaked the fun into the state map and `Map.size Map.empty` crashed with `badmap`. The lowering now short-circuits the zero-arity case; higher-arity stdlib references continue to receive the fun wrapper they need to round-trip through `apply/2`. Bare `state table = Map.empty` is the idiomatic form again.
- `Text.split "" str` returns the list of graphemes in `str` instead of crashing the BEAM with `badarg`. The bridge delegated straight to `binary:split/3`, which rejects an empty pattern; the runtime shim now branches on `<<>>` and walks the input via `string:next_grapheme/1`, rebuilding each cluster as a UTF-8 binary so multi-byte characters survive. `Text.split "" ""` is `[]`. Two stdlib tests pin the empty-separator and empty-input cases.
- 0-arity stdlib bridge calls no longer trip on the dummy `Unit` argument the parser inserts for `f ()` calls. PR #71 dropped the surplus `Unit` for local 0-arity functions but missed the stdlib bridge path, so `Map.empty ()`, `Json.jNull ()`, and any other `0-arity stdlib ()` form was rejected with `E001 expects 0 args, got 1`. The codegen's `lower_call_to_stdlib` now checks the bridge target's arity and drops a single `Unit` argument when the target takes none — the symmetric counterpart to the local-fn shim. All four `BridgeTarget` variants benefit (`BeamStdlib`, `Perm`, `RidgeRuntime`, `RidgeStdlibLocal`).
- State-field defaults that fail to lower no longer silently drop the field. `lower_init_body` filtered codegen errors through `.ok()` and quietly continued with the surviving fields, so any `state X = expr` whose `expr` couldn't lower (`Map.empty ()` before the PR above, for instance) produced an init that omitted `X` entirely, and the next `maps:get(X, V_State)` crashed with `badkey: X` far from the real failure. The lowering now propagates the underlying `CodegenError` with `?`, so the user sees the actual diagnostic at compile time.
- `http_listen` parses requests and emits responses as atom-keyed maps (`#{method =>, path =>, body =>}` / `#{status :=, body :=}`) instead of tagged tuples, matching the wire shape that Ridge records lower to. Handlers can now read `req.body` and return `Response { ... }` without the runtime falling through to its `bad response: ...` 500 fallback. Validated end-to-end against `http-echo`: `curl -d "round trip" :18181/` round-trips POST / GET / DELETE bodies as expected.
- The `with`-peephole optimisation no longer rewrites a user-named record-literal RHS as a `with` update when the field reads point at a *different* record type. `forwarding_base` used to accept any local variable, so `Response { status = 200, body = req.body }` (Request → Response) was collapsed into `req with { ... }` and the type-changing field assignments silently lost — `body` ended up dropped because the peephole assumed no-op. The check now requires the base local to be one of the synthetic `__with_base_N` locals that Phase-5 `with`-lowering emits, restricting the optimisation to its intended subject. A dedicated test reproduces the http-echo shape.
- `std.net.http.respond status body` lowers to the two-argument bridge call it declares instead of being treated as a single-argument identity. The codegen had a hard-coded shortcut that assumed `respond` was a 1-arg wrapper and emitted `E001 expects 1 arg, got 2` for the canonical `respond 200 req.body` form. The shortcut is gone; the call now resolves through the bridge map, which already points at `std.net.http:respond/2`. Two tests cover bridge resolution and the full-pipeline lowering.

### CI

- `release.yml` only fires on tags that match `v[0-9]*`. Branch pushes and non-version tags no longer kick off the multi-platform artifact build, removing an entire class of CI noise after merge-without-tag pushes.

## [0.2.2] - 2026-05-24

### Added

- Diagnostic hint on `T003 arity mismatch` when the offending argument is a curried `fn x1 -> fn x2 -> … -> body` chain and the callee expects an uncurried `fn x1 x2 -> body`. The classic trigger is `List.fold (fn acc -> fn x -> acc + x) 0 xs` — Ridge supports both lambda shapes, but `List.fold` and the rest of the uncurried stdlib helpers expect the n-arg form, and the bare T003 message gave no breadcrumbs. The hint is opt-in: it only fires when the "got" side is a 1-parameter function whose return type chains through additional 1-parameter functions totalling the expected arity.
- `Json.asInt`, `Json.asFloat`, `Json.asBool`, `Json.asText`, `Json.asList`, `Json.asObject`, and `Json.isNull` — destructor wrappers that turn a `JsonValue` back into `Option Int`, `Option Text`, etc. The underlying tagged-tuple representation (`{json_int, N}`, `{json_object, M}`, …) is still wire-internal, but user code can now pattern-walk decoded JSON via these accessors without depending on cross-module visibility of the `JsonValue` constructors (which is deferred per `stdlib/json.ridge`).

### Fixed

- `ridge run` projects the `Result` returned by `main` to a process exit code instead of silently exiting 0 on `Err`. When `fn main () -> Result Unit T` (or `Result Unit Error`) returns `Err msg`, the message is written to stderr and the process exits with status 1; `Ok ()` and a bare `Unit`-typed main continue to exit 0. The runtime shim `ridge_main_runner:run/1` wraps the entry-point call and turns `{error, _}` returns into the non-zero exit; `ridge_rt`'s existing semantics are unchanged. Pipelines like `ridge run && deploy` now propagate failure end-to-end.
- Actor handlers can call top-level functions defined in their enclosing module. Each actor was emitted into its own BEAM module (`ridge_module_N_<actor>`), and the codegen rewrote calls to parent-module functions as bare local references, which `erlc` correctly reported as `undefined function …/N in handle_call/3`. Lambda lowering now inherits `actor_parent` and `letrec_locals` from the enclosing scope, and a module that declares an actor exports every `fn`/`const` (not only `pub` ones) so the actor module's qualified `call 'ridge_module_N':<fn> (…)` resolves at load time. Inlining the helper into the handler is no longer required.
- `f ()` is treated as a call with no arguments when `f` is a 0-arity function in scope. Ridge's declaration form `fn foo () -> T` lowers `foo` as `foo/0`, but the call `foo ()` was lowered as `foo/1` because the parser produces `args: [Unit]`. The lowering's `lower_static_call` now drops a single `Unit` literal when the callee is a known 0-arity local, removing the need for the `(_unit: Unit)` parameter workaround that previously cluttered idiomatic code.
- Actor handler call forms `?> name ()` and `! name ()` are accepted against handlers declared as `on name () -> T` or `on name = …`. Both surfaces (decl and call site) now produce the same wire shape — a bare `{name}` tag tuple — and the type checker treats a single `()` argument against a zero-parameter handler as no payload instead of firing a false `T003`. Restores symmetry with the regular fn case fixed in 0.2.1.
- `Float / Float` inside actor handler bodies lowers to `erlang:'/' /2` instead of `erlang:div/2`. The arithmetic-dispatch logic in `ridge-lower` reads each operand's type from `node_types` to decide between the Int and Float stdlib families, but actor handler bodies were never visited by `infer_expr`, so the side-table was empty for sub-expressions and the dispatch fell back to the Int default — making every Float division crash the handler with `badarith` at runtime. Type-checking now runs over each handler body with state fields and parameters bound, populating `node_types` for handler-internal expressions. As defence in depth, the binop lowering also consults the right-hand-side type and a conservative structural check for Float literals and `Float.*` calls before defaulting to Int.

### Docs

- `examples/rate_limiter.ridge` initialises `lastRefill` with `Time.now ()` instead of `Time.epoch ()`. The previous form computed an initial elapsed time of half a century, which the refill arithmetic still handled correctly but obscured the intended algorithm. The result banner also uses ASCII dashes instead of U+2500 box-drawing characters so the example's stdout is stable across console encodings.

## [0.2.1] - 2026-05-23

### Added

- Diagnostic `R023` when a project source tree contains legacy `.rg` files, with a `git mv` renaming hint. Affects all build, check, run, test, and fmt entry points.
- `Int.rem`, `Int.mod`, and the `%` operator wired through `BinOp::Mod` to `std.int.mod`. `Int.rem` is the BEAM truncating remainder (same sign as the dividend); `Int.mod` is mathematical modulo (same sign as the divisor) and matches the canonical FizzBuzz idiom `match n { m when (m % 15) == 0 -> ... }`.
- `Int.pow` and the `^` operator. `^` already had a precedence and a `BinOp::Pow` lowering target in the compiler, but `std.int` exposed no `pow` symbol, so any user program writing `x ^ y` failed at codegen with `E002 NoStdlibBridge`. `pow` is implemented via repeated squaring; negative exponents truncate to `0` to keep the result in `Int`.

### Fixed

- `compile_stdlib_beams` no longer silently emits zero `.beam` files on machines other than the build host. The 0.2.0 binary embedded `env!("CARGO_MANIFEST_DIR")` (a path on the GitHub Actions runner) as the stdlib source directory; on every other machine the path was missing and the bundling pass failed quietly. Any program calling a Ridge-bodied stdlib function — `List.head`, `Option.withDefault`, `Float.parse`, … — crashed at runtime with `undef`. The stdlib sources are now embedded via `include_str!` at compile time and unpacked into the workspace's `OUT_DIR` on every build; bundling failures are surfaced loudly instead of being swallowed.
- `ridge-lsp` no longer advertises `diagnosticProvider` in its `initialize` response. The server emits diagnostics by `client.publish_diagnostics(...)` (push) only and never implemented the pull side, so VS Code logged a `Method not found (-32601)` for every document open and change. The capability is removed; VS Code falls back to push and the error log clears.
- `Float.parse` returns `None` instead of crashing the BEAM with `badarg` when handed an integer-shaped string like `"100"`. The wrapper now goes through `ridge_rt:float_parse/1`, which tries `binary_to_float/1` first and falls back to `float(binary_to_integer/1)` before reporting `None`.
- T017 `RedundantPattern` no longer fires on arms that carry a `when` guard. The exhaustiveness algorithm in `crates/ridge-typecheck/src/exhaustiveness.rs` now skips guarded arms in both the T016 coverage matrix and the T017 prefix matrix, matching Maranget's algorithm. The previous behaviour rejected every canonical guarded `match` (e.g. `match n { m when (m % 15) == 0 -> "FizzBuzz" ; m when (m % 3) == 0 -> "Fizz" ; ... }`) as redundant.
- Non-BIF calls in `when` guards no longer make `erlc` reject the generated Core Erlang. Guards that contain calls outside the BEAM guard-BIF whitelist — e.g. `m when (m % 15) == 0`, which lowers through `std.int:mod/2` — are lifted out of clause-guard position into a nested `case` chain. The whitelist now matches the OTP reference manual exactly, so non-guard `erlang:*` functions (`integer_to_binary`, `list_to_binary`, …) that previously slipped past the loose `module == "erlang"` check are correctly routed through the lift path too.
- Actor handlers invoked via `!` (cast) no longer drop the side-effecting expressions in their body. `lower_handler_body_for_cast` ignored the leaf value when wrapping the `{noreply, V_State}` tuple, so every `Io.println`, `partner ! msg`, and non-assign call disappeared from `handle_cast/2` (state mutations survived because they thread through `V_State<n>` SSA). The wrap now sequences the leaf via `Do { first: val, then: noreply }`, mirroring the `?>` (ask) path.
- `partner ! handler arg1 arg2` now sends `{handler, arg1, arg2}` instead of `{''}`. The lowering of `Expr::Send` only recognised a bare `Expr::Ident` as the handler name and hard-coded `args: Vec::new()`, so every send with arguments emitted an empty 1-tuple that no receiver could pattern-match against. `unfold_send_message` peels the `Call { callee: Ident, args }` shape the parser produces and propagates the args through `IrExpr::Send`.
- Reads of an actor state field that follow a `<-` assign in the same handler invocation now see the new value. Before, `count <- count + 1; Io.println $"count = ${count}"` lowered the second `count` against the pre-assign `V_State`, so the print reported the stale value; the `received == N` checks in collector-style actors silently never matched. Codegen now tracks the current state SSA index on the local scope and retargets `IrExpr::Local { name: "__state" }` to the latest `V_State<n>` after every assign, propagating the per-arm result back to the outer scope after a `Match`.
- `ridge run` streams the BEAM program's stdout to the terminal as it is produced instead of buffering the whole pipe and dumping it at exit. Long-running programs, anything with a `Time.sleep`, and any non-trivial actor flow previously looked like a hang followed by a single output dump. Stdout is now inherited; stderr stays piped so `RunError::ErlExitNonZero` can still surface BEAM crash dumps and warnings.

### Refactor

- `lift_guarded_match` hoists the remaining-arms expression into a `let V_LiftedRest<depth> = fun () -> <rest> end` thunk and replaces the duplicated fall-through references with `apply V_LiftedRest<depth> ()`. The previous shape cloned the rest into both the guard-case wildcard and the outer wildcard, so a chain of `N` lifted arms produced `2^N` copies of the deepest fall-through body.
- Stdlib per-tier scratch workspaces are managed by `tempfile::TempDir`. The directory is removed on every `compile_tier` exit (success, `Err`, or panic), eliminating the `/tmp/ridge_stdlib_tier*_<pid>/` orphans that the old manual cleanup left behind whenever discover, resolve, typecheck, or lower returned `Err`.

### Docs

- `docs/tutorial.md` Troubleshooting section gains a Windows entry covering `chcp 65001`. `Io.println` writes UTF-8 to stdout, but the default Windows console codepage is `cp1252` on most English/Spanish installs, so non-ASCII output rendered as mojibake (`°` → `Â°`, `é` → `Ã©`). The new entry documents both the per-session `chcp 65001` and the system-wide *Use Unicode UTF-8 for worldwide language support* toggle.

### Internal

- `crates/ridge-driver/tests/integration.rs` serialises the five `erl`-touching tests behind a module-level `Mutex` so the PATH-clearing `run_missing_erlang` test no longer races with parallel siblings that spawn `erl`. The earlier workaround — moving the related test to its own binary file — stays in place as defence-in-depth.

## [0.2.0] - 2026-05-20

First public release. Ridge is installable on Linux, macOS, and Windows
via signed prebuilt binaries; the VS Code extension is on the Marketplace
as `ridge-lang.vscode-ridge`.

### Added

- VS Code extension published to the Marketplace as
  [`ridge-lang.vscode-ridge`](https://marketplace.visualstudio.com/items?itemName=ridge-lang.vscode-ridge).
  Install with `code --install-extension ridge-lang.vscode-ridge` on any
  platform; first publish is v0.2.0. Three-platform install attestation
  in [`docs/marketplace-attestation.md`](docs/marketplace-attestation.md).
- VS Code extension prepared for Marketplace publication: Ridge brand
  icon (128×128 PNG with SVG vector source traced from the master),
  `galleryBanner` and `keywords` metadata, `homepage` / `bugs` / `license`
  fields, and an `Apache-2.0` `LICENSE` shipped inside the extension
  package. Extension version bumped from `0.1.0` to `0.2.0` to track the
  language release. Extension README rewritten as a Marketplace listing.

### Changed

- **BREAKING:** Source-file extension renamed from `.rg` to `.ridge`. Resolves a registry collision with Rouge on GitHub Linguist and avoids ambiguous syntax highlighting on github.com. Existing projects must rename their `.rg` files to `.ridge` and update `entry = "src/Main.rg"` in `ridge.toml` to `entry = "src/Main.ridge"`; the CLI no longer recognises `.rg` files.
- Install scripts no longer hardcode the expected version. Both `install.sh` and `install.ps1` now derive the version they validate against from `RIDGE_VERSION` (release-download path) or from `Cargo.toml` (cargo-install path). Future release cuts only need to bump `Cargo.toml` line 6 plus the resulting `Cargo.lock` regeneration; the eight hardcoded version strings the scripts previously carried are gone.

### CI

- `.github/workflows/vscode-publish.yml` packages the extension on every PR touching `tools/vscode-ridge/**` and publishes to the Marketplace via manual `workflow_dispatch` with a `publish` checkbox. The `VSCE_PAT` secret must be configured under repo settings before the first dispatched publish.
- `install-smoke.yml` gains `pull_request` (paths-filtered to `tools/install/**`, `Cargo.toml`, `Cargo.lock`, and itself) and `workflow_dispatch` triggers so install-script changes validate on Linux, macOS, and Windows before merging instead of only at release-publish time.

## [0.2.0-rc4] - 2026-05-18

Release candidate adding Sigstore keyless signing for release artifacts and
opportunistic signature verification in the install scripts. Integrity guarantees
remain SHA256-anchored when `cosign` is unavailable.

### Added

- Sigstore keyless signing in `release.yml`: every release archive is signed with `cosign sign-blob --yes --bundle`, producing a `.cosign.bundle` sidecar (signature, certificate, and Rekor transparency-log entry) uploaded next to the archive and its SHA256
- `install.sh` and `install.ps1` opportunistically download the `.cosign.bundle` and, when `cosign` is on PATH, verify it with `cosign verify-blob` pinned to the `ridge-lang/ridge` release workflow identity and the GitHub Actions OIDC issuer
- "Verifying release signatures manually" section in `tools/install/README.md` with the full `cosign verify-blob` recipe

### Security

- Release artifacts are now cryptographically signed and logged to the Rekor public transparency log, providing tamper-evident provenance in addition to SHA256 integrity
- Installer pins the verification identity to `https://github.com/ridge-lang/ridge/.github/workflows/release.yml@refs/tags/v*` and the OIDC issuer to `https://token.actions.githubusercontent.com`, so a signature minted by any other workflow or fork is rejected

### Changed

- New advisory codes in the installer output: `R055` when `cosign` is not on PATH (signature check skipped, SHA256 still enforced) and `R056` when `cosign verify-blob` fails (installation aborts)
- `release.yml` job permissions now include `id-token: write` so the runner can mint the OIDC token Sigstore exchanges for a short-lived signing certificate

## [0.2.0-rc3] - 2026-05-18

Release candidate cut to align release artifacts with the install-script
fixes landed in rc2. The rc2 binaries predated `ridge-lsp --version`,
which broke the cross-platform install-smoke verification.

### Added

- Install-smoke CI workflow validating `install.sh` / `install.ps1` end-to-end on Ubuntu, macOS, and Windows on every published release
- `ridge-lsp --version` flag for parity with `ridge --version`
- Post-install verification: both installers now confirm `ridge-lsp` and `ridge` report matching versions

### Fixed

- `install.sh` no longer exits silently when invoked via `curl … | sh` in CI. Root cause: the script's Erlang prerequisite check (`erl -noshell -eval …`) reads stdin, and when bash itself was reading the script from stdin, `erl` consumed the still-unread bytes and bash hit EOF before printing anything. Smoke workflow now downloads to a file and runs `bash -x` on it.
- `install.ps1` `exit N` calls inside `iex`/scriptblock no longer kill the host PowerShell session. Refactored to `throw` + `return` wrapped in `& { ... }` with try/catch that propagates `$LASTEXITCODE`.
- `install.ps1` no longer fails under `iwr | iex` due to `param()` blocks or UTF-8 BOM. Options now come from env vars (`$env:RIDGE_DRY_RUN`, etc.) and the file is BOM-free.
- macOS x86_64 release artifact builds via cross-compile from the `macos-14` (M1) runner instead of the deprecated `macos-13` image
- Windows install: `ridge-lsp.exe` extraction no longer fails when an existing VS Code LSP child has the binary locked (pre-flight stop + `Test-WriteAccess`)

## [0.2.0-rc2] - 2026-05-17

First release built by the cross-platform release pipeline. Superseded by rc3 — its `ridge-lsp` binary lacked the `--version` flag, breaking the smoke workflow's verify step.

## [0.2.0-rc1] - 2026-05-17

Initial public release candidate.

### Added

- Typed functional language for the BEAM with Hindley-Milner inference and row polymorphism
- Nine first-class capabilities (`io`, `fs`, `net`, `time`, `random`, `env`, `proc`, `spawn`, `ffi`) visible in every function signature
- Actor-first concurrency with mutable state confined to actors
- Compiler to BEAM bytecode via Core Erlang
- LSP server with diagnostics and correct file attribution
- Command-line tooling: `ridge run`, `ridge test`, `ridge fmt`, `ridge repl`, `ridge new`
- Workspace model with `git` and `path` dependencies
- VS Code extension (TextMate grammar + LSP client)
- Standard library: `bool`, `cli`, `env`, `float`, `fs`, `int`, `io`, `json`, `list`, `map`, `net.http`, `option`, `proc`, `random`, `text`, `time`
- Apache-2.0 licensed

[Unreleased]: https://github.com/ridge-lang/ridge/compare/v0.3.0-rc4...HEAD
[0.3.0-rc4]: https://github.com/ridge-lang/ridge/compare/v0.3.0-rc3...v0.3.0-rc4
[0.3.0-rc3]: https://github.com/ridge-lang/ridge/compare/v0.3.0-rc2...v0.3.0-rc3
[0.3.0-rc2]: https://github.com/ridge-lang/ridge/compare/v0.3.0-rc1...v0.3.0-rc2
[0.3.0-rc1]: https://github.com/ridge-lang/ridge/compare/v0.2.13...v0.3.0-rc1
[0.2.13]: https://github.com/ridge-lang/ridge/compare/v0.2.12...v0.2.13
[0.2.12]: https://github.com/ridge-lang/ridge/compare/v0.2.11...v0.2.12
[0.2.11]: https://github.com/ridge-lang/ridge/compare/v0.2.10...v0.2.11
[0.2.10]: https://github.com/ridge-lang/ridge/compare/v0.2.9...v0.2.10
[0.2.9]: https://github.com/ridge-lang/ridge/compare/v0.2.8...v0.2.9
[0.2.8]: https://github.com/ridge-lang/ridge/compare/v0.2.7...v0.2.8
[0.2.7]: https://github.com/ridge-lang/ridge/compare/v0.2.6...v0.2.7
[0.2.6]: https://github.com/ridge-lang/ridge/compare/v0.2.5...v0.2.6
[0.2.5]: https://github.com/ridge-lang/ridge/compare/v0.2.4...v0.2.5
[0.2.4]: https://github.com/ridge-lang/ridge/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/ridge-lang/ridge/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/ridge-lang/ridge/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/ridge-lang/ridge/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/ridge-lang/ridge/compare/v0.2.0-rc4...v0.2.0
[0.2.0-rc4]: https://github.com/ridge-lang/ridge/releases/tag/v0.2.0-rc4
[0.2.0-rc3]: https://github.com/ridge-lang/ridge/releases/tag/v0.2.0-rc3
[0.2.0-rc2]: https://github.com/ridge-lang/ridge/releases/tag/v0.2.0-rc2
[0.2.0-rc1]: https://github.com/ridge-lang/ridge/releases/tag/v0.2.0-rc1
