---
wc — a minimal wc(1) clone.

Usage:
  ridge run  -- <file>

Prints line count and byte count for the file.  Word count is omitted because
Text.split does not split globally, so splitting on spaces to count words is
not yet reliable.

Uses Fs.lines (which bridges through ridge_rt:fs_lines and correctly splits
on actual newlines), not Fs.readFile + Text.lines (which is broken).
---

import std.io as Io
import std.fs as Fs
import std.cli as Cli
import std.text as Text
import std.list as List
import std.option as Option

fn env io fs main () -> Unit =
    let args = Cli.args ()
    match List.head args
        None -> Io.println "usage: ridge run -- <file>"
        Some path ->
            match Fs.lines path
                Ok lns ->
                    let nLines = List.length lns
                    -- Workaround: passing Text.byteSize as a bare HOF reference
                    -- calls erlang:byte_size/0 (no args).  Wrap in fn.
                    let nBytes = lns
                        |> List.map (fn s -> Text.byteSize s)
                        |> List.fold (fn a b -> a + b) 0
                    Io.println $"  ${Int.toText nLines} lines"
                    Io.println $"  ${Int.toText nBytes} bytes (content only, newlines stripped)"
                    Io.println $"  ${path}"
                _ -> Io.println (Text.concat "wc: cannot read " path)
