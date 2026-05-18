fn parse_int (s: Text) -> Option Int =
    None

fn parse_bool (s: Text) -> Option Bool =
    None

fn try_block_nested_propagate (a: Text) (b: Text) -> Option Bool =
    try {
        let n = parse_int a ?
        let ok = parse_bool b ?
        if n > 0 then ok else false
    }
