%% ridge_pg — a first-party PostgreSQL client for the std.data Postgres adapter.
%%
%% Speaks the PostgreSQL frontend/backend protocol (version 3) directly over
%% gen_tcp, with an optional TLS upgrade through the OTP `ssl` application. It is
%% the real-database dual of the in-memory keeper in ridge_rt.erl: a connection
%% is a process that owns one socket and serialises every request, and the
%% adapter verbs cross the boundary in the same shapes the rest of the stdlib
%% already speaks — rows are `#{<<"col">> => SqlValue}` maps, predicates are
%% QExpr tagged tuples, and every result is `{ok, V}` / `{error, ErrorMap}`.
%%
%% Values are always sent as bind parameters of an extended-protocol query, so a
%% predicate or a row value can never be interpolated into SQL text. Only table
%% and column identifiers are rendered into the statement, and those are quoted.
%%
%% Each handle owns a pool, not a lone socket. `connect` opens and authenticates
%% one connection up front — so a bad host, password, or TLS setting fails fast —
%% and a pool manager process grows the pool lazily up to `poolSize` as
%% concurrent callers ask for connections. A verb checks a connection out, talks
%% to it directly (so N callers run N queries at once), and checks it back in.
%% The manager monitors every connection: one that drops is removed and replaced
%% on the next checkout, so a database restart heals without a reconnect storm.
%%
%% Transport faults and SQL errors are kept apart. A server-side SQL error
%% (`{pg_error, _}`) leaves the socket healthy and reusable; a transport fault
%% (`{pg_fatal, _}`) kills the connection process so a half-broken socket is
%% never handed out again. A query that races a dropped connection therefore
%% fails rather than retrying silently — the driver cannot know whether a write
%% committed, so retrying is the caller's decision.

-module(ridge_pg).

-export([
    pg_connect/7,
    pg_insert/3,
    pg_all/2,
    pg_select/3,
    pg_get_rows/4,
    pg_delete/3,
    pg_update/4,
    pg_fetch/6,
    pg_count_where/3,
    pg_aggregate/5,
    pg_project/7,
    pg_join/8,
    pg_join_select/9,
    pg_left_join/8,
    pg_left_join_select/9,
    pg_close/1
]).

-define(CONNECT_TIMEOUT, 10000).
-define(RECV_TIMEOUT, 15000).
-define(QUERY_TIMEOUT, 30000).
-define(CHECKOUT_TIMEOUT, 5000).

%% A live connection is `{Transport, Socket}` where Transport is `gen_tcp` or
%% `ssl`; both expose the same send/recv/close surface, so transport-specific
%% code is confined to xsend/2, xrecv/2 and transport_close/1.

%% --- FFI surface (mirrors the MemAdapter verbs in ridge_rt.erl) ---

%% pg_connect/7 — std.data.connect. Open and authenticate one connection from the
%% config fields, then start a pool manager seeded with it and return the Ridge
%% handle `#{id => Id}` (the same id-as-handle shape MemAdapter uses). The config
%% crosses the FFI boundary as positional scalars, not a record map, so it never
%% depends on how a Ridge record lowers its keys. Opening one connection now
%% means a bad host, password, or TLS mode is reported here rather than on the
%% first query. Result Postgres Error.
pg_connect(Host, Port, Database, User, Password, SslMode, PoolSize) ->
    application:ensure_all_started(crypto),
    Config = #{host => Host, port => Port, database => Database,
               user => User, password => Password, ssl_mode => SslMode,
               pool_size => clamp_pool(PoolSize)},
    case do_connect(Config) of
        {ok, Conn} ->
            Worker = spawn(fun() -> pg_conn_loop(Conn) end),
            set_controlling(Conn, Worker),
            Pool = spawn(fun() -> pool_init(Config, Worker) end),
            Id = pg_registry_call({register, Pool}),
            {ok, #{id => Id}};
        {error, E} ->
            {error, E}
    end.

%% pg_insert/3 — append Row to Table. Result Unit Error.
pg_insert(Id, Table, Row) -> pg_call(Id, {insert, Table, Row}).

%% pg_all/2 — every row of Table. Result (List Row) Error.
pg_all(Id, Table) -> pg_call(Id, {all, Table}).

%% pg_select/3 — the rows of Table that satisfy the captured predicate Tree.
%% Result (List Row) Error.
pg_select(Id, Table, Tree) -> pg_call(Id, {select, Table, Tree}).

%% pg_get_rows/4 — the rows of Table whose Column holds exactly Key. std.data's
%% `get` takes the first. Result (List Row) Error.
pg_get_rows(Id, Table, Column, Key) -> pg_call(Id, {get_rows, Table, Column, Key}).

%% pg_delete/3 — remove the rows of Table that satisfy Tree; answer how many were
%% removed. Result Int Error.
pg_delete(Id, Table, Tree) -> pg_call(Id, {delete, Table, Tree}).

%% pg_update/4 — set the Changes columns on the rows of Table that satisfy Tree;
%% answer the affected row count. Changes is a `#{Column => SqlValue}` map.
%% Result Int Error.
pg_update(Id, Table, Changes, Tree) -> pg_call(Id, {update, Table, Changes, Tree}).

%% pg_fetch/6 — the rows of Table that satisfy Tree, ordered by Orders, then
%% offset and limited, all pushed into the SQL. Orders is a list of `{Asc, Column}`
%% where Asc is the boolean `true` for ascending. Lim < 0 means no LIMIT and
%% Off =< 0 means no OFFSET. Result (List Row) Error.
pg_fetch(Id, Table, Tree, Orders, Lim, Off) ->
    pg_call(Id, {fetch, Table, Tree, Orders, Lim, Off}).

%% pg_count_where/3 — how many rows of Table satisfy Tree, via SELECT COUNT(*)
%% so no rows cross the wire. Result Int Error.
pg_count_where(Id, Table, Tree) -> pg_call(Id, {count_where, Table, Tree}).

%% pg_aggregate/5 — a scalar aggregate (Func is <<"SUM">>/<<"AVG">>/<<"MIN">>/
%% <<"MAX">>) over Column across the rows of Table that satisfy Tree, via
%% SELECT func(column) … WHERE so only the scalar crosses the wire. An aggregate
%% over zero rows is SQL NULL, which decodes to 'SqlNull'. Result SqlValue Error.
pg_aggregate(Id, Table, Tree, Func, Column) ->
    pg_call(Id, {aggregate, Table, Tree, Func, Column}).

%% pg_project/7 — the rows of Table that satisfy Tree, ordered and paged as
%% pg_fetch, with the `{Alias, Column}` projection compiled into the select-list
%% (`SELECT column AS alias …`); each row comes back keyed by alias. Result
%% (List Row) Error.
pg_project(Id, Table, Tree, Orders, Lim, Off, Cols) ->
    pg_call(Id, {project, Table, Tree, Orders, Lim, Off, Cols}).

%% pg_join/8 — inner-join LeftTable and RightTable on the condition tree Cond,
%% compiled into `JOIN … ON`; the left-side predicate Pred into `WHERE`; then
%% Orders/Lim/Off. Left columns (`QCol`) are qualified to the left table, right
%% columns (`QColR`) to the right. Each row comes back as the `{left, right}`
%% pair of column maps, split by the columns' source table. Result
%% (List {Row, Row}) Error.
pg_join(Id, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off) ->
    pg_call(Id, {join, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off}).

%% pg_join_select/9 — as pg_join, with the projection tree Proj compiled into the
%% select-list (each `QCol`/`QColR` qualified and aliased). Each row is one map
%% keyed by the projection's aliases. Result (List Row) Error.
pg_join_select(Id, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off, Proj) ->
    pg_call(Id, {join_select, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off, Proj}).

%% pg_left_join/8 — as pg_join, compiled to a `LEFT JOIN`. The right table is
%% wrapped in a subquery that tags every real row with a `__ridge_matched`
%% sentinel, so a null-extended (unmatched) row is told apart from a matched row
%% whose columns happen to be NULL. Each row comes back as `{left, {some, right}}`
%% for a match or `{left, none}` for an unmatched left row. Result
%% (List {Row, Option Row}) Error.
pg_left_join(Id, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off) ->
    pg_call(Id, {left_join, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off}).

%% pg_left_join_select/9 — as pg_left_join, with the projection tree Proj compiled
%% into the select-list (each `QCol`/`QColR` qualified and aliased). No sentinel is
%% needed: an unmatched right column comes back NULL and decodes to `None` in the
%% projected shape's `Option` field. Each row is one map keyed by the projection's
%% aliases. Result (List Row) Error.
pg_left_join_select(Id, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off, Proj) ->
    pg_call(Id, {left_join_select, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off, Proj}).

%% pg_close/1 — close every connection in the pool and forget the handle.
%% Result Unit Error.
pg_close(Id) ->
    Reply =
        case pg_registry_call({lookup, Id}) of
            {ok, Pool} ->
                Ref = make_ref(),
                Pool ! {close, self(), Ref},
                receive
                    {Ref, R} -> R
                after 5000 ->
                    {ok, ok}
                end;
            _ ->
                {ok, ok}
        end,
    pg_registry_call({unregister, Id}),
    Reply.

clamp_pool(N) when is_integer(N), N > 0 -> N;
clamp_pool(_)                           -> 1.

%% --- Verb dispatch: check out a connection, run, check it back in ---
%%
%% A verb resolves the handle to its pool, borrows a connection, sends the
%% request straight to that connection process, and returns it to the pool. Two
%% handles never serialise through one another, and two callers on one handle run
%% concurrently across distinct pooled connections.

pg_call(Id, Verb) ->
    case pg_registry_call({lookup, Id}) of
        {ok, Pool} ->
            case pool_checkout(Pool) of
                {ok, Worker} ->
                    Reply = worker_request(Worker, Verb),
                    pool_checkin(Pool, Worker),
                    Reply;
                {error, E} ->
                    {error, E}
            end;
        _ ->
            {error, #{code => <<"db.conn.closed">>,
                      message => <<"connection handle not found">>}}
    end.

