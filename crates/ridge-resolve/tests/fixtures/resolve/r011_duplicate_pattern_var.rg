-- expect: R011
-- T11: duplicate pattern variable inside a single tuple pattern = R011.
fn f pair =
    let (x, x) = pair
    x
