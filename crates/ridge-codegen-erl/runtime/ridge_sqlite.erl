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
%% The adapter verbs mirror ridge_pg's FFI surface, so the std.data Sqlite
%% instance is a straight parallel of the Postgres one. A handle carries an
%% integer id that selects a per-connection worker process in this module's
%% registry — the same id-as-handle shape MemAdapter and Postgres use. Unlike
%% Postgres there is no pool: a SQLite connection is a cheap in-process handle,
%% so a handle owns exactly one connection and its worker serialises every call
%% on it, which is what makes transactions on that connection coherent.
%%
%% Reads decode to a RAW SqlValue chosen by storage class (SqlInt/SqlFloat/
%% SqlText/SqlBytes/SqlNull) — SQLite is dynamically typed, so the storage class
%% is all the wire carries. The rich types (a Bool from 0/1, a Timestamp from
%% ISO text) are reconstructed one layer up by the column codec, which knows the
%% Ridge type. Writes go the other way here: every SqlValue is encoded to the
%% storage a round-trip needs — bool to 0/1, timestamp to ISO-8601 UTC text,
%% decimal and uuid to text, bytes to a blob, an interval to whole milliseconds.
%%
%% The NIF is loaded on first use. Its shared object is found by the
%% RIDGE_SQLITE_NIF environment variable when set (a path with no extension, as
%% erlang:load_nif expects), otherwise beside this module's own .beam. Loading
%% also asserts the linked SQLite version against the vendored pin, so a stale
%% or swapped native artifact fails loudly instead of running.

-module(ridge_sqlite).