%% Send a verb to a borrowed connection and await its reply. A connection that
%% dies mid-request never answers; the timeout turns that into a structured
%% error, and the pool independently drops the dead connection on its DOWN.
worker_request(Worker, Verb) ->
    Ref = make_ref(),
    Worker ! {Verb, self(), Ref},
    receive
        {Ref, Reply} -> Reply
    after ?QUERY_TIMEOUT ->
        {error, #{code => <<"db.timeout">>,
                  message => <<"postgres request timed out">>}}
    end.

%% --- Handle registry ---
%%
%% A single registered process maps an integer handle id to its pool manager.
%% Lookups are cheap; the verb path then borrows a connection from the pool.

pg_registry_call(Req) ->
    pg_registry_ensure(),
    Ref = make_ref(),
    ridge_pg_registry ! {Req, self(), Ref},
    receive
        {Ref, Reply} -> Reply
    after 5000 ->
        {error, #{code => <<"db.registry.timeout">>,
                  message => <<"connection registry timed out">>}}
    end.

pg_registry_ensure() ->
    case whereis(ridge_pg_registry) of
        undefined ->
            spawn(fun pg_registry_init/0),
            pg_registry_wait(200);
        _Pid ->
            ok
    end.

pg_registry_wait(0) -> ok;
pg_registry_wait(N) ->
    case whereis(ridge_pg_registry) of
        undefined -> timer:sleep(5), pg_registry_wait(N - 1);
        _Pid      -> ok
    end.

pg_registry_init() ->
    case catch register(ridge_pg_registry, self()) of
        true -> pg_registry_loop(#{});
        _    -> ok
    end.

pg_registry_loop(Map) ->
    receive
        {{register, Pid}, From, Ref} ->
            Id = erlang:unique_integer([positive, monotonic]),
            From ! {Ref, Id},
            pg_registry_loop(Map#{Id => Pid});
        {{lookup, Id}, From, Ref} ->
            From ! {Ref, maps:find(Id, Map)},
            pg_registry_loop(Map);
        {{unregister, Id}, From, Ref} ->
            From ! {Ref, ok},
            pg_registry_loop(maps:remove(Id, Map))
    end.

%% --- Pool manager ---
%%
%% One manager per handle owns the connection pool. It keeps the live
%% connections partitioned into `idle` (ready to lend) and `busy` (lent out),
%% monitors every one in `mons`, and queues `waiters` that arrived while the pool
%% was at `max` with nothing free. The live count is map_size(mons), so the pool
%% grows by opening a connection only when a checkout finds none idle and the
%% count is still below max.
%%
%% Checkout/checkin and DOWN drive every transition:
%%   - checkout: lend an idle connection, else open one if below max, else wait.
%%   - checkin:  hand the connection to the oldest waiter, else return it to idle.
%%   - DOWN:     forget the connection; if a waiter is parked, open a replacement.
%% A parked waiter is bounded by a manager-side timer, so a caller never blocks
%% past ?CHECKOUT_TIMEOUT and a connection is never lent to a caller that gave up.

pool_init(Config, FirstWorker) ->
    Mon = erlang:monitor(process, FirstWorker),
    State = #{config  => Config,
              max     => maps:get(pool_size, Config, 1),
              idle    => [FirstWorker],
              busy    => #{},
              mons    => #{FirstWorker => Mon},
              waiters => queue:new()},
    pool_loop(State).

pool_loop(State) ->
    receive
        {checkout, ReplyTo, Ref} ->
            pool_loop(handle_checkout(State, ReplyTo, Ref));
        {checkin, Worker} ->
            pool_loop(handle_checkin(State, Worker));
        {checkout_cancel, Ref} ->
            pool_loop(cancel_waiter(State, Ref));
        {'DOWN', _MonRef, process, Worker, _Reason} ->
            pool_loop(handle_down(State, Worker));
        {timeout, _TimerRef, {waiter_timeout, Ref}} ->
            pool_loop(timeout_waiter(State, Ref));
        {close, ReplyTo, Ref} ->
            close_all(State),
            ReplyTo ! {Ref, {ok, ok}}
    end.

%% Lend an idle connection if one is live; otherwise open a fresh one when the
%% pool has headroom, and park the caller as a waiter when it does not.
handle_checkout(State, ReplyTo, Ref) ->
    #{idle := Idle, busy := Busy, mons := Mons, max := Max} = State,
    case take_live(Idle) of
        {ok, Worker, Rest} ->
            ReplyTo ! {Ref, {ok, Worker}},
            State#{idle := Rest, busy := Busy#{Worker => true}};
        {none, Rest} ->
            case maps:size(Mons) < Max of
                true ->
                    case open_worker(State) of
                        {ok, Worker, Mon} ->
                            ReplyTo ! {Ref, {ok, Worker}},
                            State#{idle := Rest,
                                   busy := Busy#{Worker => true},
                                   mons := Mons#{Worker => Mon}};
                        {error, E} ->
                            ReplyTo ! {Ref, {error, E}},
                            State#{idle := Rest}
                    end;
                false ->
                    enqueue_waiter(State#{idle := Rest}, ReplyTo, Ref)
            end
    end.

%% Return a borrowed connection. An unknown one (already dropped on its DOWN) is
%% ignored, and a dead one is dropped rather than parked back in idle. A live one
%% is handed to the oldest waiter if any, else parked as idle.
handle_checkin(State, Worker) ->
    #{busy := Busy, idle := Idle, waiters := Waiters} = State,
    case maps:is_key(Worker, Busy) of
        false ->
            State;
        true ->
            case is_process_alive(Worker) of
                false ->
                    State#{busy := maps:remove(Worker, Busy)};
                true ->
                    case queue:out(Waiters) of
                        {{value, {Ref, ReplyTo, Timer}}, Waiters1} ->
                            erlang:cancel_timer(Timer),
                            ReplyTo ! {Ref, {ok, Worker}},
                            State#{waiters := Waiters1};
                        {empty, _} ->
                            State#{busy := maps:remove(Worker, Busy),
                                   idle := [Worker | Idle]}
                    end
            end
    end.

%% A connection died. Forget it everywhere; if a caller is parked and the pool
%% now has headroom, open a replacement to serve the oldest waiter.
handle_down(State, Worker) ->
    #{idle := Idle, busy := Busy, mons := Mons} = State,
    State1 = State#{mons := maps:remove(Worker, Mons),
                    idle := lists:delete(Worker, Idle),
                    busy := maps:remove(Worker, Busy)},
    serve_waiter(State1).

