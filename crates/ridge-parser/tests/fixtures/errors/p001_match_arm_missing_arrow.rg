-- expect: P001
-- Trigger: match arm missing `->`.
fn f x =
    match x
        A 1
