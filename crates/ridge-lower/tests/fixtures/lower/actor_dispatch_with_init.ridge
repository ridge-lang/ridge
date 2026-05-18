actor Counter =
    state count: Int

    init (start: Int) =
        count <- start

    on increment () -> Unit =
        count <- count + 1

    on get_count () -> Int =
        count
