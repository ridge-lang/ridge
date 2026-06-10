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
%% This module owns a single connection per handle. The pooled, supervised
%% substrate lands in a later step; the registry below already maps an integer
%% handle id to a connection process, which is where a pool will slot in.

-module(ridge_pg).

-export([
    pg_connect/7,
    pg_insert/3,
    pg_all/2,
    pg_select/3,
    pg_get_rows/4,
    pg_delete/3,
    pg_close/1
]).

-define(CONNECT_TIMEOUT, 10000).
-define(RECV_TIMEOUT, 15000).
-define(QUERY_TIMEOUT, 30000).

%% A live connection is `{Transport, Socket}` where Transport is `gen_tcp` or
%% `ssl`; both expose the same send/recv/close surface, so transport-specific
%% code is confined to xsend/2, xrecv/2 and transport_close/1.

%% --- FFI surface (mirrors the MemAdapter verbs in ridge_rt.erl) ---

%% pg_connect/7 — std.data.connect. Open a connection from the config fields and
%% return the Ridge handle `#{id => Id}` (the same id-as-handle shape MemAdapter
%% uses). The config crosses the FFI boundary as positional scalars, not a record
%% map, so it never depends on how a Ridge record lowers its keys. Result
%% Postgres Error.
pg_connect(Host, Port, Database, User, Password, SslMode, _PoolSize) ->
    application:ensure_all_started(crypto),
    Config = #{host => Host, port => Port, database => Database,
               user => User, password => Password, ssl_mode => SslMode},
    case do_connect(Config) of
        {ok, Conn} ->
            Pid = spawn(fun() -> pg_conn_loop(Conn) end),
            set_controlling(Conn, Pid),
            Id = pg_registry_call({register, Pid}),
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

%% pg_close/1 — close a connection and forget its handle. Result Unit Error.
pg_close(Id) -> pg_call(Id, close).

%% --- Handle registry ---
%%
%% A single registered process maps an integer handle id to its connection
%% process. Lookups are cheap and queries go straight to the connection, so two
%% handles never serialise through one another.

pg_call(Id, Req) ->
    case pg_registry_call({lookup, Id}) of
        {ok, Pid} ->
            Ref = make_ref(),
            Pid ! {Req, self(), Ref},
            receive
                {Ref, Reply} -> Reply
            after ?QUERY_TIMEOUT ->
                {error, #{code => <<"db.timeout">>,
                          message => <<"postgres request timed out">>}}
            end;
        _ ->
            {error, #{code => <<"db.conn.closed">>,
                      message => <<"connection handle not found">>}}
    end.

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

%% --- Connection process ---
%%
%% Owns one socket and runs every request to completion before taking the next,
%% so one connection is never used concurrently. Each verb builds its SQL here,
%% with all values carried as bind parameters.

pg_conn_loop(Conn) ->
    receive
        {{insert, Table, Row}, From, Ref} ->
            From ! {Ref, do_insert(Conn, Table, Row)},
            pg_conn_loop(Conn);
        {{all, Table}, From, Ref} ->
            Sql = ["SELECT * FROM ", quote_ident(Table)],
            From ! {Ref, run_query(Conn, Sql, [])},
            pg_conn_loop(Conn);
        {{select, Table, Tree}, From, Ref} ->
            {Where, Binds} = compile_where(Tree),
            Sql = ["SELECT * FROM ", quote_ident(Table), " WHERE ", Where],
            From ! {Ref, run_query(Conn, Sql, Binds)},
            pg_conn_loop(Conn);
        {{get_rows, Table, Column, Key}, From, Ref} ->
            Sql = ["SELECT * FROM ", quote_ident(Table),
                   " WHERE ", quote_ident(Column), " = $1"],
            From ! {Ref, run_query(Conn, Sql, [Key])},
            pg_conn_loop(Conn);
        {{delete, Table, Tree}, From, Ref} ->
            {Where, Binds} = compile_where(Tree),
            Sql = ["DELETE FROM ", quote_ident(Table), " WHERE ", Where],
            From ! {Ref, do_exec(Conn, Sql, Binds)},
            pg_conn_loop(Conn);
        {close, From, Ref} ->
            transport_close(Conn),
            From ! {Ref, {ok, ok}}
    end.

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

xsend({Mod, Sock}, Data) ->
    case Mod:send(Sock, Data) of
        ok -> ok;
        {error, Reason} ->
            throw({pg_error, #{code => <<"db.conn.send">>, message => to_bin(Reason)}})
    end.

xrecv({Mod, Sock}, N) ->
    case Mod:recv(Sock, N, ?RECV_TIMEOUT) of
        {ok, Data} -> Data;
        {error, Reason} ->
            throw({pg_error, #{code => <<"db.conn.recv">>, message => to_bin(Reason)}})
    end.

transport_close({Mod, Sock}) -> Mod:close(Sock).

set_controlling({gen_tcp, Sock}, Pid) -> gen_tcp:controlling_process(Sock, Pid);
set_controlling({ssl, Sock}, Pid)     -> ssl:controlling_process(Sock, Pid).

to_bin(Term) when is_binary(Term) -> Term;
to_bin(Term) -> iolist_to_binary(io_lib:format("~p", [Term])).
