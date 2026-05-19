-- std.cli — Command-line interface helpers (Tier 3).
--
-- §3.15 / §9.2: the capability is `env` for `args`, `proc` for `exit`.
-- Both functions are exposed under the `std.cli` module but their
-- capability annotations follow the spec (not a separate `cli` capability).

-- Return the command-line arguments as a list of Text values.
-- Capability: env (arguments are part of the process environment).
@ffi("ridge_rt", "cli_args", 1)
pub fn env args (_unit: Unit) -> List Text

-- Terminate the current process with the given exit code.
-- Capability: proc (controls the process lifecycle).
-- Note: formally returns Unit but never actually returns.
@ffi("erlang", "halt", 1)
pub fn proc exit (code: Int) -> Unit