-export([
    sqlite_connect/5,
    sqlite_all/2,
    sqlite_get_rows/4,
    sqlite_begin/1,
    sqlite_begin/2,
    sqlite_commit/1,
    sqlite_rollback/1,
    sqlite_migrations_applied/1,
    sqlite_record_migration/2,
    sqlite_unrecord_migration/2,
    sqlite_raw_query/3,
    sqlite_raw_exec/3,
    sqlite_close/1,
    %% Low-level native surface, replaced by the NIF on load.
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

%% How long a verb waits for its worker before giving up.
-define(CALL_TIMEOUT, 30000).

%% The migration bookkeeping table, mirroring the Postgres client.
-define(MIGRATIONS_TABLE, <<"_ridge_migrations">>).

%% ==================================================================
%% NIF loading
%% ==================================================================

init() ->
    Path = nif_path(),
    case erlang:load_nif(Path, 0) of
        ok ->
            assert_version();
        {error, Reason} ->
            {error, Reason}
    end.

%% Where the native object lives, as a path with no shared-object extension.
%% An explicit override wins; otherwise prefer a loose object beside the beam (a
%% `ridge run` build), then fall back to the object embedded in the running
%% escript, extracted to a per-user cache file.
nif_path() ->
    case os:getenv("RIDGE_SQLITE_NIF") of
        false -> resolve_nif();
        Path -> Path
    end.

resolve_nif() ->
    Loose = beside_beam(),
    case filelib:is_regular(Loose ++ nif_ext()) of
        true -> Loose;
        false -> from_escript(Loose)
    end.

beside_beam() ->
    case code:which(?MODULE) of
        Beam when is_list(Beam) ->
            filename:join(filename:dirname(Beam), "ridge_sqlite");
        _ ->
            "ridge_sqlite"
    end.

nif_ext() ->
    case os:type() of
        {win32, _} -> ".dll";
        _ -> ".so"
    end.

%% Extract the object embedded in the running escript archive to a per-user
%% cache file and return its base path. If any step is impossible (not running
%% as an escript, no embedded object), fall back to Loose so load_nif fails with
%% a clear reason rather than here.
from_escript(Loose) ->
    try
        {ok, Sections} = escript:extract(escript:script_name(), []),
        Archive = proplists:get_value(archive, Sections),
        Entry = "ridge_sqlite" ++ nif_ext(),
        {ok, [{Entry, Bytes}]} = zip:extract(Archive, [memory, {file_list, [Entry]}]),
        Base = cache_base(Bytes),
        ok = ensure_written(Base ++ nif_ext(), Bytes),
        Base
    catch
        _:_ -> Loose
    end.

%% A stable per-user cache path for this exact object, keyed by its content so it
%% is written once and shared across runs.
cache_base(Bytes) ->
    Dir = filename:basedir(user_cache, "ridge"),
    ok = filelib:ensure_dir(filename:join(Dir, "keep")),
    Tag = integer_to_list(erlang:phash2(Bytes)),
    filename:join(Dir, "ridge_sqlite_" ++ Tag).

%% Write Bytes to Path if absent, through a temp file and a rename, so a
%% concurrent first load never observes a half-written object.
ensure_written(Path, Bytes) ->
    case filelib:is_regular(Path) of
        true ->
            ok;
        false ->
            Tmp = Path ++ "." ++ integer_to_list(erlang:unique_integer([positive])) ++ ".tmp",
            ok = file:write_file(Tmp, Bytes),
            case file:rename(Tmp, Path) of
                ok -> ok;
                {error, _} -> _ = file:delete(Tmp), ok
            end
    end.

assert_version() ->
    case nif_libversion() of
        ?PINNED_VERSION ->
            ok;
        Got ->
            {error, {sqlite_version_mismatch, ?PINNED_VERSION, Got}}
    end.

%% ==================================================================
%% Adapter verbs
%% ==================================================================

%% sqlite_connect(Path, BusyTimeoutMs, JournalMode, ForeignKeys, DefaultIsolation) -> Result Sqlite Error
%%
%% Open (creating if absent) a connection to the database at Path (":memory:"
%% for a private in-memory database), apply the connection pragmas, then start
%% the worker that owns it and return the handle #{id => Id}. BusyTimeoutMs of 0
%% keeps the built-in wait; JournalMode is a mode name such as <<"WAL">> or an
%% empty binary to leave the default; ForeignKeys is 1 to enforce foreign keys,
%% 0 to leave them off. DefaultIsolation is the isolation level a plain
%% transaction begins at; the worker tracks it with the level of the open
%% transaction so a nested transactionWith can be checked against it.
sqlite_connect(Path, BusyTimeoutMs, JournalMode, ForeignKeys, DefaultIsolation) ->
    case nif_open(to_bin(Path)) of
        {ok, Res} ->
            case apply_pragmas(Res, BusyTimeoutMs, JournalMode, ForeignKeys) of
                ok ->
                    Worker = spawn(fun() -> worker_loop(#{res => Res, depth => 0,
                                                          default_isolation => known_isolation(DefaultIsolation),
                                                          tx_level => undefined, ru_on => false}) end),
                    Id = registry_call({register, Worker}),
                    {ok, #{id => Id}};
                {error, E} ->
                    nif_close(Res),
                    {error, E}
            end;
        {error, E} ->
            {error, connect_error(E)}
    end.

%% sqlite_all/2 — every row of Table. Result (List Row) Error.
sqlite_all(Id, Table) -> call(Id, {all, Table}).

%% sqlite_get_rows/4 — the rows of Table whose Column holds exactly Key.
%% Result (List Row) Error.
sqlite_get_rows(Id, Table, Column, Key) ->
    call(Id, {get_rows, Table, Column, Key}).

%% sqlite_raw_query/3 — run raw SQL with positional parameters bound from Params
%% (a List SqlValue), returning the rows as column maps. Result (List Row) Error.
sqlite_raw_query(Id, Sql, Params) -> call(Id, {raw_query, Sql, Params}).

%% sqlite_raw_exec/3 — run a raw statement (INSERT/UPDATE/DELETE/DDL) with bound
%% parameters; answer the affected row count. Result Int Error.
sqlite_raw_exec(Id, Sql, Params) -> call(Id, {raw_exec, Sql, Params}).

%% sqlite_begin/1 — open a transaction, or a savepoint when one is already open.
%% Result Unit Error.
sqlite_begin(Id) -> tx_unit(call(Id, begin_tx)).

%% sqlite_begin/2 — the same at an explicit isolation level
%% (Repo.transactionWith). Result Unit Error.
sqlite_begin(Id, Level) -> tx_unit(call(Id, {begin_tx, known_isolation(Level)})).

%% sqlite_commit/1 — commit the innermost open transaction (COMMIT at the
%% outermost level, RELEASE SAVEPOINT when nested). Result Unit Error.
sqlite_commit(Id) -> tx_unit(call(Id, commit_tx)).

%% sqlite_rollback/1 — roll back the innermost open transaction (ROLLBACK at the
%% outermost level, ROLLBACK TO SAVEPOINT when nested). Result Unit Error.
sqlite_rollback(Id) -> tx_unit(call(Id, rollback_tx)).

%% sqlite_migrations_applied/1 — ensure the tracking table and read the applied
%% names in application order. Result (List Text) Error.
sqlite_migrations_applied(Id) -> call(Id, migrations_init).

%% sqlite_record_migration/2 — record a migration name with a parameterised
%% insert; the name is bound, never spliced. Result Unit Error.
sqlite_record_migration(Id, Name) ->
    Sql = [<<"INSERT INTO ">>, quote_ident(?MIGRATIONS_TABLE),
           <<" (">>, quote_ident(<<"name">>), <<") VALUES (?)">>],
    unit(call(Id, {raw_exec, Sql, [{'SqlText', Name}]})).

%% sqlite_unrecord_migration/2 — remove a migration name, the inverse run by a
%% rollback to forget a reverted migration. Result Unit Error.
sqlite_unrecord_migration(Id, Name) ->
    Sql = [<<"DELETE FROM ">>, quote_ident(?MIGRATIONS_TABLE),
           <<" WHERE ">>, quote_ident(<<"name">>), <<" = ?">>],
    unit(call(Id, {raw_exec, Sql, [{'SqlText', Name}]})).

%% sqlite_close/1 — close the connection and forget the handle. Closing a handle
%% that is already gone is not an error. Result Unit Error.
sqlite_close(Id) ->
    Reply =
        case registry_call({lookup, Id}) of
            {ok, Worker} -> worker_request(Worker, close);
            _ -> {ok, ok}
        end,
    registry_call({unregister, Id}),
    tx_unit(Reply).

%% ==================================================================
%% Call dispatch
%% ==================================================================

call(Id, Req) ->
    case registry_call({lookup, Id}) of
        {ok, Worker} ->
            worker_request(Worker, Req);
        _ ->
            {error, closed_handle_error()}
    end.

worker_request(Worker, Req) ->
    Ref = make_ref(),
    MRef = erlang:monitor(process, Worker),
    Worker ! {Req, self(), Ref},
    receive
        {Ref, Reply} ->
            erlang:demonitor(MRef, [flush]),
            Reply;
        {'DOWN', MRef, process, Worker, _Info} ->
            {error, closed_handle_error()}
    after ?CALL_TIMEOUT ->
        erlang:demonitor(MRef, [flush]),
        {error, #{code => <<"db.timeout">>,
                  message => <<"the SQLite connection timed out">>}}
    end.

%% ==================================================================
%% Per-connection worker
%% ==================================================================

worker_loop(State) ->
    receive
        {close, From, Ref} ->
            Res = maps:get(res, State),
            _ = nif_close(Res),
            From ! {Ref, {ok, ok}};
            %% no recursion: the worker exits once its connection is closed
        {Req, From, Ref} ->
            {Reply, State2} = handle_request(Req, State),
            From ! {Ref, Reply},
            worker_loop(State2)
    end.

handle_request({raw_exec, Sql, Params}, State) ->
    Res = maps:get(res, State),
    Reply =
        case nif_exec(Res, to_bin(Sql), to_cells(Params)) of
            {ok, N} -> {ok, N};
            {error, E} -> {error, db_error(E)}
        end,
    {Reply, State};
handle_request({raw_query, Sql, Params}, State) ->
    Res = maps:get(res, State),
    Reply =
        case nif_query(Res, to_bin(Sql), to_cells(Params)) of
            {ok, Cols, Rows} -> {ok, [build_row(Cols, R) || R <- Rows]};
            {error, E} -> {error, db_error(E)}
        end,
    {Reply, State};
handle_request({all, Table}, State) ->
    handle_request({raw_query, [<<"SELECT * FROM ">>, quote_ident(Table)], []}, State);
handle_request({get_rows, Table, Column, Key}, State) ->
    Sql = [<<"SELECT * FROM ">>, quote_ident(Table),
           <<" WHERE ">>, quote_ident(Column), <<" = ?">>],
    handle_request({raw_query, Sql, [Key]}, State);
handle_request(begin_tx, State) ->
    case maps:get(depth, State) of
        0 ->
            open_tx(maps:get(default_isolation, State), State);
        Depth ->
            run_tx([<<"SAVEPOINT ">>, savepoint_name(Depth)], Depth + 1, State)
    end;
handle_request({begin_tx, Level}, State) ->
    case maps:get(depth, State) of
        0 ->
            open_tx(Level, State);
        Depth ->
            case Level =:= maps:get(tx_level, State) of
                true ->
                    run_tx([<<"SAVEPOINT ">>, savepoint_name(Depth)], Depth + 1, State);
                false ->
                    {isolation_mismatch_error(), State}
            end
    end;
handle_request(commit_tx, State) ->
    case maps:get(depth, State) of
        0 ->
            {{ok, ok}, State};
        1 ->
            close_tx(<<"COMMIT">>, State);
        Depth ->
            run_tx([<<"RELEASE SAVEPOINT ">>, savepoint_name(Depth - 1)], Depth - 1, State)
    end;
handle_request(rollback_tx, State) ->
    case maps:get(depth, State) of
        0 ->
            {{ok, ok}, State};
        1 ->
            close_tx(<<"ROLLBACK">>, State);
        Depth ->
            run_tx([<<"ROLLBACK TO SAVEPOINT ">>, savepoint_name(Depth - 1)], Depth - 1, State)
    end;
handle_request(migrations_init, State) ->
    Res = maps:get(res, State),
    Create = [<<"CREATE TABLE IF NOT EXISTS ">>, quote_ident(?MIGRATIONS_TABLE),
              <<" (">>, quote_ident(<<"name">>), <<" TEXT PRIMARY KEY, ">>,
              quote_ident(<<"applied_at">>),
              <<" TEXT NOT NULL DEFAULT (datetime('now')))">>],
    case nif_exec(Res, to_bin(Create), []) of
        {ok, _} ->
            Select = [<<"SELECT ">>, quote_ident(<<"name">>), <<" FROM ">>,
                      quote_ident(?MIGRATIONS_TABLE), <<" ORDER BY rowid">>],
            case nif_query(Res, to_bin(Select), []) of
                {ok, _Cols, Rows} ->
                    {{ok, [migration_name(R) || R <- Rows]}, State};
                {error, E} ->
                    {{error, db_error(E)}, State}
            end;
        {error, E} ->
            {{error, db_error(E)}, State}
    end.

%% Run a transaction-control statement, moving to NewDepth on success.
run_tx(Sql, NewDepth, State) ->
    Res = maps:get(res, State),
    case nif_exec(Res, to_bin(Sql), []) of
        {ok, _} -> {{ok, ok}, State#{depth => NewDepth}};
        {error, E} -> {{error, db_error(E)}, State}
    end.

%% Validate an isolation level name crossing the FFI; an unknown name reads as
%% the read_committed fallback so a bad value can never drive a pragma the
%% runtime did not intend.
known_isolation(<<"read_uncommitted">>) -> <<"read_uncommitted">>;
known_isolation(<<"repeatable_read">>)  -> <<"repeatable_read">>;
known_isolation(<<"serializable">>)     -> <<"serializable">>;
known_isolation(_)                      -> <<"read_committed">>.

%% The error a nested transactionWith answers when its explicit isolation level
%% differs from the one the open transaction started with — SQL forbids
%% changing a transaction's isolation level mid-transaction.
isolation_mismatch_error() ->
    {error, #{code => <<"db.tx.isolation_mismatch">>,
              message => <<"a nested transaction cannot change the isolation level of the transaction already open">>}}.

%% Open the outermost transaction at an isolation level. SQLite is always
%% serializable for committed reads; the one distinguishable level is
%% read_uncommitted, honoured by the connection pragma for the transaction's
%% span and restored when it closes. read_committed and repeatable_read degrade
%% to serializable, which SQLite already guarantees.
open_tx(Level, State) ->
    Res = maps:get(res, State),
    Pre = case Level of
              <<"read_uncommitted">> -> nif_exec(Res, <<"PRAGMA read_uncommitted=ON">>, []);
              _                      -> {ok, 0}
          end,
    case Pre of
        {ok, _} ->
            case nif_exec(Res, <<"BEGIN">>, []) of
                {ok, _} ->
                    {{ok, ok}, State#{depth => 1, tx_level => Level,
                                      ru_on => Level =:= <<"read_uncommitted">>}};
                {error, E} ->
                    {{error, db_error(E)}, State}
            end;
        {error, E} ->
            {{error, db_error(E)}, State}
    end.

