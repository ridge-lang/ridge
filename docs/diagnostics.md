# Diagnostic codes

Every error and warning the compiler reports carries a code — `T031`, `P012`, `C001`.
The code is the stable handle on that failure: it survives rewording, a search box and a
CI filter can both match on it, and it is what this page is indexed by.

To read one from the terminal instead:

```text
ridge explain T031
```

`ridge explain --list` prints the whole table. Every code answers — a code with no entry
cannot ship, because the registry census fails the build first.

The leading letter groups the codes; it is not a compiler phase. `P` covers the parser
and the package layer, `T` the type checker and the standard library, and the numbers
within a letter are shared rather than split between them.

*Generated from `crates/ridge-diagnostics/src/registry.rs`. Edit the registry, not this
page.*

## `C` codes

Declared by `ridge-cli`, `ridge-driver` and `ridge-fmt`.

### C001

No `ridge.toml` with a `[workspace]` table was found at or above the search root.

### C002

A member listed in `[workspace] members` has no on-disk directory or no `ridge.toml`.

### C004

An OTP binary (`erl`, `erlc`) is not on `PATH`.

### C005

`--member` named a member that does not exist in the workspace.

### C006

No `app` or `service` member found in the workspace (for `ridge run`).

### C007

`--member` names a `library` member, which is not executable.

### C008

`--observer` needs the Erlang cookie, but none was given and none was found on disk.

### C010

The Ridge standard library could not be compiled to BEAM.

### C011

`--watch` requested but multiple executable members exist and `--member` was not specified.

### C012

An output file could not be written.

### C013

The BEAM process could not be spawned.

### C014

Codegen produced no BEAM module to run.

### C015

The runtime started, but the OS stopped reporting on it.

### C101

The source could not be parsed.

### C102

A `<paths>` argument supplied to `ridge fmt` does not exist.

### C103

A file could not be read from or written to during `ridge fmt`.

### C104

`--check` mode found files that would be reformatted.

### C105

`ridge fmt` encountered a file with the legacy `.rg` extension.

### C201

The project name given to `ridge new` is not a portable directory name.

### C202

`ridge new <name>` refused because `<name>/` already exists in the current directory.

### C203

The project name is reserved by the Ridge toolchain (`std`, `test`, `core`).

### C204

`ridge init` refused: the directory holds files other than `.git/` and `.gitignore`.

### C205

`ridge init` could not read the current working directory.

### C301

A `pub fn test_*` function has arity != 0.

### C302

A `pub fn test_*` function declares the `ffi` capability.

### C303

A discovered test returns `Bool` rather than `Result Unit Text`.

### C304

A test was found by its `test_` prefix rather than `@test`.

### C305

A test declares a return type other than `Result Unit Text`.

### C306

A test declares no return type, so the runner cannot check it.

### C401

`<src_root>/migrations/Model.ridge` is missing.

### C403

The model failed to compile.

### C404

Generating the migration failed for a reason that is not the user's to fix.

### C405

The name given to `ridge migrate add` is not valid.

### C406

A database environment variable the command needs is missing or empty.

### C407

`ridge migrate apply` reached the database, but the migration run failed.

### C408

`ridge migrate status` could not read the set of applied migrations.

### C409

`ridge migrate rollback` reached the database, but the rollback failed.

### C501

The file watcher could not be created.

### C502

The workspace directory could not be watched for changes.

### C503

The REPL session could not be started.

### C504

The watch loop's shared state was left unusable by a thread that panicked while holding it.

### C505

A watched rebuild could not be restarted, and neither could its placeholder.

### C601

`ridge explain` was given something that is not a code the compiler can emit.

## `E` codes

Declared by `ridge-codegen-erl`.

### E001

The lowered IR has a shape codegen cannot emit.

### E002

Stdlib bridge missing for symbol `X`.

### E003

`erlc` not found on PATH.

### E004

`erlc` rejected the emitted `.core` (with stderr surfaced).

### E005

Output directory not writable.

### E006

Module name collision (two Ridge modules mangle to the same BEAM module).

### E007

An unresolved type reached a codegen site that requires a concrete one.

### E008

Capability erasure audit found a `Capability` token in emitted Core Erlang.

### E101

`erlc` toolchain version below OTP 26 minimum.

### E102

`erlc` produced unexpected output (parse error in our `.core`).

### E201

`erlc` is not available on `PATH` (or the given override path).

### E202

`erlc` rejected one of the emitted `.core` files or the generated shim.

### E203

An I/O error occurred writing intermediate files or the final artefact.

### E204

The specified `main` module was not found in `modules`.

### E205

