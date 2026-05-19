-- expect: R014
-- T15 / §5.1 R014: `List.mapp` is a typo of `List.map`; the qualified-name
-- pass finds the stdlib `std.list` module via the alias but no `mapp` symbol
-- in its export list, so it emits R014 UnknownStdlibSymbol with a Damerau-
-- Levenshtein suggestion (T13).
import std.list as List

fn f xs = List.mapp xs (fn x -> x)
