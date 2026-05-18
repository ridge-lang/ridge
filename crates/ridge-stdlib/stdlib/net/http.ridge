-- std.net.http — HTTP client and server (Tier 4, §3.18).
--
-- Client strategy: @ffi to ridge_rt helpers that wrap httpc:request/4
-- (OTP-bundled inets — zero external deps).
-- Server strategy: @ffi to ridge_rt:http_listen/2 which drives the
-- accept loop on a gen_tcp socket.
--
-- Scope guard (§1.2): TLS, custom headers, query strings deferred to 0.2.0.
--
-- Request  { method: Text, path: Text, body: Text }
-- Response { status: Int,  body: Text }
-- Error    = pre-allocated built-in record { code: Text, message: Text }

-- An incoming HTTP request (server) or request descriptor (client).
pub type Request = { method: Text, path: Text, body: Text }

-- An HTTP response produced by a handler or returned by a client call.
pub type Response = { status: Int, body: Text }

-- ── Raw FFI helpers (private) ─────────────────────────────────────────────────
--
-- ridge_rt:http_get/1    — wraps httpc:request(get, {Url,[]}, [], [])
--                          returns {ok, {Status, Body}} | {error, {error_record, Code, Msg}}
-- ridge_rt:http_post/2   — wraps httpc:request(post, {Url,CType,CType,Body}, [], [])
-- ridge_rt:http_put/2    — wraps httpc:request(put, ...)
-- ridge_rt:http_delete/1 — wraps httpc:request(delete, ...)
--
-- Each bridge function returns {ok, {Status, Body}} on success where Status
-- is an integer (e.g. 200) and Body is a binary.
-- On failure returns {error, {error_record, Code, Message}}.

@ffi("ridge_rt", "http_get", 1)
fn raw_http_get (url: Text) -> Result Response Error

@ffi("ridge_rt", "http_post", 2)
fn raw_http_post (url: Text) (body: Text) -> Result Response Error

@ffi("ridge_rt", "http_put", 2)
fn raw_http_put (url: Text) (body: Text) -> Result Response Error

@ffi("ridge_rt", "http_delete", 1)
fn raw_http_delete (url: Text) -> Result Response Error

-- ── Public client API ────────────────────────────────────────────────────────

-- Perform an HTTP GET request against `url`.
-- Returns Ok(Response) on a successful connection, Err(Error) otherwise.
-- Requires the `net` capability.
pub fn net get (url: Text) -> Result Response Error =
    raw_http_get url

-- Perform an HTTP POST request with the given body text.
-- Requires the `net` capability.
pub fn net post (url: Text) (body: Text) -> Result Response Error =
    raw_http_post url body

-- Perform an HTTP PUT request with the given body text.
-- Requires the `net` capability.
pub fn net put (url: Text) (body: Text) -> Result Response Error =
    raw_http_put url body

-- Perform an HTTP DELETE request against `url`.
-- Requires the `net` capability.
pub fn net delete (url: Text) -> Result Response Error =
    raw_http_delete url

-- ── Public server API ────────────────────────────────────────────────────────

-- Start an HTTP server on `port`, dispatching each request to `handler`.
-- Binding port 0 lets the OS assign a free port (used by tests).
-- The bound port is registered under the name `ridge_http_server` via
-- ridge_rt so tests can retrieve it via ridge_rt:http_port/0.
-- Returns Unit (blocks in the accept loop until the process is killed).
-- Requires the `net` capability.
@ffi("ridge_rt", "http_listen", 2)
pub fn net listen (port: Int) (handler: fn net Request -> Response) -> Unit

-- ── Pure constructor ──────────────────────────────────────────────────────────

-- Construct a Response with the given HTTP status code and body text.
-- Pure — no capability required.
pub fn respond (status: Int) (body: Text) -> Response =
    Response { status = status, body = body }
