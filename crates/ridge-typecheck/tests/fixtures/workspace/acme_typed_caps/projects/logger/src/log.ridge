-- A logger fn that requires `{io}`.  Cross-project consumers must declare
-- `{io}` themselves — verifies that capability prefixes propagate across
-- workspace boundaries (D040 / D076).
import std.io as Io

pub fn io info (msg: Text) -> Unit =
    Io.println $"[INFO] ${msg}"
