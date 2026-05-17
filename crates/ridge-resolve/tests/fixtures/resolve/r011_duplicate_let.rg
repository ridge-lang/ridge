-- expect: R011
-- T11: two `let x = ...` in the same Block scope = R011 DuplicateLocal.
fn f =
    let x = 1
    let x = 2
    x