A workspace member marked as a `library` (no entry point) was passed.

### E206

The `main` function's arity is not 0 or 1.

### E207

Zip archive construction failed.

## `L` codes

Declared by `ridge-lexer` and `ridge-lower`.

### L001

A tab character was found in source code outside a string literal.

### L002

A string literal was opened but never closed before end-of-line or EOF.

### L003

An interpolated string (`$"..."`) was opened but never closed.

### L004

A block doc-comment (`---` ...

### L005

An unrecognised escape sequence inside a string literal or interpolated text segment (e.g.

### L006

A `\u{{...}}` escape sequence was syntactically present but its value could not be decoded.

### L007

A dedent returned to a column that matches no previously pushed indentation level.

### L008

A numeric literal had a leading underscore where none is allowed (e.g.

### L009

A numeric literal had a trailing underscore (e.g.

### L010

A base-prefix literal had no digits after the prefix (e.g.

### L011

An unexpected character that belongs to no token class.

### L012

The first non-blank line of the file is indented (column > 0).

### L013

A triple-quoted string `"""` had non-whitespace content on the opening line.

### L014

An interior line of a triple-quoted string is indented less than its closing delimiter.

### L015

A triple-quoted string or raw string was opened but EOF was reached before the matching closing delimiter.

### L016

A statement terminator carried over from a C-family language.

### L101

Pipe right-hand side is not a valid call or section shape.

### L102

Pipe right-hand side shape could not be classified.

### L103

`?` propagation used outside any `Option`- or `Result`-typed scope.

### L104

Two propagation operators nest in a structurally ambiguous way.

### L105

A `try` block has an empty body.

### L106

A `when` guard appears outside a `match` arm, where it cannot be desugared.

### L107

String interpolation reached a value with no `ToText` coercion to synthesise.

### L108

`with` applied to a value whose type is not a record.

### L109

A refutable sub-pattern appears after the variable-length part of a slice pattern.

### L110

An integer literal does not fit in the `Int` range (`i64`).

### L997

An unsolved type variable reached the IR, indicating incomplete typecheck output was passed to the lowerer.

### L998

A capability variable reached the IR.

### L999

Catch-all internal lowering invariant violation.

## `M` codes

Declared by `ridge-manifest`.

### M001

The manifest TOML could not be parsed.

### M002

The workspace manifest is missing the `[workspace]` table.

### M003

A project manifest is missing the `[project]` table.

### M004

A workspace member directory has no `ridge.toml` project manifest.

### M005

A workspace `members` glob pattern is invalid.

### M006

A required field is absent from a manifest table.

### M007

The `kind` field contains an unrecognised project kind string.

### M008

A `forbid` rule entry is syntactically or semantically invalid.

### M009

A dependency entry uses an unrecognised `kind` value.

### M010

Two workspace members declared the same project name.

### M011

An unrecognised capability name was used in a manifest.

### M013

A dependency names a project not present in the workspace.

### M014

A project `exports` pattern string is not a valid glob.

### M015

A manifest references a workspace-level dependency that is not declared in `[workspace.dependencies]`.

### M016

A Git dependency specifies more than one of `tag`, `branch`, or `rev` simultaneously.

### M017

A relative path dependency escapes the workspace root.

### M018

Hex dependencies are not supported; use a path or git dependency.

### M019

An unrecognised key appeared in a manifest table.

### M020

A `[project.exports].public` pattern matched no symbol in the module's top-level table.

### M021

`entry` names a file that is not a module of the project.

### M022

The module named by `entry` declares no `main`.

## `P` codes

Declared by `ridge-parser` and `ridge-pkg`.

### P001

The parser expected a specific token but found something else.

### P002

An unexpected token was encountered with no specific expectation.

### P005

A type annotation is required but was absent.

### P006

An `Indent`, `Dedent`, or `Newline` token appeared in a context where the layout invariant was violated.

### P009

A non-associative operator was chained without parentheses.

### P012

A top-level function parameter was a tuple or constructor pattern.

### P013

A language feature is reserved but deferred to a future version.

### P014

An `INDENT`/`DEDENT` block contained no statements.

### P019

A doc comment sits where it cannot attach to any declaration.

### P020

A reserved keyword (e.g.

### P021

An inline record type `{ … }` in type position is syntactically malformed.

### P022

`mailbox bounded N` was declared without an overflow policy.

### P023

`mailbox bounded N` was given a capacity that is not a positive `i64` literal.

### P024

A list pattern contains more than one `..` rest element.

### P025

Reserved; previously used for suffix/middle rest (now supported).

### P026

A suffix or middle element in a list pattern is a refutable sub-pattern (literal, constructor, tuple, …).

### P027

`@test` was not given a string-literal argument.

### P028

Syntax nested deeper than the parser's recursion limit.

### P030

A `class` declaration is structurally malformed.

### P031

An `instance` declaration is structurally malformed.

### P032

`opaque` was applied to a type alias.

### P033

A `let … in …` expression was written.

### P034

A match arm used `if` to introduce its guard.

### P035

Record update was written `{ record with … }` (the OCaml/Elm/F# spelling).

### P036

A versioned type reference (`Name@N`) appeared outside a `migrate` signature.

### P037

An expression is followed directly by `[`, the C-family index spelling.

### P038

`!` was written in expression-atom position, the C-family boolean-negation spelling.

### P039

A `match` scrutinee was followed directly by `{`, the Rust-style brace-delimited arm block.

### P040

A `for` or `while` loop was written.

### P101

Path dependency's `ridge.toml` is missing or the path does not exist.

### P102

A `ridge.toml` was found but could not be parsed.

### P103

Cache root could not be determined (no home directory available).

### P104

`GitRev::Commit` was encountered; commit-pinned git dependencies are not yet supported in 0.1.0.

### P201

`git clone` exited non-zero due to network failure.

### P202

Cache directory write failed (disk full or permission denied).

### P203

Git URL uses SSH scheme (`git@…` or `ssh://…`), which is not supported in 0.1.0 (HTTPS-only).

### P204

A git dependency tracks a mutable branch rather than a pinned tag.

### P205

`git` binary not found on `PATH`.

### P206

Circular dependency detected during resolution.

### P207

The requested tag or branch does not exist on the remote.

### P208

Installed `git` is older than the minimum required version 2.20.

### P209

`git --version` output could not be parsed (exotic distro or custom build).

### P210

A registry-based version dependency was encountered.

### P999

The lexer's bracket-suppression invariant was violated — a compiler bug, not yours.

## `R` codes

Declared by `ridge-resolve`.

### R001

No `ridge.toml` workspace manifest was found at the given path.

### R002

The same fully-qualified module name was declared more than once.

### R003

A cycle was detected in the import graph.

### R004

A module imports itself.

### R005

The same name was declared more than once at the top level of a module.

### R006

An import path could not be resolved to any known module.

### R007

A module in one project tried to import a non-exported symbol from another project.

### R008

A named import item could not be found in the target module.

### R009

A name is referenced outside its declared visibility scope.

### R010

An identifier could not be resolved; suggestions are provided if available.

### R011

The same local variable name was bound more than once in the same scope.

### R012

A qualified name (e.g.

### R013

A `forbid` architectural rule was violated.

### R014

A reference to a standard-library symbol that does not exist.

### R015

A capability is used but denied by the project or workspace manifest.

### R016

A capability is declared on a function but the project's `capabilities_allow` list does not include it.

### R017

A local binding shadows an actor state field in the same scope.

### R019

An unrecognised capability keyword was encountered.

### R020

Capabilities were attached to a declaration that does not take them.

### R021

An actor state type has neither a `default` nor an `init`, so it can never be built.

### R022

An `@ffi` attribute was used outside the `crates/ridge-stdlib/` crate.

### R023

A source file with the legacy `.rg` extension was found.

### R024

Two distinct typeclasses declare the same method name, making a bare reference to that name ambiguous.

### R025

A constructor of an `opaque` type was used to build a value outside the module that declares the type.

### R026

A constructor of an `opaque` type was matched in a pattern outside the module that declares the type.

### R027

The alternatives of an or-pattern `p1 | p2 | …` bind different variables.

### R028

A `type` or union constructor reuses a name the prelude keeps in scope everywhere.

### R029

An actor declares a singleton member (`init`, `mailbox`, `terminate`, or `onDown`) more than once.

### R999

Two AST nodes were assigned the same `NodeId` (signals a compiler bug, not a user error).

## `T` codes

Declared by `ridge-stdlib` and `ridge-typecheck`.

### T001

Type mismatch at an annotation or binding site.

### T002

Type mismatch on a specific argument in a function call.

### T003

Wrong number of arguments at a call site.

### T004

A required field is absent in a record construction expression.

### T005

A field name used in a record construction does not exist on the type.

### T006

The `with` expression is applied to a non-record type.

### T007

A pattern does not match the scrutinee's type.

### T009

A constructor is applied to the wrong number of arguments.

### T010

Unification would create an infinite type.

### T011

A chain of type aliases forms a cycle.

### T013

A declaration is used at a second type inside its own definition, and its signature does not annotate every parameter and the return type.

### T014

The capability set inferred from a function body exceeds its declared annotation.

### T015

A message name sent to an actor does not match any declared `on` handler.

### T016

A `match` expression does not cover all constructors / patterns.

### T017

A match arm is unreachable because an earlier arm already covers it.

### T018

A function calls another with higher capabilities than itself declares.

### T019

An actor handler declares capabilities not present in the actor's own declared capability set.

### T020

The `!` send operator is applied to a non-`Handle` value.

### T021

The `?>` ask operator is applied to a non-`Handle` value.

### T022

A non-`Unit` value is silently discarded at statement level.

### T023

A type variable cannot be resolved — the user must add a type annotation.

### T024

A capability variable escapes into a user-visible type (D057).

### T025

A `spawn` expression passes the wrong number of `init` arguments.

### T026

The expression supplied to `?> ... timeout <expr>` is not `Int`.

### T027

An actor declares `mailbox bounded N drop oldest`.

### T028

A constructor-less record pattern omits fields and has no `..` rest pattern.

### T029

A constrained function is called with a type that has no instance for the required class.

### T030

A class constraint's type variable is ambiguous: neither resolved nor generalised.

### T031

An instance is declared outside both the class's module and the type's module.

### T032

A second `instance C T` is declared for the same `(C, T)` pair.

### T033

`instance C T` is declared but a required superclass instance is absent.

### T034

A type has both an auto-promoted `pub fn toText` and an explicit `ToText` instance.

### T035

The class hierarchy forms a cycle (e.g.

### T036

A field of an `opaque` type was reached (`.field` or `with`) from outside the module that declares the type.

### T037

Two record rows disagree on their fixed field sets and cannot be unified.

### T038

An `instance` head supplies the wrong number of type atoms for its class.

### T039

A quoted predicate references a field that is not a column of its entity.

### T040

A quoted predicate uses a form the quotation layer does not support yet.

### T041

The two sides of a comparison in a quoted predicate have different types.

### T042

The entity type a quoted predicate is checked against cannot be determined at the call site.

### T043

A function parameter destructures with a pattern that does not match every value of its type.

### T044

A name is used as a constructor (in a value or pattern) but does not name one.

### T045

A functional dependency on a class names a variable that is not one of the class's type parameters.

### T046

Two instances agree on a dependency's determining types but differ on a determined one.

### T047

A full entity was supplied where a typed insert expects its `Insert` companion.

### T048

An actor callback's declared parameters do not match the shape the runtime delivers.

### T049

A versioned type reference (`User@1`) named a version the compiler has no record of.

### T050

Two `migrate` members on the same type or actor cover the same version edge.

### T051

An `instance` head has a form the dispatcher cannot key on.

### T052

An arithmetic operator was applied to operands of a concrete non-numeric type.

### T053

A top-level `fn main` declares parameters.

### T054

A field access `base.field` is applied to a non-record type.

### T055

A fully-annotated signature does not promise a class its body needs.

### T101

The `@ffi` arity doesn't match the Ridge parameter count.

### T102

The Ridge decl is missing a capability that the BEAM target requires.

### T103

The BEAM `module:name/arity` triplet is not in the audit table.

### T999

Internal type-checker invariant violation — should never reach users.

## Retired codes

These are no longer reported. They keep their page because the number outlives the
compiler that emitted it — it is still in logs, in CI filters, and in answers written
years ago — and a number is never reused, so what it meant then is what it means now.

### C003

Reported a dependency cycle among workspace members, as a debug-formatted list of the members involved. Cycle detection moved to the package layer, which names the path it found.

See [P206](#p206) instead.

### C402

Reported that `erl` and `erlc` had to be on PATH before `ridge migrate add` could run. It was `C004` under a second number, and vaguer: it never said which of the two binaries was missing.

See [C004](#c004) instead.

### M012

Reported a dependency cycle among workspace projects while reading manifests. A manifest is read on its own, so the cycle was never visible at that point; the package layer resolves the whole graph and reports it.

See [P206](#p206) instead.

### P018

Rejected a record pattern written without a leading constructor name. Bare record patterns became legal in 0.2.12, so the failure it reported stopped being one.

### T008

Reported a constructor that is not defined on the expected union type, with a did-you-mean. A name nothing declares is rejected before types exist, carrying the same suggestion; a constructor that exists but belongs elsewhere is a plain mismatch, which names both types and does not call it unknown.

See [R010](#r010) instead.

### T012

Reported that an interpolated value's type could not be converted to text, on the rule that only built-in types and records of built-in types were allowed. `ToText` became an open class in 0.2.13, so the rule it enforced stopped holding.

See [T029](#t029) instead.
