-- expect: R014
-- T15 / §5.1 R014 (variant 2): `Text.zzzzz` — `Text` resolves to the
-- stdlib `std.text` module but `zzzzz` has no Levenshtein-close candidate,
-- so R014 fires with an empty suggestion list.
import std.text as Text

fn f s = Text.zzzzz s
