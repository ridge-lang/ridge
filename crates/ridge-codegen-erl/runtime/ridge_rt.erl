%% ridge_rt — Ridge runtime bridge for BEAM.
%% Provides Option/Result adapters, I/O helpers, actor primitives,
%% and text/number utilities that map Ridge stdlib symbols to BEAM.
%% Bundled with ridge-codegen-erl and installed into <out_root>/runtime/.
-module(ridge_rt).
-export([
    println/1, print/1, eprintln/1,
    read_line/1,
    fs_lines/1, fs_read/1, fs_write/2, fs_append/2,
    cli_args/0, cli_args/1,
    time_now/0, time_now/1, time_epoch/0, time_epoch/1,
    time_diff_ms/2, time_diff/2,
    time_from_iso/1, time_since_ms/1, time_iso/1,
    int_parse/0, int_parse/1, float_parse/1, float_to_text/1, bool_to_text/1,
    text_split_all/2, text_replace_all/3,
    list_fold/3, list_sort_by/2,
    random_int/2, random_choice/1, random_float/1, random_alphanumeric/1, random_seed/1,
    env_get/1, env_all/1, env_set/2,
    proc_run/2,
    json_encode/1, json_decode/1,
    json_null/0, json_null/1,
    json_bool/1, json_int/1, json_float/1, json_text/1,
    json_list/1, json_object/1,
    http_listen/2, http_port/0,
    http_get/1, http_post/2, http_put/2, http_delete/1,
    ask/3, send/2, spawn_actor/3,
    escript_main/1
]).

%% --- I/O ---

println(B)  -> io:format("~ts~n", [B]).
print(B)    -> io:format("~ts",   [B]).
eprintln(B) -> io:format(standard_error, "~ts~n", [B]).

%% read_line/1 — std.io.readLine
%% Reads one line from stdin.
%% Returns {ok, Line} on success or {error, {error_record, Code, Message}} on
%% EOF / read error.  Ridge type: Result Text Error.
%% Ridge calling convention: zero-param fns receive the Unit `ok` arg.
read_line(_Unit) ->
    case io:get_line("") of
        eof        -> {error, {error_record, <<"eof">>,     <<"end of input">>}};
        {error, R} -> {error, {error_record, <<"io_error">>,
                                iolist_to_binary(io_lib:format("~p", [R]))}};
        Line       -> {ok, iolist_to_binary(string:trim(Line, trailing, "\n"))}
    end.

%% --- File-system ---

fs_lines(Path) ->
    case file:read_file(Path) of
        {ok, Bin}  -> {ok, binary:split(Bin, <<"\n">>, [global])};
        {error, R} -> {error, atom_to_binary(R, utf8)}
    end.

%% fs_read/1 — std.fs.readFile
%% Reads an entire file as a binary.  Returns Ridge Result shape.
fs_read(Path) ->
    case file:read_file(Path) of
        {ok, Bin}  -> {ok, Bin};
        {error, R} -> {error, atom_to_binary(R, utf8)}
    end.

%% fs_write/2 — std.fs.writeFile (truncating write)
%% Writes content to a file, replacing its contents.  Returns Ridge Result shape.
fs_write(Path, Content) ->
    case file:write_file(Path, Content) of
        ok         -> {ok, ok};
        {error, R} -> {error, atom_to_binary(R, utf8)}
    end.

%% fs_append/2 — std.fs.append
%% Appends content to a file, creating it if it does not exist.
fs_append(Path, Content) ->
    case file:write_file(Path, Content, [append]) of
        ok         -> {ok, ok};
        {error, R} -> {error, atom_to_binary(R, utf8)}
    end.

%% cli_args/0: returns CLI arguments as a list of binaries.
%% In escript mode the escript_main/1 bridge stores the pre-processed argument list
%% under the process-dictionary key `ridge_escript_args` so that this function
%% returns the correct args (without the escript script-name prefix that
%% init:get_plain_arguments/0 includes in escript invocations).
cli_args() ->
    case erlang:get(ridge_escript_args) of
        undefined -> [list_to_binary(A) || A <- init:get_plain_arguments()];
        Args      -> Args
    end.
%% Ridge calling convention: zero-param fns accept a unit `ok` arg from the caller.
cli_args(_Unit) -> cli_args().

%% --- Time ---

