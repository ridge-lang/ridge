-- expect-clean
-- D040: capabilities are *declared* on `pub`, *inferred* on file-private
-- (underscore-prefixed).  `_helper` uses `Io.println` ({io}) but is private
-- and has no annotation — typechecks cleanly (caps are inferred at call
-- sites, not enforced against a declaration).
import std.io as Io

fn _helper (msg: Text) -> Unit =
    Io.println msg

fn io top (greeting: Text) -> Unit =
    _helper greeting