%% Close the outermost transaction, restoring the read_uncommitted pragma when
%% the transaction had relaxed it.
close_tx(Sql, State) ->
    Res = maps:get(res, State),
    case nif_exec(Res, to_bin(Sql), []) of
        {ok, _} ->
            maybe_restore_ru(Res, maps:get(ru_on, State, false)),
            {{ok, ok}, State#{depth => 0, tx_level => undefined, ru_on => false}};
        {error, E} ->
            {{error, db_error(E)}, State}
    end.

maybe_restore_ru(Res, true) ->
    _ = nif_exec(Res, <<"PRAGMA read_uncommitted=OFF">>, []),
    ok;
maybe_restore_ru(_, false) ->
    ok.

%% A savepoint identifier for nesting level N. It names a runtime savepoint,
%% never user data, so it needs no quoting.
savepoint_name(N) -> [<<"ridge_sp_">>, integer_to_binary(N)].

%% The `name` text out of a tracking-table row (a single text cell).
migration_name([{text, Name} | _]) -> Name;
migration_name(_) -> <<>>.

%% ==================================================================
%% Connection pragmas
%% ==================================================================

apply_pragmas(Res, BusyTimeoutMs, JournalMode, ForeignKeys) ->
    Steps = [busy_timeout_pragma(BusyTimeoutMs),
             journal_mode_pragma(JournalMode),
             foreign_keys_pragma(ForeignKeys)],
    run_pragmas(Res, [S || S <- Steps, S =/= skip]).

busy_timeout_pragma(Ms) when is_integer(Ms), Ms > 0 ->
    <<"PRAGMA busy_timeout=", (integer_to_binary(Ms))/binary>>;
busy_timeout_pragma(_) ->
    skip.

journal_mode_pragma(Mode) when is_binary(Mode), Mode =/= <<>> ->
    <<"PRAGMA journal_mode=", Mode/binary>>;
journal_mode_pragma(_) ->
    skip.

foreign_keys_pragma(1) -> <<"PRAGMA foreign_keys=ON">>;
foreign_keys_pragma(_) -> <<"PRAGMA foreign_keys=OFF">>.

run_pragmas(_Res, []) ->
    ok;
run_pragmas(Res, [Sql | Rest]) ->
    case nif_exec(Res, Sql, []) of
        {ok, _} -> run_pragmas(Res, Rest);
        {error, E} -> {error, connect_error(E)}
    end.

%% ==================================================================
%% Handle registry (id -> worker)
%% ==================================================================

registry_call(Req) ->
    registry_ensure(),
    Ref = make_ref(),
    ridge_sqlite_registry ! {Req, self(), Ref},
    receive
        {Ref, Reply} -> Reply
    after 5000 ->
        {error, #{code => <<"db.registry.timeout">>,
                  message => <<"the connection registry timed out">>}}
    end.