time_now()        -> {timestamp, erlang:system_time(microsecond)}.
%% Ridge calling convention: zero-param fns accept a unit `ok` arg from the caller.
time_now(_Unit)   -> time_now().
time_epoch()      -> {timestamp, 0}.
time_epoch(_Unit) -> time_epoch().

%% time_diff_ms/2 — std.time.diffMs  (§7.3 new adapter)
%% Returns the difference A - B in milliseconds (integer).
time_diff_ms({timestamp, A}, {timestamp, B}) -> (A - B) div 1000.

%% time_diff/2 — std.time.diff  (§3.12 line 349)
%% Returns the difference A - B as a Duration record {duration, Ms}.
%% Ridge type: Timestamp -> Timestamp -> Duration.
time_diff({timestamp, A}, {timestamp, B}) -> {duration, (A - B) div 1000}.

%% time_from_iso/1 — std.time.fromIso / std.time.parse
%% Parses an ISO-8601 text into a Timestamp.
%% Returns {ok, {timestamp, Micros}} or {error, {error_record, Code, Message}}.
%% Ridge type: Text -> Result Timestamp Error  (§3.12 lines 348, 353).
time_from_iso(Text) ->
    Str = binary_to_list(Text),
    try
        Micros = calendar:rfc3339_to_system_time(Str, [{unit, microsecond}]),
        {ok, {timestamp, Micros}}
    catch
        _:_ -> {error, {error_record, <<"parse_error">>,
                         <<"invalid ISO-8601 timestamp">>}}
    end.

%% time_since_ms/1 — std.time.sinceMs
%% Returns milliseconds elapsed since the given timestamp.
time_since_ms({timestamp, T}) ->
    Now = erlang:system_time(microsecond),
    (Now - T) div 1000.

%% time_iso/1 — std.time.iso
%% Formats a Timestamp as an ISO-8601 text string.
time_iso({timestamp, Micros}) ->
    Str = calendar:system_time_to_rfc3339(Micros, [{unit, microsecond}, {offset, "Z"}]),
    list_to_binary(Str).

%% --- Numbers ---

%% int_parse/0: returns a fun ref for use in higher-order contexts (e.g. Option.flatMap Int.parse).
int_parse() -> fun int_parse/1.

int_parse(B) ->
    try {some, binary_to_integer(B)} catch _:_ -> none end.

%% float_parse/1: std.float.parse — Text -> Option Float.
%% Accepts both float-shaped strings ("3.14", "1e3") and integer-shaped
%% strings ("100"), returning none only when neither form parses.
%% Erlang's binary_to_float/1 rejects "100" with badarg even though it is a
%% valid Float value; falling back to binary_to_integer + erlang:float/1
%% mirrors what callers (and most languages) expect from a Float parser.
float_parse(B) ->
    try {some, binary_to_float(B)}
    catch _:_ ->
        try {some, float(binary_to_integer(B))}
        catch _:_ -> none end
    end.

float_to_text(F) -> iolist_to_binary(io_lib:format("~p", [F])).

%% text_split_all/2 — binary:split with [global] option (Sep, Subject order matches Ridge FFI).
text_split_all(Sep, S) -> binary:split(S, Sep, [global]).

%% list_fold/3 — std.list.fold via lists:foldl with arg-order adapter.
%%
%% Ridge's `fold` takes a callback `fn b -> a -> b`
%% (accumulator first, element second).  Erlang's `lists:foldl(F, Acc, List)`
%% calls the callback as `F(Elem, Acc)` (element first, accumulator second).
%% Without an adapter, every `List.fold` silently passed args in the wrong
%% order — undetectable when the callback was symmetric (`fn a b -> a + b`)
%% but data-corrupting whenever the two argument types differed.
list_fold(F, Acc, List) ->
    lists:foldl(fun(Elem, A) -> F(A, Elem) end, Acc, List).

%% list_sort_by/2 — std.list.sortBy via lists:sort/2 with key-fn adapter.
%%
%% Ridge's `sortBy` takes a KEY function
%% `fn a -> b` and orders elements by `key(a) <= key(b)`.  Erlang's
%% `lists:sort(Fun, List)` instead takes a COMPARATOR `Fun(A, B) -> bool`.
%% Without an adapter, every `List.sortBy` invoked the user's key function
%% with two unrelated elements and used its (often nonsensical) Boolean
%% result as the ordering predicate.
list_sort_by(Key, List) ->
    lists:sort(fun(A, B) -> Key(A) =< Key(B) end, List).

