-- expect: R010
-- T15 / §5.1 R010 (variant 2): `undeclaredFn` is used inside a `let`-binding
-- body where it is not bound; the scope walker emits R010 UnresolvedIdent.
fn compute x =
    let y = undeclaredFn x
    y