serve_waiter(State) ->
    #{waiters := Waiters, mons := Mons, busy := Busy, max := Max} = State,
    case queue:out(Waiters) of
        {empty, _} ->
            State;
        {{value, {Ref, ReplyTo, Timer}}, Waiters1} ->
            case maps:size(Mons) < Max of
                false ->
                    State;
                true ->
                    erlang:cancel_timer(Timer),
                    case open_worker(State) of
                        {ok, Worker, Mon} ->
                            ReplyTo ! {Ref, {ok, Worker}},
                            State#{waiters := Waiters1,
                                   busy := Busy#{Worker => true},
                                   mons := Mons#{Worker => Mon}};
                        {error, E} ->
                            ReplyTo ! {Ref, {error, E}},
                            State#{waiters := Waiters1}
                    end
            end
    end.

%% Pop the first live connection from the idle list, dropping any that died
%% between being parked and now. Returns {ok, Worker, Rest} or {none, []}.
take_live([]) ->
    {none, []};
take_live([Worker | Rest]) ->
    case is_process_alive(Worker) of
        true  -> {ok, Worker, Rest};
        false -> take_live(Rest)
    end.

enqueue_waiter(State, ReplyTo, Ref) ->
    #{waiters := Waiters} = State,
    Timer = erlang:start_timer(?CHECKOUT_TIMEOUT, self(), {waiter_timeout, Ref}),
    State#{waiters := queue:in({Ref, ReplyTo, Timer}, Waiters)}.

%% The manager-side checkout timer fired: drop the waiter and tell its caller.
%% Idempotent — a waiter already served (its timer cancelled, but the timeout
%% message may have raced ahead) is simply not found.
timeout_waiter(State, Ref) ->
    #{waiters := Waiters} = State,
    case remove_waiter(Waiters, Ref) of
        {{Ref, ReplyTo, _Timer}, Waiters1} ->
            ReplyTo ! {Ref, {error, #{code => <<"db.pool.timeout">>,
                                      message => <<"connection pool checkout timed out">>}}},
            State#{waiters := Waiters1};
        notfound ->
            State
    end.

%% Caller-side backstop cancel: drop the waiter without answering it.
cancel_waiter(State, Ref) ->
    #{waiters := Waiters} = State,
    case remove_waiter(Waiters, Ref) of
        {{Ref, _ReplyTo, Timer}, Waiters1} ->
            erlang:cancel_timer(Timer),
            State#{waiters := Waiters1};
        notfound ->
            State
    end.

remove_waiter(Waiters, Ref) ->
    case lists:keytake(Ref, 1, queue:to_list(Waiters)) of
        {value, Waiter, Rest} -> {Waiter, queue:from_list(Rest)};
        false                 -> notfound
    end.

%% Open one connection, spawn its process, hand the socket over, and monitor it.
%% The manager owns the socket between connect and the handover, so the transfer
%% is always valid.
open_worker(#{config := Config}) ->
    case do_connect(Config) of
        {ok, Conn} ->
            Worker = spawn(fun() -> pg_conn_loop(Conn) end),
            set_controlling(Conn, Worker),
            Mon = erlang:monitor(process, Worker),
            {ok, Worker, Mon};
        {error, E} ->
            {error, E}
    end.

%% Tear the pool down: stop monitoring and shut every connection (its socket
%% closes with the process), then fail any parked waiters.
close_all(State) ->
    #{mons := Mons, waiters := Waiters} = State,
    maps:foreach(
        fun(Worker, Mon) ->
            erlang:demonitor(Mon, [flush]),
            exit(Worker, shutdown)
        end, Mons),
    lists:foreach(
        fun({Ref, ReplyTo, Timer}) ->
            erlang:cancel_timer(Timer),
            ReplyTo ! {Ref, {error, #{code => <<"db.conn.closed">>,
                                      message => <<"connection pool closed">>}}}
        end, queue:to_list(Waiters)),
    ok.

%% --- Pool client helpers (run in the calling process) ---

pool_checkout(Pool) ->
    Ref = make_ref(),
    Pool ! {checkout, self(), Ref},
    receive
        {Ref, Reply} -> Reply
    after ?CHECKOUT_TIMEOUT + 1000 ->
        %% Backstop only: the manager enforces ?CHECKOUT_TIMEOUT and replies
        %% first. Cancel in case the reply was lost (a dead manager, say).
        Pool ! {checkout_cancel, Ref},
        {error, #{code => <<"db.pool.timeout">>,
                  message => <<"connection pool checkout timed out">>}}
    end.

pool_checkin(Pool, Worker) ->
    Pool ! {checkin, Worker},
    ok.

%% --- Connection process ---
%%
%% Owns one socket and runs every request to completion before taking the next,
%% so one connection is never used concurrently. A SQL error is returned and the
%% socket lives on; a transport fault (`pg_fatal`) is answered once and then ends
%% the process, so the pool drops the broken socket on its DOWN rather than
%% lending it out again.

pg_conn_loop(Conn) ->
    receive
        {Verb, From, Ref} ->
            try run_verb(Conn, Verb) of
                Reply ->
                    From ! {Ref, Reply},
                    pg_conn_loop(Conn)
            catch
                throw:{pg_fatal, E} ->
                    From ! {Ref, {error, E}},
                    transport_close(Conn)
            end
    end.

run_verb(Conn, {insert, Table, Row}) ->
    do_insert(Conn, Table, Row);
run_verb(Conn, {all, Table}) ->
    run_query(Conn, ["SELECT * FROM ", quote_ident(Table)], []);
run_verb(Conn, {select, Table, Tree}) ->
    {Where, Binds} = compile_where(Tree),
    run_query(Conn, ["SELECT * FROM ", quote_ident(Table), " WHERE ", Where], Binds);
run_verb(Conn, {get_rows, Table, Column, Key}) ->
    Sql = ["SELECT * FROM ", quote_ident(Table), " WHERE ", quote_ident(Column), " = $1"],
    run_query(Conn, Sql, [Key]);
run_verb(Conn, {delete, Table, Tree}) ->
    {Where, Binds} = compile_where(Tree),
    do_exec(Conn, ["DELETE FROM ", quote_ident(Table), " WHERE ", Where], Binds);
run_verb(Conn, {update, Table, Changes, Tree}) ->
    do_update(Conn, Table, Changes, Tree);
run_verb(Conn, {fetch, Table, Tree, Orders, Lim, Off}) ->
    {Where, Binds} = compile_where(Tree),
    Sql = ["SELECT * FROM ", quote_ident(Table), " WHERE ", Where,
           order_by_clause(Orders), limit_clause(Lim), offset_clause(Off)],
    run_query(Conn, Sql, Binds);
run_verb(Conn, {project, Table, Tree, Orders, Lim, Off, Cols}) ->
    {Where, Binds} = compile_where(Tree),
    Sql = ["SELECT ", select_list(Cols), " FROM ", quote_ident(Table), " WHERE ", Where,
           order_by_clause(Orders), limit_clause(Lim), offset_clause(Off)],
    run_query(Conn, Sql, Binds);
run_verb(Conn, {join, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off}) ->
    {OnFrag, RevB1, N1} = cwj(Cond, 1, []),
    {WhereFrag, RevB2, _N2} = cwj(Pred, N1, RevB1),
    Sql = ["SELECT l.*, r.* FROM ", quote_ident(LeftTable), " AS l JOIN ",
           quote_ident(RightTable), " AS r ON ", OnFrag, " WHERE ", WhereFrag,
           order_by_clause_join(Orders), limit_clause(Lim), offset_clause(Off)],
    run_query_join(Conn, Sql, lists:reverse(RevB2));
