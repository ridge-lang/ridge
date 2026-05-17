-- expect: T015
-- T015 UnknownActorHandler: sending to handler `typo` which doesn't exist.
actor Counter =
    state count: Int = 0
    on increment = count <- count + 1
    on get -> Int = count

fn spawn badSend (c: Handle Counter) -> Unit =
    c ! typo
