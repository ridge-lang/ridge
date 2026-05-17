---
Reads a log file whose lines follow the format "YYYY-MM-DD HH:MM:SS LEVEL message",
parses each line into a LogEntry record, filters by a minimum severity level supplied
via argv, groups the surviving entries by hour-of-day (0–23), and prints a text
histogram to stdout.  All I/O happens at the edges; inner parsing and aggregation
functions are pure.
---

import std.fs as Fs
import std.io as Io
import std.env as Env
import std.cli as Cli
import std.text (split, trim, lines)
import std.list as List
import std.map as Map
import std.option as Option

-- Union type for log severity.
type Level = Info | Warn | Error

-- Parsed representation of a single log line.
type LogEntry = {
    date:    Text,
    hour:    Int,
    level:   Level,
    message: Text
}

-- Return a numeric rank so levels can be compared against a threshold.
fn levelRank (l: Level) -> Int =
    match l
        Info -> 0
        Warn -> 1
        Error -> 2

-- Parse a Level from its text representation.  Returns None on unknown tokens.
fn parseLevel (s: Text) -> Option Level =
    match s
        "INFO" -> Some Info
        "WARN" -> Some Warn
        "ERROR" -> Some Error
        _ -> None

-- Parse one raw log line into a LogEntry.
-- Expected format: "YYYY-MM-DD HH:MM:SS LEVEL rest..."
-- NOTE: Int.parse (§9.1 std.int) and Text.split (§9.1 std.text) used below.
fn parseLine (raw: Text) -> Option LogEntry =
    let parts    = split " " raw
    let date     = List.head parts ?
    let timePart = parts |> List.drop 1 |> List.head ?
    let levelStr = parts |> List.drop 2 |> List.head ?
    let msgParts = parts |> List.drop 3
    let level    = parseLevel levelStr ?
    let hourStr  = timePart |> split ":" |> List.head ?
    let hour     = Int.parse hourStr ?
    Some (LogEntry { date = date, hour = hour, level = level, message = List.fold (fn a b -> $"${a} ${b}") "" msgParts })

-- Filter entries whose level is at least as severe as the threshold.
fn meetsThreshold (threshold: Level) (entry: LogEntry) -> Bool =
    levelRank entry.level >= levelRank threshold

-- Increment the count for key `k` in the histogram map.
fn tally (k: Int) (acc: Map Int Int) -> Map Int Int =
    let current = Map.get k acc |> Option.withDefault 0
    Map.insert k (current + 1) acc

-- Build a Map Int Int from hour -> count.
fn buildHistogram (entries: List LogEntry) -> Map Int Int =
    List.fold (fn acc e -> tally e.hour acc) Map.empty entries

-- Render a single histogram bar:  " 7 | ████ (4)"
-- NOTE: Text.padLeft is §9.1 std.text.
fn renderBar (hour: Int) (count: Int) -> Text =
    let hourLabel = Int.toText hour |> Text.padLeft 2 " "
    let bar       = List.range 1 count |> List.map (fn _ -> "█") |> List.fold (fn a b -> $"${a}${b}") ""
    $" ${hourLabel} | ${bar} (${count})"

fn io printHistogram (histogram: Map Int Int) -> Unit =
    let pairs = Map.toList histogram
 |> List.sortBy (fn (k, _) -> k)
    Io.println "Hour | Entries"
    Io.println "-----+---------"
    pairs |> List.forEach (fn (h, c) ->
        Io.println (renderBar h c))

-- D059: main now returns Result Unit Text; ? propagation flows naturally.
fn env io fs main () -> Result Unit Text =
    let args = Cli.args ()
    guard (List.length args >= 2) else
        Io.eprintln "Usage: log_analyzer <file> <MIN_LEVEL>"
        return Err "missing arguments"
    -- args length was checked above; these heads are safe.
    let path     = List.head args |> Option.withDefault ""
    let levelArg = args |> List.drop 1 |> List.head |> Option.withDefault "WARN"
    let threshold = parseLevel levelArg |> Option.withDefault Warn

    -- Fs.lines returns Result (List Text) Text; ? propagates the Err upward.
    let rawLines = Fs.lines path ?
    let entries =
        rawLines
 |> List.filterMap parseLine
 |> List.filter (meetsThreshold threshold)

    let histogram = buildHistogram entries
    Io.println $"Log analysis — threshold: ${levelArg}"
    Io.println ""
    match (List.length entries)
        0 -> Io.println "No entries match the given threshold."
        _ -> printHistogram histogram
    Ok ()
