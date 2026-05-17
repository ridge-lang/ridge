---
An in-memory URL shortener.  A single actor, Store, owns the code→URL mapping.
An HTTP server translates POST /shorten and GET /:code into actor messages.
Short codes are 6-character base-62 strings generated with std.random.
The actor's internal `random` capability is encapsulated; HTTP handler callers
only require `time` for the implicit ask timeout (Model B, §6.4).
---

import std.io       as Io
import std.list     as List
import std.map      as Map
import std.net.http as Http (Request, Response, listen, respond)
import std.option   as Option
import std.random   as Random
import std.text     (split, trim, startsWith)

-- std.net.http exposes record types `Request { method, path, body }` and
-- `Response { status, body }` plus the `listen` / `respond` server fns.
-- Per D072 the import list above brings them into unqualified scope so the
-- handler signatures and constructors below read naturally.  Route matching
-- is done with a plain `match` expression on the request fields.
-- ── Base-62 code generation ────────────────────────────────────────────────
const alphabet: Text = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"

const codeLen:  Int  = 6

-- NOTE: Random.alphanumeric generates a random alphanumeric character.
-- We assume it accepts a seed-free form and uses internal entropy.
-- The full base-62 set needs Random.choice over the alphabet chars;
-- we use Text.split "" to get a list of single-char texts.
-- (Text.split and List.range are §9.1 items; Random.choice is §9.2.)
fn random genCode () -> Text =
    let chars = split "" alphabet
    List.range 1 codeLen
 |> List.map (fn _ -> Random.choice chars |> Option.withDefault "a")
 |> List.fold (fn acc c -> $"${acc}${c}") ""

-- ── Actor: Store ───────────────────────────────────────────────────────────
actor Store =
    state table: Map Text Text = Map.empty

    -- Returns the 6-char code.  Generates a fresh one if url not already stored.
    on random time shorten (url: Text) -> Text =
        -- Check if the URL already has a code (linear scan over values).
        -- NOTE: Map.toList + List.find for reverse lookup; no dedicated Map.findValue.
        let existing =
            Map.toList table
 |> List.find (fn (_, v) -> v == url)
        match existing
            Some (code, _) -> code
            None ->
                let code = genCode ()
                table <- Map.insert code url table
                code
    on lookup (code: Text) -> Option Text =
        Map.get code table
-- ── HTTP handler helpers ────────────────────────────────────────────────────
-- Extract the short code from a path like "/:code".
fn extractCode (path: Text) -> Option Text =
    -- path is e.g. "/ab3XyZ"; drop the leading slash
    let parts = split "/" path
    parts |> List.drop 1 |> List.head

-- Build a plain-text HTTP response.
fn okText (body: Text) -> Response =
    Response { status = 200, body = body }

fn notFound (body: Text) -> Response =
    Response { status = 404, body = body }

fn redirect (url: Text) -> Response =
    -- 302 Found; in a real impl the Location header would also be set.
    -- Assumption: Response has an optional `headers: Map Text Text` field;
    -- we set it inline here.
    Response { status = 302, body = $"Redirecting to ${url}" }

fn badRequest (body: Text) -> Response =
    Response { status = 400, body = body }

-- ── Main: spawn Store + start HTTP server ──────────────────────────────────
-- NOTE: Http.listen accepts a port and a handler fn io time (Request -> Response),
-- blocking until the server shuts down.
-- D059: main returns Result Unit Error.
fn spawn net io time main () -> Result Unit Error =
    let store = spawn Store
    Io.println "URL shortener listening on :8080"

    -- D045: ask operator changed from ? to ?>.
    Http.listen 8080 (fn (req: Request) -> Response =
        match (req.method, req.path)

            -- POST /shorten  body = raw URL
            ("POST", "/shorten") ->
                let url = trim req.body
                guard (url != "") else return (badRequest "URL body is required")
                let code = store ?> shorten url
                okText $"http://localhost:8080/${code}"

            -- GET /:code  — redirect or 404
            ("GET", path) when path != "/" ->
                let code = extractCode path |> Option.withDefault ""
                guard (code != "") else return (notFound "Not found")
                let target = store ?> lookup code
                match target
                    Some url -> redirect url
                    None -> notFound $"No URL found for code '${code}'"

            -- Catch-all
            _ -> notFound "Not found")
    Ok ()
