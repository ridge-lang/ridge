-- expect: T025
-- T025 SpawnArityMismatch: Counter.init takes 1 arg but spawn passes 0.
actor Counter =
    state count: Int = 0
    init (start: Int) =
        count <- start
    on increment =
        count <- count + 1

fn spawn bad -> Handle Counter =
    spawn Counter
