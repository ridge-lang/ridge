-- expect: T010
-- T010 OccursCheck: self-application creates an infinite type.
-- `selfApply x = x x` requires x : a and x : a -> b simultaneously,
-- so a ~ a -> b, which is an infinite recursive type.
fn selfApply x = x x
