-- expect: R005
-- T15 / §5.1 R005 (variant 3): three top-level `fn` declarations — the third
-- `fn baz` is a duplicate of the second (different from variant 1 which only
-- has two declarations).  Both R005s fire but only one `-- expect:` is needed
-- to confirm the code is emitted.
fn foo x = x

fn baz y = y

fn baz z = z