registry_ensure() ->
    case whereis(ridge_sqlite_registry) of
        undefined ->
            spawn(fun registry_init/0),
            registry_wait(200);
        _ ->
            ok
    end.

registry_wait(0) -> ok;
registry_wait(N) ->
    case whereis(ridge_sqlite_registry) of
        undefined -> timer:sleep(5), registry_wait(N - 1);
        _ -> ok
    end.

registry_init() ->
    case catch register(ridge_sqlite_registry, self()) of
        true -> registry_loop(#{});
        _ -> ok
    end.

registry_loop(Map) ->
    receive
        {{register, Worker}, From, Ref} ->
            Id = erlang:unique_integer([positive, monotonic]),
            From ! {Ref, Id},
            registry_loop(Map#{Id => Worker});
        {{lookup, Id}, From, Ref} ->
            From ! {Ref, maps:find(Id, Map)},
            registry_loop(Map);
        {{unregister, Id}, From, Ref} ->
            From ! {Ref, ok},
            registry_loop(maps:remove(Id, Map))
    end.

%% ==================================================================
%% SqlValue <-> storage-class cell mapping
%% ==================================================================

to_cells(Params) -> [to_cell(P) || P <- Params].

%% Encode a SqlValue to the storage a round-trip needs. Rich types collapse to
%% the storage class that preserves them: a bool to an integer 0/1, a timestamp
%% to ISO-8601 UTC text, a decimal and a uuid to their exact text, a byte string
%% to a blob, an interval to whole milliseconds, an array to JSON text.
to_cell({'SqlInt', N}) -> {int, N};
to_cell({'SqlText', T}) -> {text, T};
to_cell({'SqlBool', true}) -> {int, 1};
to_cell({'SqlBool', false}) -> {int, 0};
to_cell({'SqlFloat', F}) -> {float, F};
to_cell('SqlNull') -> null;
to_cell({'SqlInstant', Micros}) -> {text, iso8601_micros(Micros)};
to_cell({'SqlDecimal', S}) -> {text, S};
to_cell({'SqlUuid', S}) -> {text, S};
to_cell({'SqlBytes', Hex}) -> {blob, hex_to_bin(Hex)};
to_cell({'SqlJson', S}) -> {text, S};
to_cell({'SqlDate', S}) -> {text, S};
to_cell({'SqlTime', S}) -> {text, S};
to_cell({'SqlInterval', Ms}) -> {int, Ms};
to_cell({'SqlArray', Elems}) -> {text, encode_array(Elems)}.

%% Decode a result cell to a RAW SqlValue by storage class. The column codec one
%% layer up reconstructs a rich type (a Bool, a Timestamp) from this when the
%% Ridge column type calls for it.
build_row(Cols, Cells) ->
    maps:from_list(lists:zipwith(fun(C, V) -> {C, from_cell(V)} end, Cols, Cells)).

from_cell({int, N}) -> {'SqlInt', N};
from_cell({float, F}) -> {'SqlFloat', F};
from_cell({text, T}) -> {'SqlText', T};
from_cell({blob, B}) -> {'SqlBytes', bin_to_hex(B)};
from_cell(null) -> 'SqlNull'.

%% An epoch-microsecond instant as ISO-8601 UTC text, lexicographically ordered
%% and readable, matching how the Postgres client renders a timestamp parameter.
iso8601_micros(Micros) ->
    list_to_binary(
        calendar:system_time_to_rfc3339(Micros, [{unit, microsecond}, {offset, "Z"}])).

hex_to_bin(Hex) -> binary:decode_hex(Hex).

bin_to_hex(Bin) -> binary:encode_hex(Bin, lowercase).

%% Encode an array to JSON text (SQLite has no native array). Each element is
%% rendered by its scalar SqlValue; a nested array or an unhandled element is a
%% loud error rather than a silent bad value.
encode_array(Elems) ->
    iolist_to_binary(json:encode([json_scalar(E) || E <- Elems])).

json_scalar({'SqlInt', N}) -> N;
json_scalar({'SqlFloat', F}) -> F;
json_scalar({'SqlText', T}) -> T;
json_scalar({'SqlBool', B}) -> B;
json_scalar('SqlNull') -> null;
json_scalar({'SqlDecimal', S}) -> S;
json_scalar({'SqlUuid', S}) -> S;
json_scalar({'SqlDate', S}) -> S;
json_scalar({'SqlTime', S}) -> S;
json_scalar({'SqlInstant', Micros}) -> iso8601_micros(Micros);
json_scalar({'SqlInterval', Ms}) -> Ms.

%% ==================================================================
%% Error mapping
%% ==================================================================

%% A query-side native error as a raw storage error the stdlib classifies via
%% dbErrorKind. A SQLite constraint code maps to the matching Postgres SQLSTATE
%% so a unique or foreign-key violation reads the same on either backend; any
%% other code keeps its number under a db.error prefix, which reads as a general
%% query error.
db_error({sqlite_error, Code, Msg}) ->
    #{code => sqlite_code(Code), message => Msg};
db_error({bad_param, _Term}) ->
    #{code => <<"db.error.badparam">>,
      message => <<"a bound parameter was not a valid value">>};
db_error(closed) ->
    closed_handle_error();
db_error(Other) ->
    #{code => <<"db.error.unknown">>,
      message => iolist_to_binary(io_lib:format("~p", [Other]))}.

