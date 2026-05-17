-- expect: T014
-- D018 Model B: `Ask` (`?>`) requires the caller to have `{time}`.  This
-- caller has no declared caps — `{time}` is inferred but not declared, so
-- T014 fires.  Spec §6.4 / OQ-T005.
actor Worker =
    state n: Int = 0
    on write (v: Int) -> Int =
        n <- v
        v

fn caller (h: Handle Worker) -> Int =
    h ?> write 7