run_verb(Conn, {join_select, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off, Proj}) ->
    {OnFrag, RevB1, N1} = cwj(Cond, 1, []),
    {WhereFrag, RevB2, _N2} = cwj(Pred, N1, RevB1),
    Sql = ["SELECT ", join_select_list(Proj), " FROM ", quote_ident(LeftTable), " AS l JOIN ",
           quote_ident(RightTable), " AS r ON ", OnFrag, " WHERE ", WhereFrag,
           order_by_clause_join(Orders), limit_clause(Lim), offset_clause(Off)],
    run_query(Conn, Sql, lists:reverse(RevB2));
run_verb(Conn, {left_join, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off}) ->
    {OnFrag, RevB1, N1} = cwj(Cond, 1, []),
    {WhereFrag, RevB2, _N2} = cwj(Pred, N1, RevB1),
    %% The right table is wrapped in a subquery that adds a constant TRUE column;
    %% the LEFT JOIN null-extends it to NULL for unmatched rows, so the sentinel
    %% tells a real right row (even one with all-NULL columns) from a missing one.
    Sql = ["SELECT l.*, r.* FROM ", quote_ident(LeftTable),
           " AS l LEFT JOIN (SELECT *, TRUE AS \"__ridge_matched\" FROM ",
           quote_ident(RightTable), ") AS r ON ", OnFrag, " WHERE ", WhereFrag,
           order_by_clause_join(Orders), limit_clause(Lim), offset_clause(Off)],
    run_query_left_join(Conn, Sql, lists:reverse(RevB2));
run_verb(Conn, {left_join_select, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off, Proj}) ->
    {OnFrag, RevB1, N1} = cwj(Cond, 1, []),
    {WhereFrag, RevB2, _N2} = cwj(Pred, N1, RevB1),
    %% A projection decodes each aliased column on its own, so no sentinel is
    %% needed: an unmatched right column comes back NULL and decodes to `None` in
    %% the projected shape's `Option` field. Just a `LEFT JOIN` of the two tables.
    Sql = ["SELECT ", join_select_list(Proj), " FROM ", quote_ident(LeftTable), " AS l LEFT JOIN ",
           quote_ident(RightTable), " AS r ON ", OnFrag, " WHERE ", WhereFrag,
           order_by_clause_join(Orders), limit_clause(Lim), offset_clause(Off)],
    run_query(Conn, Sql, lists:reverse(RevB2));
run_verb(Conn, {count_where, Table, Tree}) ->
    do_count(Conn, Table, Tree);
run_verb(Conn, {aggregate, Table, Tree, Func, Column}) ->
    do_aggregate(Conn, Table, Func, Column, Tree).

do_insert(Conn, Table, Row) ->
    Pairs = maps:to_list(Row),
    Cols = lists:join(",", [quote_ident(C) || {C, _V} <- Pairs]),
    {Placeholders, _} =
        lists:mapfoldl(fun(_, N) -> {[$$ | integer_to_list(N)], N + 1} end, 1, Pairs),
    Binds = [V || {_C, V} <- Pairs],
    Sql = ["INSERT INTO ", quote_ident(Table), " (", Cols, ") VALUES (",
           lists:join(",", Placeholders), ")"],
    case do_exec(Conn, Sql, Binds) of
        {ok, _Count} -> {ok, ok};
        {error, E}   -> {error, E}
    end.

%% UPDATE Table SET col = $1, … WHERE <Tree>. The SET binds take placeholders
%% $1..$K in column order; the WHERE clause is compiled starting at $K+1, seeded
%% with the SET binds (held reversed, the order `cw` accumulates in), so the two
%% placeholder runs never collide. An empty Changes map cannot form a valid SET,
%% so it is a no-op reporting zero rows changed — matching the in-memory store.
do_update(_Conn, _Table, Changes, _Tree) when map_size(Changes) =:= 0 ->
    {ok, 0};
do_update(Conn, Table, Changes, Tree) ->
    Pairs = maps:to_list(Changes),
    {SetFragsRev, SetBindsRev, NextN} =
        lists:foldl(
            fun({Col, Val}, {Frags, Binds, N}) ->
                Frag = [quote_ident(Col), " = $", integer_to_list(N)],
                {[Frag | Frags], [Val | Binds], N + 1}
            end,
            {[], [], 1},
            Pairs),
    SetClause = lists:join(", ", lists:reverse(SetFragsRev)),
    {WhereFrag, RevAllBinds, _N} = cw(Tree, NextN, SetBindsRev),
    Sql = ["UPDATE ", quote_ident(Table), " SET ", SetClause, " WHERE ", WhereFrag],
    do_exec(Conn, Sql, lists:reverse(RevAllBinds)).

%% SELECT COUNT(*) and read the single integer back out. The result is one row
%% of one column; its name varies, so the value is taken positionally.
do_count(Conn, Table, Tree) ->
    {Where, Binds} = compile_where(Tree),
    Sql = ["SELECT COUNT(*) FROM ", quote_ident(Table), " WHERE ", Where],
    case run_query(Conn, Sql, Binds) of
        {ok, [Row | _]} ->
            case maps:values(Row) of
                [{'SqlInt', N} | _]   -> {ok, N};
                [{'SqlFloat', F} | _] -> {ok, trunc(F)};
                _                     -> {ok, 0}
            end;
        {ok, []}   -> {ok, 0};
        {error, E} -> {error, E}
    end.

%% SELECT func(col) … WHERE and read the single scalar back out positionally (its
%% column name is the aggregate keyword, which varies). An aggregate always
%% returns one row; over zero matching rows its single column is NULL, decoded to
%% 'SqlNull'. Func is whitelisted to the four aggregate keywords and Column is
%% quoted as an identifier, so neither is ever interpolated as raw SQL.
do_aggregate(Conn, Table, Func, Column, Tree) ->
    {Where, Binds} = compile_where(Tree),
    Sql = ["SELECT ", agg_expr(Func, Column), " FROM ", quote_ident(Table),
           " WHERE ", Where],
    case run_query(Conn, Sql, Binds) of
        {ok, [Row | _]} ->
            case maps:values(Row) of
                ['SqlNull' | _] -> {ok, none};
                [V | _]         -> {ok, {some, V}};
                []              -> {ok, none}
            end;
        {ok, []}   -> {ok, none};
        {error, E} -> {error, E}
    end.

