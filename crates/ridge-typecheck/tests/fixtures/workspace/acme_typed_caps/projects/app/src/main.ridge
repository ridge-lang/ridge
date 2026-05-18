-- Independent project in the `acme-caps` workspace.  Exercises the workspace
-- pipeline + capability prefix on a `pub` fn, alongside the `logger` project
-- that also requires `{io}`.  D040 (caps declared on `pub`) verified per
-- module under `typecheck_workspace`.
import std.io as Io

pub fn io banner (line: Text) -> Unit =
    Io.println $"=== ${line} ==="
