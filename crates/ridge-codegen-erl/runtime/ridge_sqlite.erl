%% ridge_sqlite — the SQLite backend for the std.data adapter.
%%
%% SQLite is an embedded C library, so this module is a thin Erlang layer over a
%% native function (sqlite_nif.c) rather than a socket client like ridge_pg. The
%% native side is deliberately dumb: it opens and closes connections, binds
%% parameters, and returns result cells tagged only by SQLite storage class
%% ({int,_} | {float,_} | {text,_} | {blob,_} | null). All of the SqlValue
%% mapping — the same rows and {ok,_}/{error,_} shapes the rest of the stdlib
%% speaks — lives here in readable Erlang, which keeps the memory-unsafe surface
%% confined to the small C file.
%%
%% The NIF is loaded on first use of the module. Its shared object is located by
%% the RIDGE_SQLITE_NIF environment variable when set (the path with no
%% extension, as erlang:load_nif expects), otherwise beside this module's own
%% .beam. Loading also asserts the linked SQLite version against the vendored
%% pin, so a stale or swapped native artifact fails loudly instead of running.

-module(ridge_sqlite).

-export([
    nif_open/1,
    nif_close/1,
    nif_exec/3,
    nif_query/3,
    nif_libversion/0
]).

-on_load(init/0).

%% The SQLite version this module is built and tested against. Kept in step with
%% runtime/native/README.md and the vendored amalgamation.
-define(PINNED_VERSION, <<"3.45.3">>).

init() ->
    Path = nif_path(),
    case erlang:load_nif(Path, 0) of
        ok ->
            assert_version();
        {error, Reason} ->
            {error, Reason}
    end.

%% Where the native object lives, as a path with no shared-object extension.
nif_path() ->
    case os:getenv("RIDGE_SQLITE_NIF") of
        false -> beside_beam();
        Path -> Path
    end.

beside_beam() ->
    case code:which(?MODULE) of
        Beam when is_list(Beam) ->
            filename:join(filename:dirname(Beam), "ridge_sqlite");
        _ ->
            "ridge_sqlite"
    end.

assert_version() ->
    case nif_libversion() of
        ?PINNED_VERSION ->
            ok;
        Got ->
            {error, {sqlite_version_mismatch, ?PINNED_VERSION, Got}}
    end.

%% --- NIF stubs ---
%%
%% Each function below is replaced by the native implementation once the NIF
%% loads. If it does not load, calling one raises rather than silently doing
%% nothing.

nif_open(_Path) ->
    erlang:nif_error(nif_not_loaded).

nif_close(_Conn) ->
    erlang:nif_error(nif_not_loaded).

nif_exec(_Conn, _Sql, _Params) ->
    erlang:nif_error(nif_not_loaded).

nif_query(_Conn, _Sql, _Params) ->
    erlang:nif_error(nif_not_loaded).

nif_libversion() ->
    erlang:nif_error(nif_not_loaded).
