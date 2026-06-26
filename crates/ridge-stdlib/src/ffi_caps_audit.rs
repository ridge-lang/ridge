//! Closed-list audit table for `@ffi` BEAM targets (§2.7 / §5.6 T3).
//!
//! Every BEAM `module:name/arity` triplet that Ridge stdlib `@ffi` declarations
//! may reference must appear in [`AUDIT_TABLE`].  A target absent from the table
//! produces `T004 FfiTargetUnknown`.  Adding a new target requires editing this
//! file — intentional friction that keeps the capability table authoritative.
//!
//! Capability requirements are expressed as bitmasks from [`ridge_ast::Capability`].
//! `requires_caps: &[]` means the target is pure (no capability required).

use ridge_ast::Capability;

// ── FfiAuditEntry ─────────────────────────────────────────────────────────────

/// One entry in the closed-list audit table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiAuditEntry {
    /// BEAM module name (e.g. `"erlang"`, `"lists"`).
    pub beam_module: &'static str,
    /// Function name inside the module (e.g. `"+"`, `"map"`).
    pub fn_name: &'static str,
    /// Expected BEAM arity.
    pub arity: u32,
    /// Capabilities that callers of this target must declare.
    ///
    /// Empty slice = no capability required (pure).
    pub requires_caps: &'static [Capability],
}

// ── Lookup ────────────────────────────────────────────────────────────────────

/// Look up a BEAM target in the audit table.
///
/// Returns `None` when the triple is not in the closed list (→ T004).
#[must_use]
pub fn lookup(module: &str, name: &str, arity: u32) -> Option<&'static FfiAuditEntry> {
    AUDIT_TABLE
        .iter()
        .find(|e| e.beam_module == module && e.fn_name == name && e.arity == arity)
}

// ── AUDIT_TABLE ───────────────────────────────────────────────────────────────

