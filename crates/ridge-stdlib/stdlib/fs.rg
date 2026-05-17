-- std.fs — File-system utilities (Tier 3, imports std.text, std.list, std.result).
--
-- All functions require the `fs` capability.
-- §3.11: error values are Text (typecheck baseline: Result T Text).
-- No filesystem paths as literals in 0.1.0.

-- Read the entire contents of a file as Text.
-- Returns Ok(contents) or Err(reason) on failure.
-- Bridges via ridge_rt:fs_read/1 so the Err side
-- returns Text (atom_to_binary'd) rather than a raw atom.
@ffi("ridge_rt", "fs_read", 1)
pub fn fs readFile (path: Text) -> Result Text Text

-- Write text content to a file, replacing its contents.
-- Returns Ok(()) or Err(reason) on failure.
-- Bridges via ridge_rt:fs_write/2 so the Ok side
-- returns the Ridge Result shape `{ok, ok}` instead of the bare Erlang atom
-- `ok` which crashed any `match ... with Ok _ | Err _` (`if_clause`).
@ffi("ridge_rt", "fs_write", 2)
pub fn fs writeFile (path: Text) (content: Text) -> Result Unit Text

-- Append text content to a file (creates the file if it does not exist).
-- Returns Ok(()) or Err(reason) on failure.
@ffi("ridge_rt", "fs_append", 2)
pub fn fs append (path: Text) (content: Text) -> Result Unit Text

-- Return true if the path refers to a regular file.
@ffi("filelib", "is_file", 1)
pub fn fs exists (path: Text) -> Bool

-- Read a file and split its contents into a list of lines.
-- Returns Ok(lines) or Err(reason) on failure.
@ffi("ridge_rt", "fs_lines", 1)
pub fn fs lines (path: Text) -> Result (List Text) Text
