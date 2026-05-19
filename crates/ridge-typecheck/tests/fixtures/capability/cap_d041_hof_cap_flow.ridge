-- expect-clean
-- D041: stdlib HOFs carry a capability variable.  `List.foreach`
-- has shape `(a -> Unit {c}) -> List a -> Unit {c}`.  When the callback uses
-- `Io.println` ({io}), the cap variable `c` unifies to `{io}`, which then
-- flows into the caller's *inferred* caps.  `shout` declares `{io}` so the
-- inferred set matches and the program typechecks cleanly — verifying that
-- HOF cap polymorphism propagates without spurious T-errors.
import std.io as Io
import std.list as List

fn io shout (xs: List Text) -> Unit =
    List.forEach (fn x -> Io.println x) xs