%% The aggregate select expression. The function name is matched against the four
%% supported keywords (never spliced from the caller's bytes); an unknown keyword
%% falls back to COUNT, which the typed surface never produces. AVG is cast to
%% float8 so an integer column's average crosses the wire as a float, matching the
%% `Float` result the repository verb decodes.
agg_expr(<<"AVG">>, Column) -> ["AVG(", quote_ident(Column), ")::float8"];
agg_expr(<<"SUM">>, Column) -> ["SUM(", quote_ident(Column), ")"];
agg_expr(<<"MIN">>, Column) -> ["MIN(", quote_ident(Column), ")"];
agg_expr(<<"MAX">>, Column) -> ["MAX(", quote_ident(Column), ")"];
agg_expr(_Other, Column)    -> ["COUNT(", quote_ident(Column), ")"].

%% ORDER BY / LIMIT / OFFSET fragments. Identifiers are quoted; the limit and
%% offset are integers from the typed surface, so they render inline without a
%% bind. An empty order list, a negative limit, or a non-positive offset each
%% contributes nothing.
order_by_clause([])     -> [];
order_by_clause(Orders) -> [" ORDER BY ", lists:join(", ", [order_term(O) || O <- Orders])].

%% Each order key carries `Asc` as the boolean `true`.
order_term({Asc, Col}) -> [quote_ident(Col), " ", dir_keyword(Asc)].

%% Projection select-list from `{Alias, Column}` pairs. A field whose alias
%% matches its source column is emitted bare; otherwise as `column AS alias`. An
%% empty list (a projection that captured no columns) falls back to `*`.
select_list([])   -> "*";
select_list(Cols) -> lists:join(", ", [select_term(C) || C <- Cols]).

select_term({Alias, Col}) when Alias =:= Col -> quote_ident(Col);
select_term({Alias, Col})                    -> [quote_ident(Col), " AS ", quote_ident(Alias)].

%% --- Inner-join SQL fragments ---
%%
%% A join qualifies every column: a `QCol` belongs to the left table (aliased
%% `l`), a `QColR` to the right (`r`). `cwj` is the join-aware dual of `cw`,
%% emitting `l."col"` / `r."col"` instead of a bare identifier; the rest of the
%% predicate structure and the bind threading are identical.

cwj({'QAnd', L, R}, N, B) ->
    {FL, B1, N1} = cwj(L, N, B),
    {FR, B2, N2} = cwj(R, N1, B1),
    {["(", FL, " AND ", FR, ")"], B2, N2};
cwj({'QOr', L, R}, N, B) ->
    {FL, B1, N1} = cwj(L, N, B),
    {FR, B2, N2} = cwj(R, N1, B1),
    {["(", FL, " OR ", FR, ")"], B2, N2};
cwj({'QNot', X}, N, B) ->
    {FX, B1, N1} = cwj(X, N, B),
    {["(NOT ", FX, ")"], B1, N1};
cwj({'QEq', L, R}, N, B) -> cwj_cmp("=", L, R, N, B);
cwj({'QNe', L, R}, N, B) -> cwj_cmp("<>", L, R, N, B);
cwj({'QLt', L, R}, N, B) -> cwj_cmp("<", L, R, N, B);
cwj({'QGt', L, R}, N, B) -> cwj_cmp(">", L, R, N, B);
cwj({'QLe', L, R}, N, B) -> cwj_cmp("<=", L, R, N, B);
cwj({'QGe', L, R}, N, B) -> cwj_cmp(">=", L, R, N, B);
cwj({'QCol', C}, N, B) -> {qcol_left(C), B, N};
cwj({'QColR', C}, N, B) -> {qcol_right(C), B, N};
cwj({'QLitBool', true}, N, B) -> {"TRUE", B, N};
cwj({'QLitBool', false}, N, B) -> {"FALSE", B, N};
cwj(Other, N, B) -> cwj_operand(Other, N, B).

cwj_cmp(Op, L, R, N, B) ->
    {FL, B1, N1} = cwj_operand(L, N, B),
    {FR, B2, N2} = cwj_operand(R, N1, B1),
    {[FL, " ", Op, " ", FR], B2, N2}.

cwj_operand({'QCol', C}, N, B)        -> {qcol_left(C), B, N};
cwj_operand({'QColR', C}, N, B)       -> {qcol_right(C), B, N};
cwj_operand({'QLitInt', V}, N, B)     -> {[$$ | integer_to_list(N)], [{'SqlInt', V} | B], N + 1};
cwj_operand({'QLitText', V}, N, B)    -> {[$$ | integer_to_list(N)], [{'SqlText', V} | B], N + 1};
cwj_operand({'QLitBool', V}, N, B)    -> {[$$ | integer_to_list(N)], [{'SqlBool', V} | B], N + 1};
cwj_operand({'QLitFloat', V}, N, B)   -> {[$$ | integer_to_list(N)], [{'SqlFloat', V} | B], N + 1};
cwj_operand(_Other, N, B)             -> {"NULL", B, N}.

qcol_left(C)  -> [$l, $., quote_ident(C)].
qcol_right(C) -> [$r, $., quote_ident(C)].

%% ORDER BY over a join orders by the left query's keys, qualified to `l`.
order_by_clause_join([])     -> [];
order_by_clause_join(Orders) -> [" ORDER BY ", lists:join(", ", [order_term_join(O) || O <- Orders])].

order_term_join({Asc, Col}) -> [qcol_left(Col), " ", dir_keyword(Asc)].

%% A join projection select-list from a `QProj` of `{Alias, Column}` cells: each
%% column is qualified by its side and aliased to the output field. An empty
%% projection falls back to every column of both sides.
join_select_list({'QProj', []})   -> "l.*, r.*";
join_select_list({'QProj', Cols}) -> lists:join(", ", [join_select_term(C) || C <- Cols]);
join_select_list(_Other)          -> "l.*, r.*".

join_select_term({Alias, {'QCol', Col}})  -> [qcol_left(Col), " AS ", quote_ident(Alias)];
join_select_term({Alias, {'QColR', Col}}) -> [qcol_right(Col), " AS ", quote_ident(Alias)];
join_select_term({Alias, _Other})         -> ["NULL AS ", quote_ident(Alias)].

dir_keyword(true)  -> "ASC";
dir_keyword(false) -> "DESC";
dir_keyword(_)     -> "ASC".

limit_clause(Lim) when is_integer(Lim), Lim >= 0 -> [" LIMIT ", integer_to_list(Lim)];
limit_clause(_)                                  -> [].

offset_clause(Off) when is_integer(Off), Off > 0 -> [" OFFSET ", integer_to_list(Off)];
offset_clause(_)                                 -> [].

%% --- QExpr -> parameterised WHERE clause ---
%%
%% The SQL dual of mem_pred/2 in ridge_rt.erl: a column becomes a quoted
%% identifier, a literal becomes a `$N` placeholder with its value pushed onto
%% the ordered bind list, and the boolean/comparison nodes nest into a fragment.

compile_where(Tree) ->
    {Frag, RevBinds, _N} = cw(Tree, 1, []),
    {Frag, lists:reverse(RevBinds)}.

cw({'QAnd', L, R}, N, B) ->
    {FL, B1, N1} = cw(L, N, B),
    {FR, B2, N2} = cw(R, N1, B1),
    {["(", FL, " AND ", FR, ")"], B2, N2};
cw({'QOr', L, R}, N, B) ->
    {FL, B1, N1} = cw(L, N, B),
    {FR, B2, N2} = cw(R, N1, B1),
    {["(", FL, " OR ", FR, ")"], B2, N2};
cw({'QNot', X}, N, B) ->
    {FX, B1, N1} = cw(X, N, B),
    {["(NOT ", FX, ")"], B1, N1};
cw({'QEq', L, R}, N, B) -> cw_cmp("=", L, R, N, B);
cw({'QNe', L, R}, N, B) -> cw_cmp("<>", L, R, N, B);
cw({'QLt', L, R}, N, B) -> cw_cmp("<", L, R, N, B);
cw({'QGt', L, R}, N, B) -> cw_cmp(">", L, R, N, B);
cw({'QLe', L, R}, N, B) -> cw_cmp("<=", L, R, N, B);
cw({'QGe', L, R}, N, B) -> cw_cmp(">=", L, R, N, B);
cw({'QCol', C}, N, B) -> {quote_ident(C), B, N};
cw({'QLitBool', true}, N, B) -> {"TRUE", B, N};
cw({'QLitBool', false}, N, B) -> {"FALSE", B, N};
cw(Other, N, B) -> cw_operand(Other, N, B).

cw_cmp(Op, L, R, N, B) ->
    {FL, B1, N1} = cw_operand(L, N, B),
    {FR, B2, N2} = cw_operand(R, N1, B1),
    {[FL, " ", Op, " ", FR], B2, N2}.

cw_operand({'QCol', C}, N, B) ->
    {quote_ident(C), B, N};
cw_operand({'QLitInt', V}, N, B) ->
    {[$$ | integer_to_list(N)], [{'SqlInt', V} | B], N + 1};
cw_operand({'QLitText', V}, N, B) ->
    {[$$ | integer_to_list(N)], [{'SqlText', V} | B], N + 1};
cw_operand({'QLitBool', V}, N, B) ->
    {[$$ | integer_to_list(N)], [{'SqlBool', V} | B], N + 1};
cw_operand({'QLitFloat', V}, N, B) ->
    {[$$ | integer_to_list(N)], [{'SqlFloat', V} | B], N + 1};
cw_operand(_Other, N, B) ->
    {"NULL", B, N}.

quote_ident(Name) ->
    Escaped = binary:replace(to_bin_text(Name), <<"\"">>, <<"\"\"">>, [global]),
    [$", Escaped, $"].

to_bin_text(B) when is_binary(B) -> B;
to_bin_text(L) when is_list(L)   -> list_to_binary(L).

%% --- Query / exec round trips (extended protocol) ---

run_query(Conn, Sql, Binds) ->
    try
        send_extended(Conn, iolist_to_binary(Sql), Binds),
        collect_rows(Conn, [], [])
    catch
        throw:{pg_error, E} ->
            drain_until_ready(Conn),
            {error, E}
    end.

run_exec(Conn, Sql, Binds) ->
    try
        send_extended(Conn, iolist_to_binary(Sql), Binds),
        collect_exec(Conn, <<>>)
    catch
        throw:{pg_error, E} ->
            drain_until_ready(Conn),
            {error, E}
    end.

do_exec(Conn, Sql, Binds) -> run_exec(Conn, Sql, Binds).

%% A join query returns each row as the `{left, right}` pair of column maps. The
%% row description carries every column's source table and attribute number; the
%% select is `l.*, r.*`, so the left columns come first in attribute order, then
%% the right's. The split is at the point where the attribute number resets.
run_query_join(Conn, Sql, Binds) ->
    try
        send_extended(Conn, iolist_to_binary(Sql), Binds),
        collect_rows_join(Conn, [], [])
    catch
        throw:{pg_error, E} ->
            drain_until_ready(Conn),
            {error, E}
    end.

collect_rows_join(Conn, Cols, Acc) ->
    case recv_msg(Conn) of
        {$1, _} -> collect_rows_join(Conn, Cols, Acc);
        {$2, _} -> collect_rows_join(Conn, Cols, Acc);
        {$T, P} -> collect_rows_join(Conn, decode_row_desc_join(P), Acc);
        {$n, _} -> collect_rows_join(Conn, Cols, Acc);
        {$D, P} -> collect_rows_join(Conn, Cols, [decode_data_row_join(P, Cols) | Acc]);
        {$C, _} -> collect_rows_join(Conn, Cols, Acc);
        {$Z, _} -> {ok, lists:reverse(Acc)};
        {_, _}  -> collect_rows_join(Conn, Cols, Acc)
    end.

%% As run_query_join, but the right side is the sentinel-tagged subquery, so each
%% row decodes the right map into `{some, _}` when the `__ridge_matched` marker is
%% set and `none` when the left row was null-extended.
run_query_left_join(Conn, Sql, Binds) ->
    try
        send_extended(Conn, iolist_to_binary(Sql), Binds),
        collect_rows_left_join(Conn, [], [])
    catch
        throw:{pg_error, E} ->
            drain_until_ready(Conn),
            {error, E}
    end.

collect_rows_left_join(Conn, Cols, Acc) ->
    case recv_msg(Conn) of
        {$1, _} -> collect_rows_left_join(Conn, Cols, Acc);
        {$2, _} -> collect_rows_left_join(Conn, Cols, Acc);
        {$T, P} -> collect_rows_left_join(Conn, decode_row_desc_join(P), Acc);
        {$n, _} -> collect_rows_left_join(Conn, Cols, Acc);
        {$D, P} -> collect_rows_left_join(Conn, Cols, [decode_data_row_left_join(P, Cols) | Acc]);
        {$C, _} -> collect_rows_left_join(Conn, Cols, Acc);
        {$Z, _} -> {ok, lists:reverse(Acc)};
        {_, _}  -> collect_rows_left_join(Conn, Cols, Acc)
    end.

send_extended(Conn, SqlBin, Binds) ->
    %% Parse: unnamed statement, the query text, no pre-declared parameter types.
    send_msg(Conn, $P, <<0, SqlBin/binary, 0, 0:16>>),
    %% Bind: unnamed portal/statement, all-text parameters, all-text results.
    send_msg(Conn, $B, build_bind(Binds)),
    %% Describe the portal so the row description arrives before the rows.
    send_msg(Conn, $D, <<$P, 0>>),
    %% Execute with no row cap, then Sync to close the implicit transaction.
    send_msg(Conn, $E, <<0, 0:32>>),
    send_msg(Conn, $S, <<>>).

build_bind(Binds) ->
    ParamData = iolist_to_binary([encode_param(V) || V <- Binds]),
    NumParams = length(Binds),
    %% portal "" \0, statement "" \0, 0 param format codes, params, 0 result format codes.
    <<0, 0, 0:16, NumParams:16, ParamData/binary, 0:16>>.

encode_param(V) ->
    Bin = param_text(V),
    <<(byte_size(Bin)):32, Bin/binary>>.

param_text({'SqlInt', N})     -> integer_to_binary(N);
param_text({'SqlText', T})    -> T;
param_text({'SqlBool', true}) -> <<"t">>;
param_text({'SqlBool', false})-> <<"f">>;
param_text({'SqlFloat', F})   -> iolist_to_binary(io_lib:format("~p", [F]));
param_text('SqlNull')         -> <<>>.

collect_rows(Conn, Cols, Acc) ->
    case recv_msg(Conn) of
        {$1, _} -> collect_rows(Conn, Cols, Acc);
        {$2, _} -> collect_rows(Conn, Cols, Acc);
        {$T, P} -> collect_rows(Conn, decode_row_desc(P), Acc);
        {$n, _} -> collect_rows(Conn, Cols, Acc);
        {$D, P} -> collect_rows(Conn, Cols, [decode_data_row(P, Cols) | Acc]);
        {$C, _} -> collect_rows(Conn, Cols, Acc);
        {$Z, _} -> {ok, lists:reverse(Acc)};
        {_, _}  -> collect_rows(Conn, Cols, Acc)
    end.

collect_exec(Conn, Tag) ->
    case recv_msg(Conn) of
        {$C, P} ->
            {CmdTag, _} = read_cstring(P),
            collect_exec(Conn, CmdTag);
        {$Z, _} ->
            {ok, parse_affected(Tag)};
        {_, _} ->
            collect_exec(Conn, Tag)
    end.

parse_affected(<<"DELETE ", N/binary>>) -> safe_int(N);
parse_affected(<<"UPDATE ", N/binary>>) -> safe_int(N);
parse_affected(<<"INSERT ", Rest/binary>>) ->
    case binary:split(Rest, <<" ">>) of
        [_Oid, N] -> safe_int(N);
        _         -> 0
    end;
parse_affected(_) -> 0.

safe_int(B) ->
    try binary_to_integer(B) catch _:_ -> 0 end.

%% --- Row decoding ---

decode_row_desc(<<NFields:16, Rest/binary>>) ->
    decode_fields(NFields, Rest, []).

decode_fields(0, _Rest, Acc) ->
    lists:reverse(Acc);
decode_fields(N, Bin, Acc) ->
    {Name, R1} = read_cstring(Bin),
    <<_TableOid:32, _Attnum:16, TypeOid:32, _Len:16, _Typmod:32, _Fmt:16, R2/binary>> = R1,
    decode_fields(N - 1, R2, [{Name, TypeOid} | Acc]).

decode_data_row(<<NCols:16, Rest/binary>>, Cols) ->
    Vals = decode_cols(NCols, Rest, []),
    maps:from_list(
        lists:zipwith(fun({Name, Oid}, V) -> {Name, decode_cell(Oid, V)} end, Cols, Vals)).

decode_cols(0, _Bin, Acc) ->
    lists:reverse(Acc);
decode_cols(N, <<16#FFFFFFFF:32, Rest/binary>>, Acc) ->
    decode_cols(N - 1, Rest, [null | Acc]);
decode_cols(N, <<Len:32, Val:Len/binary, Rest/binary>>, Acc) ->
    decode_cols(N - 1, Rest, [Val | Acc]).

%% --- Join row decoding ---
%%
%% Like the single-table path, but the field descriptors keep each column's
%% attribute number so a `l.*, r.*` row can be split back into its two source
%% rows. Within one table `SELECT *` lists columns in ascending attribute order,
%% so the boundary between the left and right sides is the first column whose
%% attribute number does not exceed the previous one (this also handles a
%% self-join, where both sides share a table OID but the numbering still resets).

decode_row_desc_join(<<NFields:16, Rest/binary>>) ->
    decode_fields_join(NFields, Rest, []).

decode_fields_join(0, _Rest, Acc) ->
    lists:reverse(Acc);
decode_fields_join(N, Bin, Acc) ->
    {Name, R1} = read_cstring(Bin),
    <<_TableOid:32, Attnum:16, TypeOid:32, _Len:16, _Typmod:32, _Fmt:16, R2/binary>> = R1,
    decode_fields_join(N - 1, R2, [{Name, TypeOid, Attnum} | Acc]).

decode_data_row_join(<<NCols:16, Rest/binary>>, Cols) ->
    Vals = decode_cols(NCols, Rest, []),
    Cells = lists:zipwith(
        fun({Name, Oid, Attnum}, V) -> {Name, Attnum, decode_cell(Oid, V)} end, Cols, Vals),
    {Left, Right} = split_join_cells(Cells, [], -1),
    {maps:from_list(Left), maps:from_list(Right)}.

%% Walk the cells left to right, collecting `{Name, Value}` into the left map
%% until the attribute number stops increasing; that column and the rest form the
%% right map.
split_join_cells([], LeftAcc, _Prev) ->
    {lists:reverse(LeftAcc), []};
split_join_cells([{Name, Attnum, Val} | Rest], LeftAcc, Prev) when Attnum =< Prev ->
    Right = [{N, V} || {N, _A, V} <- [{Name, Attnum, Val} | Rest]],
    {lists:reverse(LeftAcc), Right};
split_join_cells([{Name, Attnum, Val} | Rest], LeftAcc, _Prev) ->
    split_join_cells(Rest, [{Name, Val} | LeftAcc], Attnum).

%% As decode_data_row_join, but the right side carries the `__ridge_matched`
%% sentinel. A TRUE marker means the row matched, so the right map (with the
%% marker dropped) is wrapped in `{some, _}`; a NULL or absent marker means the
%% left row was null-extended by the LEFT JOIN, so the right side is `none`.
decode_data_row_left_join(<<NCols:16, Rest/binary>>, Cols) ->
    Vals = decode_cols(NCols, Rest, []),
    Cells = lists:zipwith(
        fun({Name, Oid, Attnum}, V) -> {Name, Attnum, decode_cell(Oid, V)} end, Cols, Vals),
    {Left, Right} = split_join_cells(Cells, [], -1),
    LeftMap = maps:from_list(Left),
    RightMap = maps:from_list(Right),
    case maps:get(<<"__ridge_matched">>, RightMap, 'SqlNull') of
        {'SqlBool', true} ->
            {LeftMap, {some, maps:remove(<<"__ridge_matched">>, RightMap)}};
        _ ->
            {LeftMap, none}
    end.

decode_cell(_Oid, null) -> 'SqlNull';
decode_cell(Oid, Val)   -> decode_value(Oid, Val).

decode_value(16, <<"t">>) -> {'SqlBool', true};
decode_value(16, <<"f">>) -> {'SqlBool', false};
decode_value(Oid, Val) when Oid =:= 20; Oid =:= 21; Oid =:= 23 ->
    {'SqlInt', binary_to_integer(Val)};
decode_value(Oid, Val) when Oid =:= 700; Oid =:= 701; Oid =:= 1700 ->
    {'SqlFloat', to_float(Val)};
decode_value(Oid, Val) when Oid =:= 25; Oid =:= 1043; Oid =:= 1042; Oid =:= 19; Oid =:= 18 ->
    {'SqlText', Val};
decode_value(_Oid, Val) ->
    {'SqlText', Val}.

to_float(Val) ->
    try binary_to_float(Val)
    catch _:_ ->
        try float(binary_to_integer(Val)) catch _:_ -> 0.0 end
    end.

%% --- Connect, TLS upgrade, authentication ---

do_connect(Config) ->
    Host = binary_to_list(maps:get(host, Config)),
    Port = maps:get(port, Config),
    SslMode = maps:get(ssl_mode, Config),
    case gen_tcp:connect(Host, Port,
                         [binary, {active, false}, {packet, raw}], ?CONNECT_TIMEOUT) of
        {ok, Sock} ->
            try
                Conn = maybe_ssl({gen_tcp, Sock}, Host, SslMode),
                startup(Conn, Config),
                authenticate(Conn, Config),
                wait_ready(Conn),
                {ok, Conn}
            catch
                throw:{pg_error, E} ->
                    gen_tcp:close(Sock),
                    {error, E};
                throw:{pg_fatal, E} ->
                    gen_tcp:close(Sock),
                    {error, E}
            end;
        {error, Reason} ->
            {error, #{code => <<"db.connect.refused">>, message => to_bin(Reason)}}
    end.

maybe_ssl(Conn, _Host, <<"disable">>) ->
    Conn;
maybe_ssl({gen_tcp, Sock} = Conn, Host, SslMode) ->
    %% SSLRequest is a length-prefixed body with no type byte; the magic code
    %% 80877103 asks the server whether it will speak TLS.
    xsend(Conn, <<8:32, 80877103:32>>),
    case xrecv(Conn, 1) of
        <<$S>> ->
            application:ensure_all_started(ssl),
            case ssl:connect(Sock, ssl_opts(SslMode, Host), ?CONNECT_TIMEOUT) of
                {ok, SslSock} ->
                    {ssl, SslSock};
                {error, Reason} ->
                    throw({pg_error, #{code => <<"db.ssl.failed">>,
                                       message => to_bin(Reason)}})
            end;
        <<$N>> ->
            throw({pg_error, #{code => <<"db.ssl.unsupported">>,
                               message => <<"server does not support TLS but sslMode requires it">>}})
    end.

ssl_opts(<<"require">>, _Host) ->
    [{verify, verify_none}];
ssl_opts(<<"verify-full">>, Host) ->
    [{verify, verify_peer},
     {cacerts, public_key:cacerts_get()},
     {server_name_indication, Host},
     {customize_hostname_check,
      [{match_fun, public_key:pkix_verify_hostname_match_fun(https)}]}].

startup(Conn, Config) ->
    User = maps:get(user, Config),
    Db = maps:get(database, Config),
    Payload = <<196608:32, "user", 0, User/binary, 0, "database", 0, Db/binary, 0, 0>>,
    send_startup(Conn, Payload).

authenticate(Conn, Config) ->
    case recv_msg(Conn) of
        {$R, <<0:32>>} ->
            ok;
        {$R, <<3:32>>} ->
            send_msg(Conn, $p, <<(maps:get(password, Config))/binary, 0>>),
            authenticate(Conn, Config);
        {$R, <<5:32, Salt:4/binary>>} ->
            send_msg(Conn, $p, md5_password(Config, Salt)),
            authenticate(Conn, Config);
        {$R, <<10:32, _Mechanisms/binary>>} ->
            scram_auth(Conn, Config),
            authenticate(Conn, Config);
        {$R, <<Other:32, _/binary>>} ->
            throw({pg_error, #{code => <<"db.auth.unsupported">>,
                               message => to_bin({unsupported_auth, Other})}})
    end.