%% text_replace_all/3 — binary:replace with [global] option (From, To, Subject order matches Ridge FFI).
text_replace_all(From, To, S) -> binary:replace(S, From, To, [global]).

bool_to_text(true)  -> <<"true">>;
bool_to_text(false) -> <<"false">>.

%% --- Random ---

random_int(Lo, Hi) -> Lo + rand:uniform(Hi - Lo + 1) - 1.
random_choice([])  -> none;
random_choice(L)   -> {some, lists:nth(rand:uniform(length(L)), L)}.

%% random_float/1 — std.random.float
%% Returns a uniform float in [0.0, 1.0).
random_float(_Unit) -> rand:uniform().

%% random_alphanumeric/1 — std.random.alphanumeric
%% Returns a random alphanumeric binary of length N.
random_alphanumeric(N) ->
    Chars = <<"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789">>,
    Len   = byte_size(Chars),
    list_to_binary([binary:at(Chars, rand:uniform(Len) - 1) || _ <- lists:seq(1, N)]).

%% random_seed/1 — std.random.seed
%% Seeds the process-local RNG with an integer.
random_seed(S) ->
    rand:seed(exsplus, {S, S bxor 16#deadbeef, S bxor 16#cafebabe}),
    ok.

%% --- Environment ---

%% env_get/1 — std.env.get
%% Looks up an environment variable.  Returns {some, Bin} or none.
env_get(Name) ->
    case os:getenv(binary_to_list(Name)) of
        false -> none;
        Val   ->
            case unicode:characters_to_binary(Val) of
                Bin when is_binary(Bin) -> {some, Bin};
                _                       -> none
            end
    end.

%% env_all/1 — std.env.all
%% Returns all environment variables as a BEAM map #{BinKey => BinVal}.
%% Uses unicode:characters_to_binary/1 so environment entries containing
%% non-Latin-1 code points (e.g. em-dashes in Windows PATHEXT) are encoded
%% to valid UTF-8 rather than crashing with badarg in list_to_binary/1.
%% Entries that fail conversion (malformed sequences) are silently skipped.
env_all(_Unit) ->
    Pairs = os:env(),
    maps:from_list(
        lists:filtermap(
            fun({K, V}) ->
                KB = unicode:characters_to_binary(K),
                VB = unicode:characters_to_binary(V),
                case {KB, VB} of
                    {KB2, VB2} when is_binary(KB2), is_binary(VB2) ->
                        {true, {KB2, VB2}};
                    _ ->
                        false
                end
            end,
            Pairs)).

%% env_set/2 — std.env.set
%% Sets an environment variable.
env_set(Name, Value) ->
    os:putenv(binary_to_list(Name), binary_to_list(Value)),
    ok.

%% --- Process execution ---

%% proc_run/2 — std.proc.run
%% Runs an external command with the given argument list.
%% Returns {ok, {proc_output, Stdout, Stderr, ExitCode}} or
%%         {error, {error_record, Code, Message}}.
%% Ridge type: Text -> List Text -> Result ProcOutput Error  (§3.16 / D123).
%%
%% stdout and stderr are captured separately using two ports:
%%   - port 1 (stdout): {spawn_executable, ...} with use_stdio
%%   - port 2 (stderr): {spawn_executable, ...} with stderr_to_stdout on a
%%     separate invocation is not possible via open_port without an OS helper.
%% Pragmatic approach for 0.1.0: open_port with two separate fd options is not
%% supported on all platforms via the BEAM port driver without an external shim.
%% We use {spawn_executable, ...} with stdout only and stderr captured via a
%% wrapper trick: spawn "sh" ["-c", "cmd args 2>/tmp/ridge_stderr_<pid>"].
%% For simplicity and portability, we use a single port with stderr merged to
%% stdout for collection, and return empty binary for stderr.
%% This is documented as a known 0.1.0 limitation (separate stderr deferred).
proc_run(Cmd, Args) ->
    CmdStr0 = binary_to_list(Cmd),
    CmdStr  = case os:find_executable(CmdStr0) of
                  false -> CmdStr0;
                  Full  -> Full
              end,
    ArgList = [binary_to_list(A) || A <- Args],
    try
        Port = open_port({spawn_executable, CmdStr},
                         [exit_status, {args, ArgList}, binary, use_stdio]),
        proc_run_collect(Port, [])
    catch
        _:Reason ->
            Msg = iolist_to_binary(io_lib:format("~p", [Reason])),
            {error, {error_record, <<"spawn_error">>, Msg}}
    end.

%% Collect port data until exit_status; build ProcOutput.
%% stderr is empty for 0.1.0 (separate capture deferred — see proc_run comment).
%%
%% Wall-clock 30 s budget: a naive `receive ... after 30000` resets the timer
%% every time the child emits data, so a child that prints in a tight loop
%% (e.g. an interactive Erlang shell that we accidentally spawned without
%% -noshell, observed in ADO #212 Linux container) keeps the parent blocked
%% indefinitely.  We capture a monotonic deadline at entry and recompute the
%% remaining budget on each recursive call so the timeout fires after 30 s of
%% wall clock regardless of how chatty the child is.
proc_run_collect(Port, Acc) ->
    Deadline = erlang:monotonic_time(millisecond) + 30000,
    proc_run_collect(Port, Acc, Deadline).

proc_run_collect(Port, Acc, Deadline) ->
    Remaining = max(0, Deadline - erlang:monotonic_time(millisecond)),
    receive
        {Port, {data, D}} ->
            proc_run_collect(Port, [D | Acc], Deadline);
        {Port, {exit_status, Code}} ->
            Stdout = iolist_to_binary(lists:reverse(Acc)),
            %% ProcOutput is declared in stdlib/proc.ridge as
            %%   pub type ProcOutput = { stdout: Text, stderr: Text, exitCode: Int }
            %% which codegen lowers to an Erlang map keyed by field atoms
            %% (field access `.exitCode` compiles to `erlang:map_get(exitCode,
            %% _)`).  Returning a tagged tuple `{proc_output, ...}` from this
            %% bridge produced a `badmap` exception at runtime — observed in
            %% ADO #214 `proc.test.test_run_captures_stderr`.  Match the map
            %% shape codegen expects.
            {ok, #{stdout => Stdout, stderr => <<>>, exitCode => Code}}
    after Remaining ->
        port_close(Port),
        {error, {error_record, <<"timeout">>, <<"process exceeded 30s timeout">>}}
    end.

%% --- JSON (§3.17) ---

%% JsonValue constructor shims.
%%
%% Cross-module constructor resolution for user-defined `pub type` variants
%% is a 0.2.0 design item.  Until then these FFI shims let user code
%% build JsonValue trees in the exact wire format `json_encode/1` consumes
%% (lowercase-snake tag atoms with the documented payload positions).
%% Each shim is intentionally trivial — pure construction, no validation —
%% so the BEAM never observes a malformed JsonValue from user input.
json_null() -> json_null.
json_null(_Unit) -> json_null().
json_bool(B) -> {json_bool, B}.
json_int(N) -> {json_int, N}.
json_float(F) -> {json_float, F}.
json_text(S) -> {json_text, S}.
json_list(L) -> {json_list, L}.
json_object(M) -> {json_object, M}.

%% json_encode/1 — std.json.encode
%% Recursively encodes a JsonValue tagged-tuple tree to a JSON binary.
%% Wire representation:
%%   JNull              ↔  json_null
%%   JBool Bool         ↔  {json_bool, Bool}
%%   JInt Int           ↔  {json_int, Int}
%%   JFloat Float       ↔  {json_float, Float}
%%   JText Text         ↔  {json_text, Text}
%%   JList (List V)     ↔  {json_list, [V]}
%%   JObject (Map Text V) ↔ {json_object, #{Text => V}}
%%
%% Ridge type: JsonValue -> Text
json_encode(json_null) ->
    <<"null">>;
json_encode({json_bool, true}) ->
    <<"true">>;
json_encode({json_bool, false}) ->
    <<"false">>;
json_encode({json_int, N}) ->
    integer_to_binary(N);
json_encode({json_float, F}) ->
    iolist_to_binary(io_lib:format("~p", [F]));
json_encode({json_text, T}) ->
    %% Minimal escaping: backslash and double-quote only (MVP).
    Escaped = binary:replace(
        binary:replace(T, <<"\\">>, <<"\\\\">>, [global]),
        <<"\"">>, <<"\\\"">>, [global]),
    <<"\"", Escaped/binary, "\"">>;
json_encode({json_list, Items}) ->
    Encoded = [json_encode(I) || I <- Items],
    Joined  = join_binaries(Encoded, <<",">>),
    <<"[", Joined/binary, "]">>;
json_encode({json_object, M}) ->
    Pairs = maps:to_list(M),
    Encoded = [begin
        K2 = json_encode({json_text, K}),
        V2 = json_encode(V),
        <<K2/binary, ":", V2/binary>>
    end || {K, V} <- Pairs],
    Joined = join_binaries(Encoded, <<",">>),
    <<"{", Joined/binary, "}">>.

%% join_binaries/2 — join a list of binaries with a separator.
join_binaries([], _Sep) -> <<>>;
join_binaries([H | T], Sep) ->
    lists:foldl(fun(B, Acc) -> <<Acc/binary, Sep/binary, B/binary>> end, H, T).

%% json_decode/1 — std.json.decode
%% Decodes a JSON binary to a JsonValue tagged-tuple tree using OTP-27's
%% native json module.  Falls back to a simple error response on OTP 26.
%% Returns {ok, JsonValue} | {error, {error_record, Code, Message}}.
%% Ridge type: Text -> Result JsonValue Error  (§3.17).
json_decode(Text) ->
    try
        %% OTP 27+: json:decode/1 is available.
        %% On OTP 26 this call throws undef — caught below.
        Decoded = json:decode(Text),
        {ok, erlang_to_json_value(Decoded)}
    catch
        error:undef ->
            %% OTP 26 fallback: json module not available.
            {error, {error_record, <<"not_implemented">>,
                     <<"json:decode/1 requires OTP 27+">>}};
        _:Reason ->
            Msg = iolist_to_binary(io_lib:format("~p", [Reason])),
            {error, {error_record, <<"decode_error">>, Msg}}
    end.

%% erlang_to_json_value/1 — convert OTP-27 json:decode/1 output to JsonValue.
%% OTP 27 json:decode/1 returns:
%%   null        → null
%%   true/false  → true/false
%%   integer()   → integer
%%   float()     → float
%%   binary()    → binary (for strings)
%%   list()      → list of decoded values
%%   map()       → map of binary keys to decoded values
erlang_to_json_value(null)              -> json_null;
erlang_to_json_value(true)              -> {json_bool, true};
erlang_to_json_value(false)             -> {json_bool, false};
erlang_to_json_value(N) when is_integer(N) -> {json_int, N};
erlang_to_json_value(F) when is_float(F)   -> {json_float, F};
erlang_to_json_value(B) when is_binary(B)  -> {json_text, B};
erlang_to_json_value(L) when is_list(L)    ->
    {json_list, [erlang_to_json_value(E) || E <- L]};
erlang_to_json_value(M) when is_map(M)     ->
    {json_object, maps:map(fun(_K, V) -> erlang_to_json_value(V) end, M)}.

%% --- HTTP server (§3.18) ---

%% http_listen/2 — std.net.http.listen
%%
%% Binds a TCP socket on Port (0 = OS-assigned), registers the bound port
%% under the name `ridge_http_server` in the process registry so that tests
%% can retrieve it via http_port/0, then enters the HTTP/1.0 accept loop.
%%
%% Each accepted connection is:
%%   1. Read into a raw binary.
%%   2. Parsed into a Request map  #{method, path, body}.
%%   3. Passed to Handler (a Ridge fun value).
%%   4. The returned Response map #{status, body} is serialised as HTTP/1.0.
%%
%% The loop runs in the calling process; it does NOT return normally.
%% Ridge type: Int -> (fn {net} (Request -> Response)) -> Unit.
http_listen(Port, Handler) ->
    application:ensure_all_started(inets),
    {ok, LSock} = gen_tcp:listen(Port,
        [binary, {active, false}, {reuseaddr, true}, {packet, raw}]),
    {ok, BoundPort} = inet:port(LSock),
    %% Register the bound port so http_port/0 can retrieve it from tests.
    catch unregister(ridge_http_server),
    register(ridge_http_server, self()),
    put(ridge_http_port, BoundPort),
    http_accept_loop(LSock, Handler).

%% http_port/0 — retrieve the port bound by the most-recent http_listen call.
%%
%% Looks up the process registered under `ridge_http_server` and reads its
%% process-dictionary key `ridge_http_port`.  Returns the integer port or
%% {error, not_started} if http_listen has not been called.
http_port() ->
    case whereis(ridge_http_server) of
        undefined ->
            {error, not_started};
        Pid ->
            {dictionary, PD} = process_info(Pid, dictionary),
            proplists:get_value(ridge_http_port, PD, {error, not_started})
    end.

%% Internal: accept loop — one connection per iteration.
http_accept_loop(LSock, Handler) ->
    case gen_tcp:accept(LSock) of
        {ok, Sock} ->
            spawn(fun() -> http_handle_connection(Sock, Handler) end),
            http_accept_loop(LSock, Handler);
        {error, closed} ->
            ok;
        {error, _Reason} ->
            http_accept_loop(LSock, Handler)
    end.

%% Internal: read the request from one socket, call the handler, write back.
http_handle_connection(Sock, Handler) ->
    case http_recv_all(Sock, <<>>) of
        {ok, Raw} ->
            Request = http_parse_request(Raw),
            Response =
                try Handler(Request)
                catch _:_ ->
                    %% Default 500 if the handler throws.
                    #{status => 500, body => <<"internal server error">>}
                end,
            RespBin = http_build_response(Response),
            gen_tcp:send(Sock, RespBin),
            gen_tcp:close(Sock);
        {error, _} ->
            gen_tcp:close(Sock)
    end.

%% Internal: read all available data from the socket with a short timeout.
http_recv_all(Sock, Acc) ->
    case gen_tcp:recv(Sock, 0, 5000) of
        {ok, Data} ->
            %% Stop once we have a blank-line-terminated request.
            Buf = <<Acc/binary, Data/binary>>,
            case binary:match(Buf, <<"\r\n\r\n">>) of
                nomatch -> http_recv_all(Sock, Buf);
                _       -> {ok, Buf}
            end;
        {error, timeout} when Acc =/= <<>> ->
            {ok, Acc};
        {error, Reason} ->
            {error, Reason}
    end.

%% Internal: parse a raw HTTP/1.x request binary into a Ridge Request map.
%% Ridge wire shape: #{method => Bin, path => Bin, body => Bin}
%% (matches {request_record, Method, Path, Body} tuple in the BEAM; but since
%%  Ridge records compile to tagged tuples, we use the atom-keyed map form
%%  that matches the Ridge record wire representation).
http_parse_request(Raw) ->
    Lines = binary:split(Raw, <<"\r\n">>, [global]),
    {Method, Path, Body} =
        case Lines of
            [RequestLine | Rest] ->
                Parts = binary:split(RequestLine, <<" ">>, [global]),
                M = case Parts of [Meth | _] -> string:uppercase(Meth); _ -> <<"GET">> end,
                P = case Parts of [_, Pth | _] -> Pth; _ -> <<"/">> end,
                %% Find body after blank line separator.
                B = http_extract_body(Rest),
                {M, P, B};
            _ ->
                {<<"GET">>, <<"/">>, <<>>}
        end,
    %% Ridge record wire: {request_record, Method, Path, Body}
    {request_record, Method, Path, Body}.

%% Internal: extract the body from the lines after the headers.
http_extract_body([]) -> <<>>;
http_extract_body([<<>> | Rest]) ->
    iolist_to_binary(lists:join(<<"\r\n">>, Rest));
http_extract_body([_ | Rest]) ->
    http_extract_body(Rest).

%% Internal: build an HTTP/1.0 response binary from a Ridge Response record.
%% Ridge record wire: {response_record, Status, Body}
http_build_response({response_record, Status, Body}) ->
    StatusText = http_status_text(Status),
    iolist_to_binary([
        <<"HTTP/1.0 ">>, integer_to_binary(Status), <<" ">>, StatusText, <<"\r\n">>,
        <<"Content-Type: text/plain\r\n">>,
        <<"Content-Length: ">>, integer_to_binary(byte_size(Body)), <<"\r\n">>,
        <<"Connection: close\r\n">>,
        <<"\r\n">>,
        Body
    ]);
http_build_response(Other) ->
    %% Fallback for unexpected shapes.
    http_build_response({response_record, 500,
        iolist_to_binary(io_lib:format("bad response: ~p", [Other]))}).

%% Internal: map common status codes to reason phrases.
http_status_text(200) -> <<"OK">>;
http_status_text(201) -> <<"Created">>;
http_status_text(204) -> <<"No Content">>;
http_status_text(400) -> <<"Bad Request">>;
http_status_text(404) -> <<"Not Found">>;
http_status_text(500) -> <<"Internal Server Error">>;
http_status_text(_)   -> <<"Unknown">>.

%% --- HTTP client (§3.18) ---

%% http_get/1 — std.net.http.get (called via Ridge FFI wrapper)
%% Performs an HTTP GET request.  Returns Ridge result shape.
http_get(Url) ->
    http_request_no_body(get, Url).

%% http_delete/1 — std.net.http.delete
http_delete(Url) ->
    http_request_no_body(delete, Url).

%% http_post/2 — std.net.http.post
http_post(Url, Body) ->
    http_request_with_body(post, Url, Body).

%% http_put/2 — std.net.http.put
http_put(Url, Body) ->
    http_request_with_body(put, Url, Body).

%% Internal: client helper for methods with no body.
http_request_no_body(Method, Url) ->
    application:ensure_all_started(inets),
    UrlStr = binary_to_list(Url),
    try httpc:request(Method, {UrlStr, []}, [], []) of
        {ok, {{_Vsn, Status, _Phrase}, _Headers, RespBody}} ->
            BodyBin = iolist_to_binary(RespBody),
            %% Ridge Response wire: {response_record, Status, Body}
            {ok, {response_record, Status, BodyBin}};
        {error, Reason} ->
            Msg = iolist_to_binary(io_lib:format("~p", [Reason])),
            {error, {error_record, <<"http_error">>, Msg}}
    catch
        _:Err ->
            Msg = iolist_to_binary(io_lib:format("~p", [Err])),
            {error, {error_record, <<"http_error">>, Msg}}
    end.

%% Internal: client helper for methods with a body.
http_request_with_body(Method, Url, Body) ->
    application:ensure_all_started(inets),
    UrlStr  = binary_to_list(Url),
    BodyStr = binary_to_list(Body),
    try httpc:request(Method,
            {UrlStr, [], "text/plain", BodyStr},
            [], []) of
        {ok, {{_Vsn, Status, _Phrase}, _Headers, RespBody}} ->
            BodyBin = iolist_to_binary(RespBody),
            {ok, {response_record, Status, BodyBin}};
        {error, Reason} ->
            Msg = iolist_to_binary(io_lib:format("~p", [Reason])),
            {error, {error_record, <<"http_error">>, Msg}}
    catch
        _:Err ->
            Msg = iolist_to_binary(io_lib:format("~p", [Err])),
            {error, {error_record, <<"http_error">>, Msg}}
    end.

%% --- Actor runtime ---
%% ask/3: ask(Pid, Msg, Timeout) — Msg is the full {tag, arg1, ...} tuple.
%% Per OQ-E004 §8.2: generated code emits ask(Pid, {tag, args...}, TimeoutMs).
%% Timeout exit is re-raised as a structured error for Ridge source attribution.

ask(Pid, Msg, Timeout) ->
    try gen_server:call(Pid, Msg, Timeout) of
        Reply -> Reply
    catch
        exit:{timeout, _} ->
            erlang:error({ridge_rt_ask_timeout, Msg, Timeout})
    end.

send(Pid, Msg) -> gen_server:cast(Pid, Msg), ok.

spawn_actor(Mod, Init, _Caps) ->
    {ok, Pid} = gen_server:start_link(Mod, Init, []),
    Pid.

%% --- escript bridge ---

%% escript_main/1 — bridge from escript dispatch to a Ridge `pub fn main` entry point.
%%
%% The escript runtime calls `<main_module>:main([Arg1, Arg2, ...])` with a
%% single list of argument binary strings.  This function:
%%
%% 1. Converts the raw arg strings to binaries (Ridge's Text type).
%% 2. Stores the converted args in the process dictionary under
%%    `ridge_escript_args` so that `ridge_rt:cli_args/0` returns the
%%    correct arg list (without the escript script-name prefix that
%%    `init:get_plain_arguments/0` includes in escript invocations).
%%
%% The escript shim calls this function before delegating to the Ridge module:
%%
%%   main(Args) ->
%%       BinArgs = ridge_rt:escript_main(Args),
%%       case erlang:function_exported('ridge_module_0', main, 1) of
%%           true  -> 'ridge_module_0':main(BinArgs);
%%           false -> 'ridge_module_0':main()
%%       end.
%%
%% Ridge type: List Text -> List Text
escript_main(Args) ->
    %% Convert string args to binaries for Ridge's Text type.
    BinArgs = [if is_binary(A) -> A; true -> list_to_binary(A) end || A <- Args],
    %% Store in process dict so cli_args/0 returns the right args in escript mode.
    erlang:put(ridge_escript_args, BinArgs),
    BinArgs.
