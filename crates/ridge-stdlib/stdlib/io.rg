-- std.io — Console I/O utilities (Tier 3, imports std.text).
--
-- All functions require the `io` capability.
-- Single-line strings only in 0.1.0.
-- §3.10 / plan line 322: readLine returns Result Text Error.

-- Write text to stdout without a trailing newline.
@ffi("ridge_rt", "print", 1)
pub fn io print (s: Text) -> Unit

-- Write text to stdout followed by a newline.
@ffi("ridge_rt", "println", 1)
pub fn io println (s: Text) -> Unit

-- Write text to stderr without a trailing newline.
@ffi("ridge_rt", "eprintln", 1)
pub fn io eprint (s: Text) -> Unit

-- Write text to stderr followed by a newline.
@ffi("ridge_rt", "eprintln", 1)
pub fn io eprintln (s: Text) -> Unit

-- Read one line from stdin.
-- Returns Ok(line) on success or Err(Error { code, message }) on EOF / read error.
-- Bridge: ridge_rt:read_line/1 returns {ok, Line} | {error, {error_record, Code, Message}}.
@ffi("ridge_rt", "read_line", 1)
pub fn io readLine (_unit: Unit) -> Result Text Error