wait_ready(Conn) ->
    case recv_msg(Conn) of
        {$Z, _} -> ok;
        {_, _}  -> wait_ready(Conn)
    end.

%% MD5: concat(user, password) hashed, salted, hashed again, "md5"-prefixed.
md5_password(Config, Salt) ->
    User = maps:get(user, Config),
    Pass = maps:get(password, Config),
    Inner = md5_hex(<<Pass/binary, User/binary>>),
    Outer = md5_hex(<<Inner/binary, Salt/binary>>),
    <<"md5", Outer/binary, 0>>.

%% --- SCRAM-SHA-256 (RFC 5802 / 7677) ---

scram_auth(Conn, Config) ->
    Pass = maps:get(password, Config),
    ClientNonce = base64:encode(crypto:strong_rand_bytes(18)),
    ClientFirstBare = <<"n=,r=", ClientNonce/binary>>,
    ClientFirst = <<"n,,", ClientFirstBare/binary>>,
    InitResponse = <<"SCRAM-SHA-256", 0, (byte_size(ClientFirst)):32, ClientFirst/binary>>,
    send_msg(Conn, $p, InitResponse),

    ServerFirst =
        case recv_msg(Conn) of
            {$R, <<11:32, SF/binary>>} -> SF;
            {$R, <<Code:32, _/binary>>} ->
                throw({pg_error, #{code => <<"db.auth.failed">>,
                                   message => to_bin({scram_continue, Code})}})
        end,
    Attrs = parse_scram(ServerFirst),
    ServerNonce = maps:get($r, Attrs),
    Salt = base64:decode(maps:get($s, Attrs)),
    Iters = binary_to_integer(maps:get($i, Attrs)),
    case binary:longest_common_prefix([ClientNonce, ServerNonce]) >= byte_size(ClientNonce) of
        true  -> ok;
        false -> throw({pg_error, #{code => <<"db.auth.failed">>,
                                    message => <<"SCRAM nonce mismatch">>}})
    end,

    SaltedPassword = pbkdf2(Pass, Salt, Iters),
    ClientKey = crypto:mac(hmac, sha256, SaltedPassword, <<"Client Key">>),
    StoredKey = crypto:hash(sha256, ClientKey),
    ClientFinalNoProof = <<"c=biws,r=", ServerNonce/binary>>,
    AuthMessage = <<ClientFirstBare/binary, ",", ServerFirst/binary, ",", ClientFinalNoProof/binary>>,
    ClientSignature = crypto:mac(hmac, sha256, StoredKey, AuthMessage),
    ClientProof = crypto:exor(ClientKey, ClientSignature),
    ClientFinal = <<ClientFinalNoProof/binary, ",p=", (base64:encode(ClientProof))/binary>>,
    send_msg(Conn, $p, ClientFinal),

    ServerFinal =
        case recv_msg(Conn) of
            {$R, <<12:32, SFin/binary>>} -> SFin;
            {$R, <<Code2:32, _/binary>>} ->
                throw({pg_error, #{code => <<"db.auth.failed">>,
                                   message => to_bin({scram_final, Code2})}})
        end,
    ServerKey = crypto:mac(hmac, sha256, SaltedPassword, <<"Server Key">>),
    ServerSig = base64:encode(crypto:mac(hmac, sha256, ServerKey, AuthMessage)),
    case maps:get($v, parse_scram(ServerFinal), undefined) of
        ServerSig -> ok;
        _ -> throw({pg_error, #{code => <<"db.auth.failed">>,
                                message => <<"SCRAM server signature mismatch">>}})
    end.

