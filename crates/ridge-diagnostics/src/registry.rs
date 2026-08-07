//! The registry of every diagnostic code the compiler can emit.
//!
//! A code is the one stable handle a user has on an error: it goes in the
//! search box, the changelog entry, the CI filter. That only works while a
//! code means one thing, and while something can say what that thing is.
//!
//! Source stays authoritative about which codes *exist* - each error type's
//! `code()` decides that. This table is authoritative about what each one
//! *means*, which no `code()` can say. `tests/registry_census.rs` reconciles
//! the two in both directions, so a code cannot be added without landing here,
//! and an entry cannot outlive the variant it describes.
//!
//! # Deliberately absent: a status field
//!
//! Retired codes need a policy before they need a column: what retirement
//! means, whether the number is reusable, what someone searching a retired code
//! should read. Seeding the field by keyword got eleven codes wrong - `C203`
//! `ReservedName` and `P020` `ReservedKeywordAsIdent` are about names the
//! *program* may not use, not about codes the compiler withdrew. A field that
//! wrong is worse than no field.

/// One diagnostic code, and what it means.
///
/// `variants` and `owner` are what the census checks against source. `summary`
/// is the part a person wrote, and the reason this table exists at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CodeEntry {
    /// The code itself, e.g. `"T001"`.
    pub code: &'static str,
    /// Every error variant whose `code()` returns it.
    ///
    /// Usually one. Two variants share a code when they are the same failure
    /// reached by different paths - and the plural is what makes the other
    /// case, two unrelated failures under one number, visible here.
    pub variants: &'static [&'static str],
    /// The crate whose source declares it.
    pub owner: &'static str,
    /// One line about what went wrong in the program, not in the compiler.
    pub summary: &'static str,
}

/// Look up one code.
///
/// Case-sensitive, and the codes are upper-case: a caller that wants to accept
/// `t001` should upper-case its own input, rather than this matching loosely
/// and leaving every other caller to guess which spellings are equivalent.
#[must_use]
pub fn lookup(code: &str) -> Option<&'static CodeEntry> {
    REGISTRY
        .binary_search_by(|e| e.code.cmp(code))
        .ok()
        .and_then(|i| REGISTRY.get(i))
}

