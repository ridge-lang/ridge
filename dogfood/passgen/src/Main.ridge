---
passgen — random alphanumeric password generator.

Usage:
  ridge run  -- [length]    (default 16)
---

import std.io as Io
import std.cli as Cli
import std.random as Random
import std.list as List
import std.option as Option

const defaultLength: Int = 16

fn env io random main () -> Unit =
    let args = Cli.args ()
    let len =
        List.head args
 |> Option.flatMap Int.parse
 |> Option.withDefault defaultLength
    if len <= 0 then
        Io.println "length must be > 0"
    else
        let pw = Random.alphanumeric len
        Io.println pw