%% Split a SCRAM "a=...,b=..." message into a map keyed by the attribute char.
%% Only the first '=' separates key from value, so base64 padding in the value
%% survives intact.
parse_scram(Bin) ->
    lists:foldl(
        fun(Part, Acc) ->
            case Part of
                <<K:8, $=, V/binary>> -> Acc#{K => V};
                _                     -> Acc
            end
        end, #{}, binary:split(Bin, <<",">>, [global])).

%% SCRAM-SHA-256 needs a single PBKDF2 block: the derived key length (32) equals
%% the HMAC-SHA-256 output length, so block index 1 is the whole answer.
pbkdf2(Password, Salt, Iters) ->
    U1 = crypto:mac(hmac, sha256, Password, <<Salt/binary, 1:32>>),
    pbkdf2_iter(Password, U1, U1, Iters - 1).

pbkdf2_iter(_Password, _U, Acc, 0) ->
    Acc;
pbkdf2_iter(Password, U, Acc, N) ->
    Next = crypto:mac(hmac, sha256, Password, U),
    pbkdf2_iter(Password, Next, crypto:exor(Acc, Next), N - 1).

md5_hex(Bin) ->
    bin_to_hex(crypto:hash(md5, Bin)).

bin_to_hex(Bin) ->
    << <<(hex_digit(N bsr 4)), (hex_digit(N band 16#0F))>> || <<N>> <= Bin >>.

hex_digit(D) when D < 10 -> $0 + D;
hex_digit(D)             -> $a + (D - 10).

%% --- Error responses ---
%%
%% An ErrorResponse is a sequence of `<<FieldCode, Value..., 0>>` pairs ending in
%% a lone 0. The 'C' field is the SQLSTATE and 'M' is the human message.
decode_error(Payload) ->
    Fields = decode_error_fields(Payload, #{}),
    Message = maps:get($M, Fields, <<"postgres error">>),
    Code =
        case maps:get($C, Fields, undefined) of
            undefined -> <<"db.error">>;
            SqlState  -> <<"db.error.", SqlState/binary>>
        end,
    #{code => Code, message => Message}.

decode_error_fields(<<0>>, Acc) -> Acc;
decode_error_fields(<<>>, Acc)  -> Acc;
decode_error_fields(<<FieldCode:8, Rest/binary>>, Acc) ->
    {Value, Rest2} = read_cstring(Rest),
    decode_error_fields(Rest2, Acc#{FieldCode => Value}).

%% --- Message framing ---

send_msg(Conn, Tag, Payload) ->
    Len = byte_size(Payload) + 4,
    xsend(Conn, <<Tag:8, Len:32, Payload/binary>>).

send_startup(Conn, Payload) ->
    Len = byte_size(Payload) + 4,
    xsend(Conn, <<Len:32, Payload/binary>>).

%% Read one tagged message, surfacing server errors as a throw and skipping
%% notices, so the happy path never sees either.
recv_msg(Conn) ->
    case read_raw_msg(Conn) of
        {$E, Payload} -> throw({pg_error, decode_error(Payload)});
        {$N, _}       -> recv_msg(Conn);
        Msg           -> Msg
    end.

read_raw_msg(Conn) ->
    <<Tag:8, Len:32>> = xrecv(Conn, 5),
    PayloadLen = Len - 4,
    Payload =
        case PayloadLen of
            0 -> <<>>;
            _ -> xrecv(Conn, PayloadLen)
        end,
    {Tag, Payload}.

%% After a server error the backend still sends ReadyForQuery; drain to it so the
%% connection is reusable. Tolerate a dead socket here — the error is already in
%% hand.
drain_until_ready(Conn) ->
    try
        case read_raw_msg(Conn) of
            {$Z, _} -> ok;
            {_, _}  -> drain_until_ready(Conn)
        end
    catch
        throw:{pg_error, _} -> ok;
        _:_ -> ok
    end.

read_cstring(Bin) ->
    case binary:split(Bin, <<0>>) of
        [Str, Rest] -> {Str, Rest};
        [Str]       -> {Str, <<>>}
    end.

%% --- Transport ---
%%
%% A send or receive failure is a transport fault, thrown as `pg_fatal` so it is
%% not caught alongside SQL errors: the connection process ends and the pool
%% replaces it, rather than the broken socket being returned to service.

xsend({Mod, Sock}, Data) ->
    case Mod:send(Sock, Data) of
        ok -> ok;
        {error, Reason} ->
            throw({pg_fatal, #{code => <<"db.conn.send">>, message => to_bin(Reason)}})
    end.

xrecv({Mod, Sock}, N) ->
    case Mod:recv(Sock, N, ?RECV_TIMEOUT) of
        {ok, Data} -> Data;
        {error, Reason} ->
            throw({pg_fatal, #{code => <<"db.conn.recv">>, message => to_bin(Reason)}})
    end.

transport_close({Mod, Sock}) -> Mod:close(Sock).

set_controlling({gen_tcp, Sock}, Pid) -> gen_tcp:controlling_process(Sock, Pid);
set_controlling({ssl, Sock}, Pid)     -> ssl:controlling_process(Sock, Pid).

to_bin(Term) when is_binary(Term) -> Term;
to_bin(Term) -> iolist_to_binary(io_lib:format("~p", [Term])).
