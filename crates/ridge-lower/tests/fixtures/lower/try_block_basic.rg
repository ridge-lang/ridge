fn parse_int (s: Text) -> Option Int =
    None

fn try_block_basic (s: Text) -> Option Int =
    try {
        let n = parse_int s ?
        Some (n * 2)
    }