%% A connect-side native error is a connection fault (db.* but not db.error.*).
connect_error({sqlite_error, _Code, Msg}) ->
    #{code => <<"db.conn.open">>, message => Msg};
connect_error(Other) ->
    db_error(Other).

closed_handle_error() ->
    #{code => <<"db.conn.closed">>, message => <<"connection handle not found">>}.

sqlite_code(2067) -> <<"db.error.23505">>;
sqlite_code(1555) -> <<"db.error.23505">>;
sqlite_code(787) -> <<"db.error.23503">>;
sqlite_code(1299) -> <<"db.error.23502">>;
sqlite_code(275) -> <<"db.error.23514">>;
sqlite_code(Code) ->
    <<"db.error.sqlite.", (integer_to_binary(Code))/binary>>.

%% ==================================================================
%% Small helpers
%% ==================================================================

%% Map a worker's {ok, _} reply to the Unit a transaction/close verb answers.
tx_unit({ok, _}) -> {ok, ok};
tx_unit({error, E}) -> {error, E}.

%% Map a raw-exec reply to Unit for the migration record verbs.
unit({ok, _}) -> {ok, ok};
unit({error, E}) -> {error, E}.

%% Quote a SQL identifier with double quotes, doubling any embedded quote.
quote_ident(Ident) ->
    Bin = to_bin(Ident),
    Escaped = binary:replace(Bin, <<"\"">>, <<"\"\"">>, [global]),
    <<"\"", Escaped/binary, "\"">>.

to_bin(B) when is_binary(B) -> B;
to_bin(IoList) -> iolist_to_binary(IoList).

%% ==================================================================
%% NIF stubs — replaced by the native implementation on load
%% ==================================================================

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
