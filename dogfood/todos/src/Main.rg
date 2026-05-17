---
todos — a tiny persistent CLI todo list.

Usage:
  ridge run -- list                Show all todos.
  ridge run -- add "<title>"       Add a new todo.
  ridge run -- done <id>           Mark a todo as done.
  ridge run -- rm <id>             Remove a todo.

Persistence: todos.db in the current directory, one todo per line in
the format "<id>|<done 0|1>|<title>".

This app exercises the following stdlib features:

  - string escapes (\n, \", \\)
  - Fs.writeFile / Fs.lines + match Ok/Err
  - Text.split
  - record with { field = value }  (see note below)
  - bare HOF reference to a stdlib fn (List.sortBy key)
  - ? after |> pipeline
  - match on user-defined ADT
  - List.fold callback arg order (acc, elem)
  - List.sortBy as a key-fn

Known residual (filed as 0.2.0 follow-up): when `with` appears
inside a lambda whose parameter has no explicit type annotation,
the lowering pass still miscompiles to the atom `ok` because the
inferred type for the lambda parameter isn't visible to
`lower_with::lookup_base_type` at the right point.  Workaround
used below: every lambda that calls `with` annotates its param
(`fn (t: Todo) -> ...`).
---

import std.io as Io
import std.fs as Fs
import std.cli as Cli
import std.text as Text
import std.int as Int
import std.list as List
import std.option as Option

const dbPath: Text = "todos.db"

type Todo = {
    id:    Int,
    done:  Bool,
    title: Text
}

-- ── Parse / format ───────────────────────────────────────────────────────────

fn parseLine (line: Text) -> Option Todo =
    let parts = Text.split "|" line
    if List.length parts == 3 then
        let idOpt = parts |> List.head |> Option.flatMap Int.parse
        let doneOpt = parts |> List.drop 1 |> List.head
        let titleOpt = parts |> List.drop 2 |> List.head
        match idOpt
            Some id ->
                match doneOpt
                    Some d ->
                        match titleOpt
                            Some t -> Some (Todo { id = id, done = d == "1", title = t })
                            None -> None
                    None -> None
            None -> None
    else
        None

fn formatLine (t: Todo) -> Text =
    let doneStr = if t.done then "1" else "0"
    let idStr = Int.toText t.id
    Text.concat (Text.concat (Text.concat idStr "|") (Text.concat doneStr "|")) t.title

fn renderTodo (t: Todo) -> Text =
    let mark = if t.done then "[x]" else "[ ]"
    $"  ${mark} #${Int.toText t.id} ${t.title}"

-- ── Persistence ──────────────────────────────────────────────────────────────

fn fs loadTodos (_unit: Unit) -> List Todo =
    match Fs.lines dbPath
        Ok lns ->
            lns
                |> List.filter (fn s -> s != "")
                |> List.filterMap parseLine
        Err _ -> []

fn fs io saveTodos (todos: List Todo) -> Unit =
    let body = todos
                   |> List.map formatLine
                   |> List.fold (fn acc line -> if acc == "" then line else Text.concat (Text.concat acc "\n") line) ""
    match Fs.writeFile dbPath body
        Ok _ -> ()
        Err msg -> Io.eprintln (Text.concat "todos: save failed: " msg)

-- ── Helpers ──────────────────────────────────────────────────────────────────

fn nextId (todos: List Todo) -> Int =
    let maxId = todos
                    |> List.map (fn t -> t.id)
                    |> List.fold (fn acc n -> if n > acc then n else acc) 0
    maxId + 1

-- ── Commands ─────────────────────────────────────────────────────────────────

fn fs io listCmd (_unit: Unit) -> Unit =
    let todos = loadTodos ()
    if List.isEmpty todos then
        Io.println "  (no todos)"
    else
        todos
            |> List.sortBy (fn t -> t.id)
            |> List.forEach (fn t -> Io.println (renderTodo t))

fn fs io addCmd (title: Text) -> Unit =
    let current = loadTodos ()
    let newTodo = Todo { id = nextId current, done = false, title = title }
    saveTodos (newTodo :: current)
    Io.println (Text.concat "  added: " (renderTodo newTodo))

fn fs io doneCmd (idText: Text) -> Unit =
    match Int.parse idText
        Some targetId ->
            let updated = loadTodos ()
                              |> List.map (fn (t: Todo) -> if t.id == targetId then t with { done = true } else t)
            saveTodos updated
            Io.println $"  marked #${idText} as done"
        None -> Io.eprintln $"todos: '${idText}' is not a valid id"

fn fs io rmCmd (idText: Text) -> Unit =
    match Int.parse idText
        Some targetId ->
            let updated = loadTodos () |> List.filter (fn t -> t.id != targetId)
            saveTodos updated
            Io.println $"  removed #${idText}"
        None -> Io.eprintln $"todos: '${idText}' is not a valid id"

fn io usage (_unit: Unit) -> Unit =
    Io.println "todos — a tiny persistent todo list"
    Io.println ""
    Io.println "Usage:"
    Io.println "  ridge run -- list"
    Io.println "  ridge run -- add \"<title>\""
    Io.println "  ridge run -- done <id>"
    Io.println "  ridge run -- rm <id>"

-- ── Entry point ──────────────────────────────────────────────────────────────

fn env fs io main () -> Unit =
    let args = Cli.args ()
    match List.head args
        None -> usage ()
        Some cmd ->
            let rest = List.drop 1 args
            if cmd == "list" then
                listCmd ()
            else if cmd == "add" then
                match List.head rest
                    Some title -> addCmd title
                    None -> Io.eprintln "todos: add requires a title"
            else if cmd == "done" then
                match List.head rest
                    Some id -> doneCmd id
                    None -> Io.eprintln "todos: done requires an id"
            else if cmd == "rm" then
                match List.head rest
                    Some id -> rmCmd id
                    None -> Io.eprintln "todos: rm requires an id"
            else
                usage ()
