actor Counter =
    state count: Int

    init (start: Int) =
        count <- start

    on get_count () -> Int =
        count

fn main =
    let c = spawn Counter 0
    c ?> get_count timeout never