/// The closed-list audit table.
///
/// Entries are grouped by BEAM module for readability; sorted within each group
/// by (name, arity).  Duplicate (module, name, arity) triplets are a compile-time
/// error — the `lookup` function uses `find` so the first entry wins, but tests
/// assert uniqueness.
///
/// Capability annotations follow §3: targets in `erlang`, `math`, `binary`,
/// `string`, `lists`, `maps` are pure; `os`, `file`, `filelib`, `timer`,
/// `calendar`, `rand`, `httpc`, `ridge_rt` carry the capabilities declared per
/// §3.10–§3.18.
pub static AUDIT_TABLE: &[FfiAuditEntry] = &[
    // ── erlang (pure arithmetic / type ops) ───────────────────────────────────
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "+",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "-",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "-",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "*",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "/",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "div",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "abs",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "float",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "round",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "trunc",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "not",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "and",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "or",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "integer_to_binary",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "binary_to_integer",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "binary_to_float",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "byte_size",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "iolist_to_binary",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "length",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "halt",
        arity: 1,
        requires_caps: &[Capability::Proc],
    },
    // ── erts_internal (IEEE-754 total order) ──────────────────────────────────
    FfiAuditEntry {
        beam_module: "erts_internal",
        fn_name: "cmp_term",
        arity: 2,
        requires_caps: &[],
    },
    // ── math (float ops) ──────────────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "math",
        fn_name: "sqrt",
        arity: 1,
        requires_caps: &[],
    },
    // ── binary (text ops) ─────────────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "binary",
        fn_name: "split",
        arity: 3,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "binary",
        fn_name: "match",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "binary",
        fn_name: "replace",
        arity: 4,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "binary",
        fn_name: "part",
        arity: 3,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "binary",
        fn_name: "copy",
        arity: 2,
        requires_caps: &[],
    },
    // ── string (text ops) ─────────────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "string",
        fn_name: "trim",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "string",
        fn_name: "uppercase",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "string",
        fn_name: "lowercase",
        arity: 1,
        requires_caps: &[],
    },
    // ── lists ─────────────────────────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "map",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "filter",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "foldl",
        arity: 3,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "foldr",
        arity: 3,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "reverse",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "sort",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "sort",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "nthtail",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "flatmap",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "zip",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "zipwith",
        arity: 3,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "member",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "any",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "all",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "seq",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "foreach",
        arity: 2,
        requires_caps: &[],
    },
    // ── maps ──────────────────────────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "maps",
        fn_name: "new",
        arity: 0,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "maps",
        fn_name: "from_list",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "maps",
        fn_name: "to_list",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "maps",
        fn_name: "put",
        arity: 3,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "maps",
        fn_name: "remove",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "maps",
        fn_name: "get",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "maps",
        fn_name: "is_key",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "maps",
        fn_name: "keys",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "maps",
        fn_name: "values",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "maps",
        fn_name: "map",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "maps",
        fn_name: "filter",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "maps",
        fn_name: "size",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "maps",
        fn_name: "merge",
        arity: 2,
        requires_caps: &[],
    },
    // ── os (env capability) ───────────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "os",
        fn_name: "getenv",
        arity: 1,
        requires_caps: &[Capability::Env],
    },
    FfiAuditEntry {
        beam_module: "os",
        fn_name: "putenv",
        arity: 2,
        requires_caps: &[Capability::Env],
    },
    FfiAuditEntry {
        beam_module: "os",
        fn_name: "list_env_vars",
        arity: 0,
        requires_caps: &[Capability::Env],
    },
    // ── file (fs capability) ──────────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "file",
        fn_name: "read_file",
        arity: 1,
        requires_caps: &[Capability::Fs],
    },
    FfiAuditEntry {
        beam_module: "file",
        fn_name: "write_file",
        arity: 2,
        requires_caps: &[Capability::Fs],
    },
    FfiAuditEntry {
        beam_module: "file",
        fn_name: "write_file",
        arity: 3,
        requires_caps: &[Capability::Fs],
    },
    // ── filelib (fs capability) ───────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "filelib",
        fn_name: "is_file",
        arity: 1,
        requires_caps: &[Capability::Fs],
    },
    // ── timer (time capability) ───────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "timer",
        fn_name: "sleep",
        arity: 1,
        requires_caps: &[Capability::Time],
    },
    // ── calendar (time, pure conversion) ──────────────────────────────────────
    FfiAuditEntry {
        beam_module: "calendar",
        fn_name: "rfc3339_to_system_time",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "calendar",
        fn_name: "system_time_to_rfc3339",
        arity: 1,
        requires_caps: &[],
    },
    // ── rand (random capability) ──────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "rand",
        fn_name: "uniform",
        arity: 0,
        requires_caps: &[Capability::Random],
    },
    FfiAuditEntry {
        beam_module: "rand",
        fn_name: "seed",
        arity: 1,
        requires_caps: &[Capability::Random],
    },
    // ── httpc (net capability) ────────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "httpc",
        fn_name: "request",
        arity: 4,
        requires_caps: &[Capability::Net],
    },
    // ── json (OTP 27+, pure) ──────────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "json",
        fn_name: "decode",
        arity: 1,
        requires_caps: &[],
    },
    // ── ridge_rt (runtime adapters, capability per function) ──────────────────
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "text_split_all",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "text_replace_all",
        arity: 3,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "text_like",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "float_to_text",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "bool_to_text",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "int_parse",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "print",
        arity: 1,
        requires_caps: &[Capability::Io],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "println",
        arity: 1,
        requires_caps: &[Capability::Io],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "eprintln",
        arity: 1,
        requires_caps: &[Capability::Io],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "read_line",
        arity: 1,
        requires_caps: &[Capability::Io],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "time_now",
        arity: 1,
        requires_caps: &[Capability::Time],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "time_epoch",
        arity: 0,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "time_diff",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "time_diff_ms",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "time_from_iso",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "time_since_ms",
        arity: 1,
        requires_caps: &[Capability::Time],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "time_iso",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "random_int",
        arity: 2,
        requires_caps: &[Capability::Random],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "random_float",
        arity: 1,
        requires_caps: &[Capability::Random],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "random_alphanumeric",
        arity: 1,
        requires_caps: &[Capability::Random],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "random_seed",
        arity: 1,
        requires_caps: &[Capability::Random],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "random_choice",
        arity: 1,
        requires_caps: &[Capability::Random],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "cli_args",
        arity: 1,
        requires_caps: &[Capability::Env],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "env_get",
        arity: 1,
        requires_caps: &[Capability::Env],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "env_set",
        arity: 2,
        requires_caps: &[Capability::Env],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "env_all",
        arity: 1,
        requires_caps: &[Capability::Env],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "fs_write",
        arity: 2,
        requires_caps: &[Capability::Fs],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "fs_append",
        arity: 2,
        requires_caps: &[Capability::Fs],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "fs_lines",
        arity: 1,
        requires_caps: &[Capability::Fs],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "proc_run",
        arity: 2,
        requires_caps: &[Capability::Proc],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_encode",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_decode",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "http_listen",
        arity: 2,
        requires_caps: &[Capability::Net],
    },
    // ── ridge_rt HTTP client helpers (§3.18 / OQ-S005 / D121) ────────────────
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "http_get",
        arity: 1,
        requires_caps: &[Capability::Net],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "http_post",
        arity: 2,
        requires_caps: &[Capability::Net],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "http_put",
        arity: 2,
        requires_caps: &[Capability::Net],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "http_delete",
        arity: 1,
        requires_caps: &[Capability::Net],
    },
    // ── erlang (integer remainder) ────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "erlang",
        fn_name: "rem",
        arity: 2,
        requires_caps: &[],
    },
    // ── lists (list append) ───────────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "lists",
        fn_name: "append",
        arity: 2,
        requires_caps: &[],
    },
    // ── string (byte length) ──────────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "string",
        fn_name: "length",
        arity: 1,
        requires_caps: &[],
    },
    // ── filelib (directory probe) ─────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "filelib",
        fn_name: "is_dir",
        arity: 1,
        requires_caps: &[Capability::Fs],
    },
    // ── ridge_rt filesystem readers ───────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "fs_read",
        arity: 1,
        requires_caps: &[Capability::Fs],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "fs_read_dir",
        arity: 1,
        requires_caps: &[Capability::Fs],
    },
    // ── ridge_rt scalar parsing / formatting ──────────────────────────────────
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "float_parse",
        arity: 1,
        requires_caps: &[],
    },
    // ── ridge_rt text helpers ─────────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "text_join",
        arity: 2,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "text_slice",
        arity: 3,
        requires_caps: &[],
    },
    // ── ridge_rt list helpers ─────────────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "list_fold",
        arity: 3,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "list_sort_by",
        arity: 2,
        requires_caps: &[],
    },
    // ── ridge_rt timestamp (monotonic epoch read) ─────────────────────────────
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "time_epoch",
        arity: 1,
        requires_caps: &[],
    },
    // ── ridge_rt actor introspection ──────────────────────────────────────────
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "mailbox_size",
        arity: 1,
        requires_caps: &[],
    },
    // ── ridge_rt JSON constructors (pure value building) ──────────────────────
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_null",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_bool",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_int",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_float",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_text",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_list",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_object",
        arity: 1,
        requires_caps: &[],
    },
    // ── ridge_rt JSON accessors (pure value inspection) ───────────────────────
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_as_int",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_as_float",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_as_bool",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_as_text",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_as_list",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_as_object",
        arity: 1,
        requires_caps: &[],
    },
    FfiAuditEntry {
        beam_module: "ridge_rt",
        fn_name: "json_is_null",
        arity: 1,
        requires_caps: &[],
    },
    // ── crypto (pure cryptographic ops) ──────────────────────────────────────
    FfiAuditEntry {
        beam_module: "crypto",
        fn_name: "hash_equals",
        arity: 2,
        requires_caps: &[],
    },
];

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_known_target_returns_entry() {
        let e = lookup("erlang", "+", 2).expect("erlang:+/2 must be in audit table");
        assert_eq!(e.beam_module, "erlang");
        assert_eq!(e.fn_name, "+");
        assert_eq!(e.arity, 2);
        assert!(e.requires_caps.is_empty());
    }

    #[test]
    fn lookup_unknown_target_returns_none() {
        assert!(lookup("some_user_lib", "unsafe_fn", 1).is_none());
    }

    #[test]
    fn os_getenv_requires_env_cap() {
        let e = lookup("os", "getenv", 1).expect("os:getenv/1 must be in audit table");
        assert!(e.requires_caps.contains(&Capability::Env));
    }

    #[test]
    fn ridge_rt_println_requires_io_cap() {
        let e =
            lookup("ridge_rt", "println", 1).expect("ridge_rt:println/1 must be in audit table");
        assert!(e.requires_caps.contains(&Capability::Io));
    }

    #[test]
    fn lists_map_is_pure() {
        let e = lookup("lists", "map", 2).expect("lists:map/2 must be in audit table");
        assert!(e.requires_caps.is_empty(), "lists:map/2 must be pure");
    }

    #[test]
    fn no_duplicate_triplets() {
        let mut seen = std::collections::HashSet::new();
        for e in AUDIT_TABLE {
            let key = (e.beam_module, e.fn_name, e.arity);
            assert!(
                seen.insert(key),
                "duplicate audit entry: {}:{}/{}",
                e.beam_module,
                e.fn_name,
                e.arity
            );
        }
    }

    #[test]
    fn table_is_non_empty() {
        assert!(!AUDIT_TABLE.is_empty());
    }
}
