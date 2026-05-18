-- expect: R005
-- T15 / §5.1 R005: two top-level `fn` declarations with the same name in a
-- single module trigger R005 DuplicateDeclaration.  `collect_symbols` keeps
-- the first declaration and emits R005 anchored at the second one's name span.
fn foo x = x

fn foo y = y
