---
Conway's Game of Life on a fixed 20x40 grid.  A pure `step` function advances
the simulation; I/O and timing are isolated to the main loop.  The grid is
represented as a record `{ rows, cols, cells }` so width and height travel with
the data without extra parameters at every call site.  Main initialises with a
glider, prints each frame at 200 ms intervals, and runs for N generations
(N from argv, default 30).
---

import std.io   as Io
import std.cli  as Cli
import std.time as Time
import std.list as List
import std.option as Option

-- Grid record chosen over bare `List (List Bool)` so step/render never
-- need rows/cols threaded as separate arguments.
type Grid = {
    rows:  Int,
    cols:  Int,
    cells: List (List Bool)
}

-- Out-of-bounds cells are treated as dead (false).
fn cellAt (grid: Grid) (r: Int) (c: Int) -> Bool =
    guard (r >= 0 && r < grid.rows && c >= 0 && c < grid.cols)
        else return false
    let row = grid.cells |> List.drop r |> List.head |> Option.withDefault []
    row |> List.drop c |> List.head |> Option.withDefault false

fn liveNeighbours (grid: Grid) (r: Int) (c: Int) -> Int =
    let deltas = [(-1,-1), (-1,0), (-1,1), (0,-1), (0,1), (1,-1), (1,0), (1,1)]
    deltas
 |> List.map (fn (dr, dc) -> if cellAt grid (r + dr) (c + dc) then 1 else 0)
 |> List.fold (fn a b -> a + b) 0

fn nextCell (grid: Grid) (r: Int) (c: Int) -> Bool =
    let alive = cellAt grid r c
    match (alive, liveNeighbours grid r c)
        (true,  2) -> true
        (true,  3) -> true
        (false, 3) -> true
        _ -> false

-- Advance one generation.  Pure: no capabilities.
fn step (grid: Grid) -> Grid =
    let newCells =
        List.range 0 (grid.rows - 1)
 |> List.map (fn r ->
               List.range 0 (grid.cols - 1)
 |> List.map (fn c -> nextCell grid r c))
    grid with { cells = newCells }

fn io renderGrid (grid: Grid) -> Unit =
    let border = List.range 1 (grid.cols + 2)
 |> List.map (fn _ -> "-")
 |> List.fold (fn a b -> $"${a}${b}") ""
    Io.println $"+${border}+"
    grid.cells |> List.forEach (fn row ->
        let line = row
 |> List.map (fn alive -> if alive then "#" else " ")
 |> List.fold (fn a b -> $"${a}${b}") ""
        Io.println $"| ${line} |")
    Io.println $"+${border}+"

fn makeEmptyGrid (r: Int) (c: Int) -> Grid =
    let emptyRow = List.range 1 c |> List.map (fn _ -> false)
    Grid { rows = r, cols = c, cells = List.range 1 r |> List.map (fn _ -> emptyRow) }

-- NOTE: List.zip + List.range is the idiomatic Ridge "indexed map" pattern.
fn setCell (targetRow: Int) (targetCol: Int) (grid: Grid) -> Grid =
    let newCells =
        List.zip (List.range 0 (grid.rows - 1)) grid.cells
 |> List.map (fn (ri, row) ->
               if ri == targetRow then
                   List.zip (List.range 0 (grid.cols - 1)) row
 |> List.map (fn (ci, v) -> if ci == targetCol then true else v)
               else row)
    grid with { cells = newCells }

-- Glider:  . X .
--          . . X
--          X X X
fn gliderGrid (rows: Int) (cols: Int) -> Grid =
    makeEmptyGrid rows cols
 |> setCell 0 1
 |> setCell 1 2
 |> setCell 2 0
 |> setCell 2 1
 |> setCell 2 2

const defaultGenerations: Int = 30

-- main now returns Result Unit Error; Ok () closes the happy path.
fn env io time main () -> Result Unit Error =
    let args = Cli.args ()
    let generations =
        List.head args
 |> Option.flatMap Int.parse
 |> Option.withDefault defaultGenerations

    -- Inner fn with capability prefixes is explicitly allowed.
    -- Inner loop keeps I/O and time away from the pure step function.
    fn io time loop (gen: Int) (grid: Grid) -> Unit =
        guard (gen > 0) else return ()
        Io.print "\u{1B}[2J\u{1B}[H"
        Io.println $"Generation ${generations - gen + 1} / ${generations}"
        renderGrid grid
        Time.sleep 200
        loop (gen - 1) (step grid)

    loop generations (gliderGrid 20 40)
    Ok ()