/// Every declared diagnostic code, sorted by code.
///
/// Sorted because [`lookup`] binary-searches it, and a test holds the order.
pub const REGISTRY: &[CodeEntry] = &[
    CodeEntry {
        code: "C001",
        variants: &["NoWorkspaceRoot"],
        owner: "ridge-driver",
        summary: "No `ridge.toml` with a `[workspace]` table was found at or above the search root.",
    },
    CodeEntry {
        code: "C002",
        variants: &["WorkspaceMemberMissing"],
        owner: "ridge-driver",
        summary: "A member listed in `[workspace] members` has no on-disk directory or no `ridge.toml`.",
    },
    CodeEntry {
        code: "C003",
        variants: &["WorkspaceCycle"],
        owner: "ridge-driver",
        summary: "Cyclic workspace dependency detected.",
    },
    CodeEntry {
        code: "C004",
        variants: &["ErlangNotFound"],
        owner: "ridge-driver",
        summary: "An OTP binary (`erl`, `erlc`) is not on `PATH`.",
    },
    CodeEntry {
        code: "C005",
        variants: &["UnknownMember"],
        owner: "ridge-cli",
        summary: "`--member` named a member that does not exist in the workspace.",
    },
    CodeEntry {
        code: "C006",
        variants: &["NoExecutableMember"],
        owner: "ridge-cli",
        summary: "No `app` or `service` member found in the workspace (for `ridge run`).",
    },
    CodeEntry {
        code: "C007",
        variants: &["LibraryNotExecutable"],
        owner: "ridge-cli",
        summary: "`--member` names a `library` member, which is not executable.",
    },
    CodeEntry {
        code: "C008",
        variants: &["ObserverNoCookie"],
        owner: "ridge-cli",
        summary: "`--observer` needs the Erlang cookie, but none was given and none was found on disk.",
    },
    CodeEntry {
        code: "C010",
        variants: &["StdlibBundleFailed"],
        owner: "ridge-driver",
        summary: "The Ridge standard library could not be compiled to BEAM.",
    },
    CodeEntry {
        code: "C011",
        variants: &["WatchAmbiguousMember"],
        owner: "ridge-cli",
        summary: "`--watch` requested but multiple executable members exist and `--member` was not specified.",
    },
    CodeEntry {
        code: "C012",
        variants: &["Io"],
        owner: "ridge-driver",
        summary: "An output file could not be written.",
    },
    CodeEntry {
        code: "C013",
        variants: &["SpawnFailed"],
        owner: "ridge-driver",
        summary: "The BEAM process could not be spawned.",
    },
    CodeEntry {
        code: "C014",
        variants: &["NoBeamModule"],
        owner: "ridge-driver",
        summary: "Codegen produced no BEAM module to run.",
    },
    CodeEntry {
        code: "C015",
        variants: &["WaitFailed"],
        owner: "ridge-driver",
        summary: "The runtime started, but the OS stopped reporting on it.",
    },
    CodeEntry {
        code: "C101",
        variants: &["FmtSourceUnparseable"],
        owner: "ridge-fmt",
        summary: "The source could not be parsed.",
    },
    CodeEntry {
        code: "C102",
        variants: &["FmtPathNotFound"],
        owner: "ridge-cli",
        summary: "A `<paths>` argument supplied to `ridge fmt` does not exist.",
    },
    CodeEntry {
        code: "C103",
        variants: &["FmtIoError"],
        owner: "ridge-cli",
        summary: "A file could not be read from or written to during `ridge fmt`.",
    },
    CodeEntry {
        code: "C104",
        variants: &["FmtCheckFailed"],
        owner: "ridge-cli",
        summary: "`--check` mode found files that would be reformatted.",
    },
    CodeEntry {
        code: "C105",
        variants: &["LegacyRgFile"],
        owner: "ridge-cli",
        summary: "`ridge fmt` encountered a file with the legacy `.rg` extension.",
    },
    CodeEntry {
        code: "C201",
        variants: &["InvalidProjectName"],
        owner: "ridge-cli",
        summary: "The project name given to `ridge new` is not a portable directory name.",
    },
    CodeEntry {
        code: "C202",
        variants: &["DirectoryExists"],
        owner: "ridge-cli",
        summary: "`ridge new <name>` refused because `<name>/` already exists in the current directory.",
    },
    CodeEntry {
        code: "C203",
        variants: &["ReservedName"],
        owner: "ridge-cli",
        summary: "The project name is reserved by the Ridge toolchain (`std`, `test`, `core`).",
    },
    CodeEntry {
        code: "C204",
        variants: &["DirectoryNotEmpty"],
        owner: "ridge-cli",
        summary: "`ridge init` refused: the directory holds files other than `.git/` and `.gitignore`.",
    },
    CodeEntry {
        code: "C205",
        variants: &["CwdUnreadable"],
        owner: "ridge-cli",
        summary: "`ridge init` could not read the current working directory.",
    },
    CodeEntry {
        code: "C301",
        variants: &["TestArityInvalid"],
        owner: "ridge-cli",
        summary: "A `pub fn test_*` function has arity != 0.",
    },
    CodeEntry {
        code: "C302",
        variants: &["TestCapabilityForbidden"],
        owner: "ridge-cli",
        summary: "A `pub fn test_*` function declares the `ffi` capability.",
    },
    CodeEntry {
        code: "C303",
        variants: &["BoolTestDeprecated"],
        owner: "ridge-cli",
        summary: "A discovered test returns `Bool` rather than `Result Unit Text`.",
    },
    CodeEntry {
        code: "C304",
        variants: &["PrefixTestDeprecated"],
        owner: "ridge-cli",
        summary: "A test was found by its `test_` prefix rather than `@test`.",
    },
    CodeEntry {
        code: "C401",
        variants: &["MigrateModelMissing"],
        owner: "ridge-cli",
        summary: "`<src_root>/migrations/Model.ridge` is missing.",
    },
    CodeEntry {
        code: "C403",
        variants: &["MigrateCompileFailed"],
        owner: "ridge-cli",
        summary: "The model failed to compile.",
    },
    CodeEntry {
        code: "C404",
        variants: &["MigrateInternal"],
        owner: "ridge-cli",
        summary: "Generating the migration failed for a reason that is not the user's to fix.",
    },
    CodeEntry {
        code: "C405",
        variants: &["MigrateInvalidName"],
        owner: "ridge-cli",
        summary: "The name given to `ridge migrate add` is not valid.",
    },
    CodeEntry {
        code: "C406",
        variants: &["MigrateEnvMissing"],
        owner: "ridge-cli",
        summary: "A database environment variable the command needs is missing or empty.",
    },
    CodeEntry {
        code: "C407",
        variants: &["MigrateApplyFailed"],
        owner: "ridge-cli",
        summary: "`ridge migrate apply` reached the database, but the migration run failed.",
    },
    CodeEntry {
        code: "C408",
        variants: &["MigrateStatusFailed"],
        owner: "ridge-cli",
        summary: "`ridge migrate status` could not read the set of applied migrations.",
    },
    CodeEntry {
        code: "C409",
        variants: &["MigrateRollbackFailed"],
        owner: "ridge-cli",
        summary: "`ridge migrate rollback` reached the database, but the rollback failed.",
    },
    CodeEntry {
        code: "C501",
        variants: &["WatcherStartFailed"],
        owner: "ridge-cli",
        summary: "The file watcher could not be created.",
    },
    CodeEntry {
        code: "C502",
        variants: &["WatchPathFailed"],
        owner: "ridge-cli",
        summary: "The workspace directory could not be watched for changes.",
    },
    CodeEntry {
        code: "C503",
        variants: &["ReplSessionFailed"],
        owner: "ridge-cli",
        summary: "The REPL session could not be started.",
    },
    CodeEntry {
        code: "C504",
        variants: &["WatchStateCorrupted"],
        owner: "ridge-cli",
        summary: "The watch loop's shared state was left unusable by a thread that panicked while holding it.",
    },
    CodeEntry {
        code: "C505",
        variants: &["WatchRestartFailed"],
        owner: "ridge-cli",
        summary: "A watched rebuild could not be restarted, and neither could its placeholder.",
    },
    CodeEntry {
        code: "E001",
        variants: &["IrShapeMalformed"],
        owner: "ridge-codegen-erl",
        summary: "The lowered IR has a shape codegen cannot emit.",
    },
    CodeEntry {
        code: "E002",
        variants: &["StdlibBridgeMissing"],
        owner: "ridge-codegen-erl",
        summary: "Stdlib bridge missing for symbol `X`.",
    },
    CodeEntry {
        code: "E003",
        variants: &["ErlcNotFound"],
        owner: "ridge-codegen-erl",
        summary: "`erlc` not found on PATH.",
    },
    CodeEntry {
        code: "E004",
        variants: &["ErlcRejectedInput"],
        owner: "ridge-codegen-erl",
        summary: "`erlc` rejected the emitted `.core` (with stderr surfaced).",
    },
    CodeEntry {
        code: "E005",
        variants: &["OutputDirNotWritable"],
        owner: "ridge-codegen-erl",
        summary: "Output directory not writable.",
    },
    CodeEntry {
        code: "E006",
        variants: &["BeamModuleNameCollision"],
        owner: "ridge-codegen-erl",
        summary: "Module name collision (two Ridge modules mangle to the same BEAM module).",
    },
    CodeEntry {
        code: "E007",
        variants: &["TypeErasureUnsupportedErrorSite"],
        owner: "ridge-codegen-erl",
        summary: "An unresolved type reached a codegen site that requires a concrete one.",
    },
    CodeEntry {
        code: "E008",
        variants: &["CapabilityLeakIntoCoreErl"],
        owner: "ridge-codegen-erl",
        summary: "Capability erasure audit found a `Capability` token in emitted Core Erlang.",
    },
    CodeEntry {
        code: "E101",
        variants: &["ErlcVersionTooOld"],
        owner: "ridge-codegen-erl",
        summary: "`erlc` toolchain version below OTP 26 minimum.",
    },
    CodeEntry {
        code: "E102",
        variants: &["ErlcUnexpectedOutput"],
        owner: "ridge-codegen-erl",
        summary: "`erlc` produced unexpected output (parse error in our `.core`).",
    },
    CodeEntry {
        code: "E201",
        variants: &["ErlcNotFound"],
        owner: "ridge-codegen-erl",
        summary: "`erlc` is not available on `PATH` (or the given override path).",
    },
    CodeEntry {
        code: "E202",
        variants: &["ErlcFailed"],
        owner: "ridge-codegen-erl",
        summary: "`erlc` rejected one of the emitted `.core` files or the generated shim.",
    },
    CodeEntry {
        code: "E203",
        variants: &["Io"],
        owner: "ridge-codegen-erl",
        summary: "An I/O error occurred writing intermediate files or the final artefact.",
    },
    CodeEntry {
        code: "E204",
        variants: &["MainModuleNotFound"],
        owner: "ridge-codegen-erl",
        summary: "The specified `main` module was not found in `modules`.",
    },
    CodeEntry {
        code: "E205",
        variants: &["EscriptNeedsEntry"],
        owner: "ridge-codegen-erl",
        summary: "A workspace member marked as a `library` (no entry point) was passed.",
    },
    CodeEntry {
        code: "E206",
        variants: &["EscriptMainArityInvalid"],
        owner: "ridge-codegen-erl",
        summary: "The `main` function's arity is not 0 or 1.",
    },
    CodeEntry {
        code: "E207",
        variants: &["ZipFailed"],
        owner: "ridge-codegen-erl",
        summary: "Zip archive construction failed.",
    },
    CodeEntry {
        code: "L001",
        variants: &["TabForbidden"],
        owner: "ridge-lexer",
        summary: "A tab character was found in source code outside a string literal.",
    },
    CodeEntry {
        code: "L002",
        variants: &["UnterminatedString"],
        owner: "ridge-lexer",
        summary: "A string literal was opened but never closed before end-of-line or EOF.",
    },
    CodeEntry {
        code: "L003",
        variants: &["UnterminatedInterpolation"],
        owner: "ridge-lexer",
        summary: "An interpolated string (`$\"...\"`) was opened but never closed.",
    },
    CodeEntry {
        code: "L004",
        variants: &["UnterminatedDocComment"],
        owner: "ridge-lexer",
        summary: "A block doc-comment (`---` ...",
    },
    CodeEntry {
        code: "L005",
        variants: &["InvalidEscape"],
        owner: "ridge-lexer",
        summary: "An unrecognised escape sequence inside a string literal or interpolated text segment (e.g.",
    },
    CodeEntry {
        code: "L006",
        variants: &["InvalidUnicodeEscape"],
        owner: "ridge-lexer",
        summary: "A `\\u{{...}}` escape sequence was syntactically present but its value could not be decoded.",
    },
    CodeEntry {
        code: "L007",
        variants: &["InconsistentDedent"],
        owner: "ridge-lexer",
        summary: "A dedent returned to a column that matches no previously pushed indentation level.",
    },
    CodeEntry {
        code: "L008",
        variants: &["LeadingUnderscoreLiteral"],
        owner: "ridge-lexer",
        summary: "A numeric literal had a leading underscore where none is allowed (e.g.",
    },
    CodeEntry {
        code: "L009",
        variants: &["TrailingUnderscoreLiteral"],
        owner: "ridge-lexer",
        summary: "A numeric literal had a trailing underscore (e.g.",
    },
    CodeEntry {
        code: "L010",
        variants: &["EmptyNumericLiteral"],
        owner: "ridge-lexer",
        summary: "A base-prefix literal had no digits after the prefix (e.g.",
    },
    CodeEntry {
        code: "L011",
        variants: &["UnexpectedCharacter"],
        owner: "ridge-lexer",
        summary: "An unexpected character that belongs to no token class.",
    },
    CodeEntry {
        code: "L012",
        variants: &["IndentAtTopLevel"],
        owner: "ridge-lexer",
        summary: "The first non-blank line of the file is indented (column > 0).",
    },
    CodeEntry {
        code: "L013",
        variants: &["MultilineStringOpenContent"],
        owner: "ridge-lexer",
        summary: "A triple-quoted string `\"\"\"` had non-whitespace content on the opening line.",
    },
    CodeEntry {
        code: "L014",
        variants: &["MultilineStringInsufficientIndent"],
        owner: "ridge-lexer",
        summary: "An interior line of a triple-quoted string is indented less than its closing delimiter.",
    },
    CodeEntry {
        code: "L015",
        variants: &["UnterminatedMultilineString"],
        owner: "ridge-lexer",
        summary: "A triple-quoted string or raw string was opened but EOF was reached before the matching closing delimiter.",
    },
    CodeEntry {
        code: "L016",
        variants: &["SemicolonNotUsed"],
        owner: "ridge-lexer",
        summary: "A statement terminator carried over from a C-family language.",
    },
    CodeEntry {
        code: "L101",
        variants: &["MalformedPipeRhs"],
        owner: "ridge-lower",
        summary: "Pipe right-hand side is not a valid call or section shape.",
    },
    CodeEntry {
        code: "L102",
        variants: &["UnknownPipeRhsShape"],
        owner: "ridge-lower",
        summary: "Pipe right-hand side shape could not be classified.",
    },
    CodeEntry {
        code: "L103",
        variants: &["PropagateOutsideScope"],
        owner: "ridge-lower",
        summary: "`?` propagation used outside any `Option`- or `Result`-typed scope.",
    },
    CodeEntry {
        code: "L104",
        variants: &["DoublePropagate"],
        owner: "ridge-lower",
        summary: "Two propagation operators nest in a structurally ambiguous way.",
    },
    CodeEntry {
        code: "L105",
        variants: &["EmptyTryBlock"],
        owner: "ridge-lower",
        summary: "A `try` block has an empty body.",
    },
    CodeEntry {
        code: "L106",
        variants: &["BareGuardExpr"],
        owner: "ridge-lower",
        summary: "A `when` guard appears outside a `match` arm, where it cannot be desugared.",
    },
    CodeEntry {
        code: "L107",
        variants: &["ToTextLowering"],
        owner: "ridge-lower",
        summary: "String interpolation reached a value with no `ToText` coercion to synthesise.",
    },
    CodeEntry {
        code: "L108",
        variants: &["WithOnNonRecord"],
        owner: "ridge-lower",
        summary: "`with` applied to a value whose type is not a record.",
    },
    CodeEntry {
        code: "L109",
        variants: &["RefutableSliceElement"],
        owner: "ridge-lower",
        summary: "A refutable sub-pattern appears after the variable-length part of a slice pattern.",
    },
    CodeEntry {
        code: "L110",
        variants: &["IntLiteralOutOfRange"],
        owner: "ridge-lower",
        summary: "An integer literal does not fit in the `Int` range (`i64`).",
    },
    CodeEntry {
        code: "L997",
        variants: &["UnsolvedTypeInIR"],
        owner: "ridge-lower",
        summary: "An unsolved type variable reached the IR, indicating incomplete typecheck output was passed to the lowerer.",
    },
    CodeEntry {
        code: "L998",
        variants: &["CapVarInIR"],
        owner: "ridge-lower",
        summary: "A capability variable reached the IR.",
    },
    CodeEntry {
        code: "L999",
        variants: &["InternalLoweringError"],
        owner: "ridge-lower",
        summary: "Catch-all internal lowering invariant violation.",
    },
    CodeEntry {
        code: "M001",
        variants: &["TomlParseFailed"],
        owner: "ridge-manifest",
        summary: "The manifest TOML could not be parsed.",
    },
    CodeEntry {
        code: "M002",
        variants: &["MissingWorkspaceTable"],
        owner: "ridge-manifest",
        summary: "The workspace manifest is missing the `[workspace]` table.",
    },
    CodeEntry {
        code: "M003",
        variants: &["MissingProjectTable"],
        owner: "ridge-manifest",
        summary: "A project manifest is missing the `[project]` table.",
    },
    CodeEntry {
        code: "M004",
        variants: &["MemberWithoutProjectManifest"],
        owner: "ridge-manifest",
        summary: "A workspace member directory has no `ridge.toml` project manifest.",
    },
    CodeEntry {
        code: "M005",
        variants: &["BadMemberGlob"],
        owner: "ridge-manifest",
        summary: "A workspace `members` glob pattern is invalid.",
    },
    CodeEntry {
        code: "M006",
        variants: &["MissingRequiredField"],
        owner: "ridge-manifest",
        summary: "A required field is absent from a manifest table.",
    },
    CodeEntry {
        code: "M007",
        variants: &["InvalidProjectKind"],
        owner: "ridge-manifest",
        summary: "The `kind` field contains an unrecognised project kind string.",
    },
    CodeEntry {
        code: "M008",
        variants: &["InvalidForbidRule"],
        owner: "ridge-manifest",
        summary: "A `forbid` rule entry is syntactically or semantically invalid.",
    },
    CodeEntry {
        code: "M009",
        variants: &["InvalidDependencyKind"],
        owner: "ridge-manifest",
        summary: "A dependency entry uses an unrecognised `kind` value.",
    },
    CodeEntry {
        code: "M010",
        variants: &["DuplicateProjectName"],
        owner: "ridge-manifest",
        summary: "Two workspace members declared the same project name.",
    },
    CodeEntry {
        code: "M011",
        variants: &["InvalidCapabilityName"],
        owner: "ridge-manifest",
        summary: "An unrecognised capability name was used in a manifest.",
    },
    CodeEntry {
        code: "M012",
        variants: &["CycleInDependencies"],
        owner: "ridge-manifest",
        summary: "A dependency cycle was detected among workspace projects.",
    },
    CodeEntry {
        code: "M013",
        variants: &["UnknownWorkspaceMember"],
        owner: "ridge-manifest",
        summary: "A dependency names a project not present in the workspace.",
    },
    CodeEntry {
        code: "M014",
        variants: &["ProjectExportPatternInvalid"],
        owner: "ridge-manifest",
        summary: "A project `exports` pattern string is not a valid glob.",
    },
    CodeEntry {
        code: "M015",
        variants: &["WorkspaceDependencyAbsent"],
        owner: "ridge-manifest",
        summary: "A manifest references a workspace-level dependency that is not declared in `[workspace.dependencies]`.",
    },
    CodeEntry {
        code: "M016",
        variants: &["GitRevConflict"],
        owner: "ridge-manifest",
        summary: "A Git dependency specifies more than one of `tag`, `branch`, or `rev` simultaneously.",
    },
    CodeEntry {
        code: "M017",
        variants: &["RelativePathEscapesWorkspace"],
        owner: "ridge-manifest",
        summary: "A relative path dependency escapes the workspace root.",
    },
    CodeEntry {
        code: "M018",
        variants: &["HexDependencyUsedIn010"],
        owner: "ridge-manifest",
        summary: "Hex dependencies are not supported; use a path or git dependency.",
    },
    CodeEntry {
        code: "M019",
        variants: &["UnknownManifestKey"],
        owner: "ridge-manifest",
        summary: "An unrecognised key appeared in a manifest table.",
    },
    CodeEntry {
        code: "M020",
        variants: &["ExportNotFound"],
        owner: "ridge-manifest",
        summary: "A `[project.exports].public` pattern matched no symbol in the module's top-level table.",
    },
    CodeEntry {
        code: "M021",
        variants: &["EntryModuleNotFound"],
        owner: "ridge-manifest",
        summary: "`entry` names a file that is not a module of the project.",
    },
    CodeEntry {
        code: "M022",
        variants: &["EntryHasNoMain"],
        owner: "ridge-manifest",
        summary: "The module named by `entry` declares no `main`.",
    },
    CodeEntry {
        code: "P001",
        variants: &["Expected"],
        owner: "ridge-parser",
        summary: "The parser expected a specific token but found something else.",
    },
    CodeEntry {
        code: "P002",
        variants: &["UnexpectedToken"],
        owner: "ridge-parser",
        summary: "An unexpected token was encountered with no specific expectation.",
    },
    CodeEntry {
        code: "P005",
        variants: &["MissingType"],
        owner: "ridge-parser",
        summary: "A type annotation is required but was absent.",
    },
    CodeEntry {
        code: "P006",
        variants: &["LayoutMismatch"],
        owner: "ridge-parser",
        summary: "An `Indent`, `Dedent`, or `Newline` token appeared in a context where the layout invariant was violated.",
    },
    CodeEntry {
        code: "P009",
        variants: &["NonAssociativeChain"],
        owner: "ridge-parser",
        summary: "A non-associative operator was chained without parentheses.",
    },
    CodeEntry {
        code: "P012",
        variants: &["TopLevelPatternParam"],
        owner: "ridge-parser",
        summary: "A top-level function parameter was a tuple or constructor pattern.",
    },
    CodeEntry {
        code: "P013",
        variants: &["DeferredFeature"],
        owner: "ridge-parser",
        summary: "A language feature is reserved but deferred to a future version.",
    },
    CodeEntry {
        code: "P014",
        variants: &["EmptyBlock"],
        owner: "ridge-parser",
        summary: "An `INDENT`/`DEDENT` block contained no statements.",
    },
    CodeEntry {
        code: "P018",
        variants: &["BareRecordPattern"],
        owner: "ridge-parser",
        summary: "Retired in 0.2.12.",
    },
    CodeEntry {
        code: "P019",
        variants: &["OrphanDocComment"],
        owner: "ridge-parser",
        summary: "A doc comment sits where it cannot attach to any declaration.",
    },
    CodeEntry {
        code: "P020",
        variants: &["ReservedKeywordAsIdent"],
        owner: "ridge-parser",
        summary: "A reserved keyword (e.g.",
    },
    CodeEntry {
        code: "P021",
        variants: &["MalformedInlineRecordType", "InlineRecordTypeInTypePosition"],
        owner: "ridge-parser",
        summary: "An inline record type `{ … }` in type position is syntactically malformed.",
    },
    CodeEntry {
        code: "P022",
        variants: &["MailboxPolicyMissing"],
        owner: "ridge-parser",
        summary: "`mailbox bounded N` was declared without an overflow policy.",
    },
    CodeEntry {
        code: "P023",
        variants: &["MailboxBoundInvalid"],
        owner: "ridge-parser",
        summary: "`mailbox bounded N` was given a capacity that is not a positive `i64` literal.",
    },
    CodeEntry {
        code: "P024",
        variants: &["MultipleRestInListPattern"],
        owner: "ridge-parser",
        summary: "A list pattern contains more than one `..` rest element.",
    },
    CodeEntry {
        code: "P025",
        variants: &["RestSuffixNotSupported"],
        owner: "ridge-parser",
        summary: "Reserved; previously used for suffix/middle rest (now supported).",
    },
    CodeEntry {
        code: "P026",
        variants: &["RefutableSliceElement"],
        owner: "ridge-parser",
        summary: "A suffix or middle element in a list pattern is a refutable sub-pattern (literal, constructor, tuple, …).",
    },
    CodeEntry {
        code: "P027",
        variants: &["TestAttrArgNotString"],
        owner: "ridge-parser",
        summary: "`@test` was not given a string-literal argument.",
    },
    CodeEntry {
        code: "P028",
        variants: &["ExpressionTooDeep"],
        owner: "ridge-parser",
        summary: "Syntax nested deeper than the parser's recursion limit.",
    },
    CodeEntry {
        code: "P030",
        variants: &["MalformedClassDecl"],
        owner: "ridge-parser",
        summary: "A `class` declaration is structurally malformed.",
    },
    CodeEntry {
        code: "P031",
        variants: &["MalformedInstanceDecl"],
        owner: "ridge-parser",
        summary: "An `instance` declaration is structurally malformed.",
    },
    CodeEntry {
        code: "P032",
        variants: &["OpaqueOnAlias"],
        owner: "ridge-parser",
        summary: "`opaque` was applied to a type alias.",
    },
    CodeEntry {
        code: "P033",
        variants: &["LetInNotSupported"],
        owner: "ridge-parser",
        summary: "A `let … in …` expression was written.",
    },
    CodeEntry {
        code: "P034",
        variants: &["GuardKeywordInMatch"],
        owner: "ridge-parser",
        summary: "A match arm used `if` to introduce its guard.",
    },
    CodeEntry {
        code: "P035",
        variants: &["RecordUpdateSyntax"],
        owner: "ridge-parser",
        summary: "Record update was written `{ record with … }` (the OCaml/Elm/F# spelling).",
    },
    CodeEntry {
        code: "P036",
        variants: &["VersionedRefOutsideMigrate"],
        owner: "ridge-parser",
        summary: "A versioned type reference (`Name@N`) appeared outside a `migrate` signature.",
    },
    CodeEntry {
        code: "P037",
        variants: &["IndexSyntaxNotSupported"],
        owner: "ridge-parser",
        summary: "An expression is followed directly by `[`, the C-family index spelling.",
    },
    CodeEntry {
        code: "P038",
        variants: &["BangNegationNotSupported"],
        owner: "ridge-parser",
        summary: "`!` was written in expression-atom position, the C-family boolean-negation spelling.",
    },
    CodeEntry {
        code: "P039",
        variants: &["MatchBraceBlock"],
        owner: "ridge-parser",
        summary: "A `match` scrutinee was followed directly by `{`, the Rust-style brace-delimited arm block.",
    },
    CodeEntry {
        code: "P040",
        variants: &["LoopNotSupported"],
        owner: "ridge-parser",
        summary: "A `for` or `while` loop was written.",
    },
    CodeEntry {
        code: "P101",
        variants: &["PkgPathManifestMissing"],
        owner: "ridge-pkg",
        summary: "Path dependency's `ridge.toml` is missing or the path does not exist.",
    },
    CodeEntry {
        code: "P102",
        variants: &["PkgManifestParseFailed"],
        owner: "ridge-pkg",
        summary: "A `ridge.toml` was found but could not be parsed.",
    },
    CodeEntry {
        code: "P103",
        variants: &["PkgCacheRootUnavailable"],
        owner: "ridge-pkg",
        summary: "Cache root could not be determined (no home directory available).",
    },
    CodeEntry {
        code: "P104",
        variants: &["PkgGitCommitUnsupported"],
        owner: "ridge-pkg",
        summary: "`GitRev::Commit` was encountered; commit-pinned git dependencies are not yet supported in 0.1.0.",
    },
    CodeEntry {
        code: "P201",
        variants: &["PkgGitFetchFailed"],
        owner: "ridge-pkg",
        summary: "`git clone` exited non-zero due to network failure.",
    },
    CodeEntry {
        code: "P202",
        variants: &["PkgCacheWriteFailed"],
        owner: "ridge-pkg",
        summary: "Cache directory write failed (disk full or permission denied).",
    },
    CodeEntry {
        code: "P203",
        variants: &["PkgGitSchemeUnsupported"],
        owner: "ridge-pkg",
        summary: "Git URL uses SSH scheme (`git@…` or `ssh://…`), which is not supported in 0.1.0 (HTTPS-only).",
    },
    CodeEntry {
        code: "P204",
        variants: &["FloatingBranchAdvisory"],
        owner: "ridge-pkg",
        summary: "A git dependency tracks a mutable branch rather than a pinned tag.",
    },
    CodeEntry {
        code: "P205",
        variants: &["PkgGitNotInstalled"],
        owner: "ridge-pkg",
        summary: "`git` binary not found on `PATH`.",
    },
    CodeEntry {
        code: "P206",
        variants: &["PkgDependencyCycle"],
        owner: "ridge-pkg",
        summary: "Circular dependency detected during resolution.",
    },
    CodeEntry {
        code: "P207",
        variants: &["PkgGitTagUnknown"],
        owner: "ridge-pkg",
        summary: "The requested tag or branch does not exist on the remote.",
    },
    CodeEntry {
        code: "P208",
        variants: &["PkgGitTooOld"],
        owner: "ridge-pkg",
        summary: "Installed `git` is older than the minimum required version 2.20.",
    },
    CodeEntry {
        code: "P209",
        variants: &["PkgGitVersionUnparseable"],
        owner: "ridge-pkg",
        summary: "`git --version` output could not be parsed (exotic distro or custom build).",
    },
    CodeEntry {
        code: "P210",
        variants: &["PkgVersionDepUnsupported"],
        owner: "ridge-pkg",
        summary: "A registry-based version dependency was encountered.",
    },
    CodeEntry {
        code: "P999",
        variants: &["InternalLayoutInvariantViolated"],
        owner: "ridge-parser",
        summary: "The lexer's bracket-suppression invariant was violated — a compiler bug, not yours.",
    },
    CodeEntry {
        code: "R001",
        variants: &["MissingWorkspaceManifest"],
        owner: "ridge-resolve",
        summary: "No `ridge.toml` workspace manifest was found at the given path.",
    },
    CodeEntry {
        code: "R002",
        variants: &["DuplicateModule"],
        owner: "ridge-resolve",
        summary: "The same fully-qualified module name was declared more than once.",
    },
    CodeEntry {
        code: "R003",
        variants: &["CyclicImport"],
        owner: "ridge-resolve",
        summary: "A cycle was detected in the import graph.",
    },
    CodeEntry {
        code: "R004",
        variants: &["SelfImport"],
        owner: "ridge-resolve",
        summary: "A module imports itself.",
    },
    CodeEntry {
        code: "R005",
        variants: &["DuplicateDeclaration"],
        owner: "ridge-resolve",
        summary: "The same name was declared more than once at the top level of a module.",
    },
    CodeEntry {
        code: "R006",
        variants: &["UnresolvedImportPath"],
        owner: "ridge-resolve",
        summary: "An import path could not be resolved to any known module.",
    },
    CodeEntry {
        code: "R007",
        variants: &["ProjectExportViolation"],
        owner: "ridge-resolve",
        summary: "A module in one project tried to import a non-exported symbol from another project.",
    },
    CodeEntry {
        code: "R008",
        variants: &["UnresolvedImportItem"],
        owner: "ridge-resolve",
        summary: "A named import item could not be found in the target module.",
    },
    CodeEntry {
        code: "R009",
        variants: &["VisibilityViolation"],
        owner: "ridge-resolve",
        summary: "A name is referenced outside its declared visibility scope.",
    },
    CodeEntry {
        code: "R010",
        variants: &["UnresolvedIdent"],
        owner: "ridge-resolve",
        summary: "An identifier could not be resolved; suggestions are provided if available.",
    },
    CodeEntry {
        code: "R011",
        variants: &["DuplicateLocal"],
        owner: "ridge-resolve",
        summary: "The same local variable name was bound more than once in the same scope.",
    },
    CodeEntry {
        code: "R012",
        variants: &["UnresolvedQualifiedName"],
        owner: "ridge-resolve",
        summary: "A qualified name (e.g.",
    },
    CodeEntry {
        code: "R013",
        variants: &["ForbidViolation"],
        owner: "ridge-resolve",
        summary: "A `forbid` architectural rule was violated.",
    },
    CodeEntry {
        code: "R014",
        variants: &["UnknownStdlibSymbol"],
        owner: "ridge-resolve",
        summary: "A reference to a standard-library symbol that does not exist.",
    },
    CodeEntry {
        code: "R015",
        variants: &["CapabilityDenied"],
        owner: "ridge-resolve",
        summary: "A capability is used but denied by the project or workspace manifest.",
    },
    CodeEntry {
        code: "R016",
        variants: &["CapabilityNotAllowed"],
        owner: "ridge-resolve",
        summary: "A capability is declared on a function but the project's `capabilities_allow` list does not include it.",
    },
    CodeEntry {
        code: "R017",
        variants: &["StateFieldShadowedByLocal"],
        owner: "ridge-resolve",
        summary: "A local binding shadows an actor state field in the same scope.",
    },
    CodeEntry {
        code: "R019",
        variants: &["UnknownCapabilityKeyword"],
        owner: "ridge-resolve",
        summary: "An unrecognised capability keyword was encountered.",
    },
    CodeEntry {
        code: "R020",
        variants: &["CapabilityListOnWrongDecl"],
        owner: "ridge-resolve",
        summary: "Capabilities were attached to a declaration that does not take them.",
    },
    CodeEntry {
        code: "R021",
        variants: &["ActorStateMissingDefaultOrInit"],
        owner: "ridge-resolve",
        summary: "An actor state type has neither a `default` nor an `init`, so it can never be built.",
    },
    CodeEntry {
        code: "R022",
        variants: &["FfiOutsideStdlib"],
        owner: "ridge-resolve",
        summary: "An `@ffi` attribute was used outside the `crates/ridge-stdlib/` crate.",
    },
    CodeEntry {
        code: "R023",
        variants: &["LegacyRgExtension"],
        owner: "ridge-resolve",
        summary: "A source file with the legacy `.rg` extension was found.",
    },
    CodeEntry {
        code: "R024",
        variants: &["AmbiguousMethodName"],
        owner: "ridge-resolve",
        summary: "Two distinct typeclasses declare the same method name, making a bare reference to that name ambiguous.",
    },
    CodeEntry {
        code: "R025",
        variants: &["OpaqueConstruct"],
        owner: "ridge-resolve",
        summary: "A constructor of an `opaque` type was used to build a value outside the module that declares the type.",
    },
    CodeEntry {
        code: "R026",
        variants: &["OpaquePattern"],
        owner: "ridge-resolve",
        summary: "A constructor of an `opaque` type was matched in a pattern outside the module that declares the type.",
    },
    CodeEntry {
        code: "R027",
        variants: &["OrPatternBindingMismatch"],
        owner: "ridge-resolve",
        summary: "The alternatives of an or-pattern `p1 | p2 | …` bind different variables.",
    },
    CodeEntry {
        code: "R028",
        variants: &["ReservedName"],
        owner: "ridge-resolve",
        summary: "A `type` or union constructor reuses a name the prelude keeps in scope everywhere.",
    },
    CodeEntry {
        code: "R029",
        variants: &["DuplicateActorMember"],
        owner: "ridge-resolve",
        summary: "An actor declares a singleton member (`init`, `mailbox`, `terminate`, or `onDown`) more than once.",
    },
    CodeEntry {
        code: "R999",
        variants: &["InternalNodeIdCollision"],
        owner: "ridge-resolve",
        summary: "Two AST nodes were assigned the same `NodeId` (signals a compiler bug, not a user error).",
    },
    CodeEntry {
        code: "T001",
        variants: &["TypeMismatch"],
        owner: "ridge-typecheck",
        summary: "Type mismatch at an annotation or binding site.",
    },
    CodeEntry {
        code: "T002",
        variants: &["TypeMismatchInCall"],
        owner: "ridge-typecheck",
        summary: "Type mismatch on a specific argument in a function call.",
    },
    CodeEntry {
        code: "T003",
        variants: &["ArityMismatch"],
        owner: "ridge-typecheck",
        summary: "Wrong number of arguments at a call site.",
    },
    CodeEntry {
        code: "T004",
        variants: &["MissingField"],
        owner: "ridge-typecheck",
        summary: "A required field is absent in a record construction expression.",
    },
    CodeEntry {
        code: "T005",
        variants: &["UnknownField"],
        owner: "ridge-typecheck",
        summary: "A field name used in a record construction does not exist on the type.",
    },
    CodeEntry {
        code: "T006",
        variants: &["WithOnNonRecord"],
        owner: "ridge-typecheck",
        summary: "The `with` expression is applied to a non-record type.",
    },
    CodeEntry {
        code: "T007",
        variants: &["PatternTypeMismatch"],
        owner: "ridge-typecheck",
        summary: "A pattern does not match the scrutinee's type.",
    },
    CodeEntry {
        code: "T008",
        variants: &["UnknownConstructor"],
        owner: "ridge-typecheck",
        summary: "A constructor name used in a pattern or expression is not defined on the expected union type.",
    },
    CodeEntry {
        code: "T009",
        variants: &["WrongConstructorArity"],
        owner: "ridge-typecheck",
        summary: "A constructor is applied to the wrong number of arguments.",
    },
    CodeEntry {
        code: "T010",
        variants: &["OccursCheck"],
        owner: "ridge-typecheck",
        summary: "Unification would create an infinite type.",
    },
    CodeEntry {
        code: "T011",
        variants: &["RecursiveTypeAlias"],
        owner: "ridge-typecheck",
        summary: "A chain of type aliases forms a cycle.",
    },
    CodeEntry {
        code: "T012",
        variants: &["ToTextNotDerivable"],
        owner: "ridge-typecheck",
        summary: "Interpolation hole type not in the closed `ToText` set (retired).",
    },
    CodeEntry {
        code: "T013",
        variants: &["PolymorphicRecursion"],
        owner: "ridge-typecheck",
        summary: "A recursive function is used at a different type inside its own body.",
    },
    CodeEntry {
        code: "T014",
        variants: &["CapabilityNotDeclared"],
        owner: "ridge-typecheck",
        summary: "The capability set inferred from a function body exceeds its declared annotation.",
    },
    CodeEntry {
        code: "T015",
        variants: &["UnknownActorHandler"],
        owner: "ridge-typecheck",
        summary: "A message name sent to an actor does not match any declared `on` handler.",
    },
    CodeEntry {
        code: "T016",
        variants: &["NonExhaustiveMatch"],
        owner: "ridge-typecheck",
        summary: "A `match` expression does not cover all constructors / patterns.",
    },
    CodeEntry {
        code: "T017",
        variants: &["RedundantPattern"],
        owner: "ridge-typecheck",
        summary: "A match arm is unreachable because an earlier arm already covers it.",
    },
    CodeEntry {
        code: "T018",
        variants: &["CallerCapabilityInsufficient"],
        owner: "ridge-typecheck",
        summary: "A function calls another with higher capabilities than itself declares.",
    },
    CodeEntry {
        code: "T019",
        variants: &["ActorCapabilityLeak"],
        owner: "ridge-typecheck",
        summary: "An actor handler declares capabilities not present in the actor's own declared capability set.",
    },
    CodeEntry {
        code: "T020",
        variants: &["SendOnNonActor"],
        owner: "ridge-typecheck",
        summary: "The `!` send operator is applied to a non-`Handle` value.",
    },
    CodeEntry {
        code: "T021",
        variants: &["AskOnNonActor", "PropagateOutsideResultOrOption"],
        owner: "ridge-typecheck",
        summary: "The `?>` ask operator is applied to a non-`Handle` value.",
    },
    CodeEntry {
        code: "T022",
        variants: &["DiscardedResult"],
        owner: "ridge-typecheck",
        summary: "A non-`Unit` value is silently discarded at statement level.",
    },
    CodeEntry {
        code: "T023",
        variants: &["UnsolvedTypeVariable"],
        owner: "ridge-typecheck",
        summary: "A type variable cannot be resolved — the user must add a type annotation.",
    },
    CodeEntry {
        code: "T024",
        variants: &["RowVariableLeak"],
        owner: "ridge-typecheck",
        summary: "A capability variable escapes into a user-visible type (D057).",
    },
    CodeEntry {
        code: "T025",
        variants: &["SpawnArityMismatch"],
        owner: "ridge-typecheck",
        summary: "A `spawn` expression passes the wrong number of `init` arguments.",
    },
    CodeEntry {
        code: "T026",
        variants: &["AskTimeoutNotInt"],
        owner: "ridge-typecheck",
        summary: "The expression supplied to `?> ... timeout <expr>` is not `Int`.",
    },
    CodeEntry {
        code: "T027",
        variants: &["MailboxPolicyDropOldestNotShipped"],
        owner: "ridge-typecheck",
        summary: "An actor declares `mailbox bounded N drop oldest`.",
    },
    CodeEntry {
        code: "T028",
        variants: &["IncompleteRecordPattern"],
        owner: "ridge-typecheck",
        summary: "A constructor-less record pattern omits fields and has no `..` rest pattern.",
    },
    CodeEntry {
        code: "T029",
        variants: &["NoInstance"],
        owner: "ridge-typecheck",
        summary: "A constrained function is called with a type that has no instance for the required class.",
    },
    CodeEntry {
        code: "T030",
        variants: &["AmbiguousConstraint"],
        owner: "ridge-typecheck",
        summary: "A class constraint's type variable is ambiguous: neither resolved nor generalised.",
    },
    CodeEntry {
        code: "T031",
        variants: &["OrphanInstance"],
        owner: "ridge-typecheck",
        summary: "An instance is declared outside both the class's module and the type's module.",
    },
    CodeEntry {
        code: "T032",
        variants: &["OverlappingInstance"],
        owner: "ridge-typecheck",
        summary: "A second `instance C T` is declared for the same `(C, T)` pair.",
    },
    CodeEntry {
        code: "T033",
        variants: &["MissingSuperclassInstance"],
        owner: "ridge-typecheck",
        summary: "`instance C T` is declared but a required superclass instance is absent.",
    },
    CodeEntry {
        code: "T034",
        variants: &["ToTextConflict"],
        owner: "ridge-typecheck",
        summary: "A type has both an auto-promoted `pub fn toText` and an explicit `ToText` instance.",
    },
    CodeEntry {
        code: "T035",
        variants: &["SuperclassCycle"],
        owner: "ridge-typecheck",
        summary: "The class hierarchy forms a cycle (e.g.",
    },
    CodeEntry {
        code: "T036",
        variants: &["OpaqueFieldAccess"],
        owner: "ridge-typecheck",
        summary: "A field of an `opaque` type was reached (`.field` or `with`) from outside the module that declares the type.",
    },
    CodeEntry {
        code: "T037",
        variants: &["RowMismatch"],
        owner: "ridge-typecheck",
        summary: "Two record rows disagree on their fixed field sets and cannot be unified.",
    },
    CodeEntry {
        code: "T038",
        variants: &["InstanceArityMismatch"],
        owner: "ridge-typecheck",
        summary: "An `instance` head supplies the wrong number of type atoms for its class.",
    },
    CodeEntry {
        code: "T039",
        variants: &["QuoteUnknownColumn"],
        owner: "ridge-typecheck",
        summary: "A quoted predicate references a field that is not a column of its entity.",
    },
    CodeEntry {
        code: "T040",
        variants: &["QuoteUnsupportedExpr"],
        owner: "ridge-typecheck",
        summary: "A quoted predicate uses a form the quotation layer does not support yet.",
    },
    CodeEntry {
        code: "T041",
        variants: &["QuoteComparisonMismatch"],
        owner: "ridge-typecheck",
        summary: "The two sides of a comparison in a quoted predicate have different types.",
    },
    CodeEntry {
        code: "T042",
        variants: &["QuoteEntityUnknown"],
        owner: "ridge-typecheck",
        summary: "The entity type a quoted predicate is checked against cannot be determined at the call site.",
    },
    CodeEntry {
        code: "T043",
        variants: &["RefutablePatternParam"],
        owner: "ridge-typecheck",
        summary: "A function parameter destructures with a pattern that does not match every value of its type.",
    },
    CodeEntry {
        code: "T044",
        variants: &["NotAConstructor"],
        owner: "ridge-typecheck",
        summary: "A name is used as a constructor (in a value or pattern) but does not name one.",
    },
    CodeEntry {
        code: "T045",
        variants: &["UnknownFunDepVar"],
        owner: "ridge-typecheck",
        summary: "A functional dependency on a class names a variable that is not one of the class's type parameters.",
    },
    CodeEntry {
        code: "T046",
        variants: &["ConflictingFunDep"],
        owner: "ridge-typecheck",
        summary: "Two instances agree on a dependency's determining types but differ on a determined one.",
    },
    CodeEntry {
        code: "T047",
        variants: &["InsertShapeFullEntity"],
        owner: "ridge-typecheck",
        summary: "A full entity was supplied where a typed insert expects its `Insert` companion.",
    },
    CodeEntry {
        code: "T048",
        variants: &["ActorCallbackSignature"],
        owner: "ridge-typecheck",
        summary: "An actor callback's declared parameters do not match the shape the runtime delivers.",
    },
    CodeEntry {
        code: "T049",
        variants: &["UnknownTypeVersion"],
        owner: "ridge-typecheck",
        summary: "A versioned type reference (`User@1`) named a version the compiler has no record of.",
    },
    CodeEntry {
        code: "T050",
        variants: &["DuplicateMigration"],
        owner: "ridge-typecheck",
        summary: "Two `migrate` members on the same type or actor cover the same version edge.",
    },
    CodeEntry {
        code: "T051",
        variants: &["UnsupportedInstanceHead"],
        owner: "ridge-typecheck",
        summary: "An `instance` head has a form the dispatcher cannot key on.",
    },
    CodeEntry {
        code: "T052",
        variants: &["ArithmeticOnNonNumeric"],
        owner: "ridge-typecheck",
        summary: "An arithmetic operator was applied to operands of a concrete non-numeric type.",
    },
    CodeEntry {
        code: "T053",
        variants: &["MainHasParams"],
        owner: "ridge-typecheck",
        summary: "A top-level `fn main` declares parameters.",
    },
    CodeEntry {
        code: "T054",
        variants: &["FieldAccessOnNonRecord"],
        owner: "ridge-typecheck",
        summary: "A field access `base.field` is applied to a non-record type.",
    },
    CodeEntry {
        code: "T101",
        variants: &["FfiArityMismatch"],
        owner: "ridge-stdlib",
        summary: "The `@ffi` arity doesn't match the Ridge parameter count.",
    },
    CodeEntry {
        code: "T102",
        variants: &["FfiCapabilityMismatch"],
        owner: "ridge-stdlib",
        summary: "The Ridge decl is missing a capability that the BEAM target requires.",
    },
    CodeEntry {
        code: "T103",
        variants: &["FfiTargetUnknown"],
        owner: "ridge-stdlib",
        summary: "The BEAM `module:name/arity` triplet is not in the audit table.",
    },
    CodeEntry {
        code: "T999",
        variants: &["InternalTypeError"],
        owner: "ridge-typecheck",
        summary: "Internal type-checker invariant violation — should never reach users.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_so_lookup_can_binary_search() {
        for pair in REGISTRY.windows(2) {
            assert!(
                pair[0].code < pair[1].code,
                "`{}` does not precede `{}`",
                pair[0].code,
                pair[1].code
            );
        }
    }

    #[test]
    fn every_entry_says_something() {
        for e in REGISTRY {
            assert!(!e.variants.is_empty(), "`{}` names no variant", e.code);
            assert!(!e.summary.is_empty(), "`{}` has no summary", e.code);
            assert!(
                e.summary.ends_with('.'),
                "`{}` is not a sentence: {}",
                e.code,
                e.summary
            );
            assert!(
                !e.summary.contains(e.code),
                "`{}` repeats its own code: {}",
                e.code,
                e.summary
            );
        }
    }

    /// The summary is what `ridge explain` will print, so it answers to the
    /// same rule as any other user-facing string: no spec section markers, no
    /// compiler phase numbers, no internal tracker ids.
    #[test]
    fn no_summary_carries_an_internal_reference() {
        for e in REGISTRY {
            for marker in ["§", "Phase ", "OQ-", "FROZEN-"] {
                assert!(
                    !e.summary.contains(marker),
                    "`{}` mentions `{marker}`: {}",
                    e.code,
                    e.summary
                );
            }
        }
    }

    #[test]
    fn lookup_finds_the_first_and_the_last() {
        let first = REGISTRY.first().map(|e| e.code);
        let last = REGISTRY.last().map(|e| e.code);
        assert_eq!(lookup(first.unwrap_or_default()).map(|e| e.code), first);
        assert_eq!(lookup(last.unwrap_or_default()).map(|e| e.code), last);
        assert!(lookup("Z999").is_none());
        assert!(lookup("t001").is_none(), "lookup is case-sensitive");
    }
}
