-- expect: P001
-- expect: P001
-- expect: P001
-- Regression: three independent errors must each produce exactly one diagnostic.
-- The `sync_to_next_item` recovery in parse_module ensures all three are found.
fn f1 =
    if a 1 else 2
fn f2 =
    if b 3 else 4
fn f3 =
    if c 5 else 6
