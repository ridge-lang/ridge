-- expect: P006
-- Trigger: deeper indentation level inside a block (Indent where Dedent expected).
-- The lexer emits: fn block INDENT, then `x`, then another INDENT (from the
-- deeper indentation of `y`), which `parse_block` sees as a layout violation.
fn f =
    x
        y
