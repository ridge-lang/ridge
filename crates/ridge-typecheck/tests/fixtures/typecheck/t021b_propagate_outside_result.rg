-- expect: T021
-- T021b PropagateOutsideResultOrOption: using ? on a value outside a
-- Result/Option context. `x?` where x : Int fires T021.
fn bad (x: Int) -> Int = x?
