-- expect: T022
-- T022 DiscardedResult: a non-Unit expression result is silently discarded
-- at statement level in a block body.
fn getNumber -> Int = 42
fn caller -> Unit =
    let x = 1
    getNumber
    x
