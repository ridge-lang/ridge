-- expect-clean
-- D018 Model B: `Ask` (`?>`) absorbs ONLY `{time}` for the caller.  Even
-- though `Worker.write` declares `{fs}`, the caller need not declare `{fs}`
-- — only `{time}`.  Spec §6.4 / OQ-T005.
actor Worker =
    state n: Int = 0
    on fs write (v: Int) -> Int =
        n <- v
        v

fn time caller (h: Handle Worker) -> Int =
    h ?> write 42
