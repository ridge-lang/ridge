%% ridge_rt — Ridge runtime bridge for BEAM.
%% Provides Option/Result adapters, I/O helpers, actor primitives,
%% and text/number utilities that map Ridge stdlib symbols to BEAM.
%% Bundled with ridge-codegen-erl and installed into <out_root>/runtime/.
-module(ridge_rt).
-export([
    println/1, print/1, eprintln/1,
    read_line/1,
    fs_lines/1, fs_read/1, fs_write/2, fs_append/2,
    fs_read_dir/1,
    cli_args/0, cli_args/1,
    time_now/0, time_now/1, time_epoch/0, time_epoch/1,
    time_diff_ms/2, time_diff/2,
    time_from_iso/1, time_since_ms/1, time_iso/1,
    int_parse/0, int_parse/1, float_parse/1, float_to_text/1, bool_to_text/1,
    text_split_all/2, text_replace_all/3, text_join/2, text_slice/3,
    list_fold/3, list_sort_by/2,
    random_int/2, random_choice/1, random_float/1, random_alphanumeric/1, random_seed/1,
    env_get/1, env_all/1, env_set/2,
    proc_run/2,
    json_encode/1, json_decode/1,
    json_null/0, json_null/1,
    json_bool/1, json_int/1, json_float/1, json_text/1,
    json_list/1, json_object/1,
    json_as_int/1, json_as_float/1, json_as_bool/1, json_as_text/1,
    json_as_list/1, json_as_object/1, json_is_null/1,
    http_listen/2, http_port/0, http_build_response/1,
    http_get/1, http_post/2, http_put/2, http_delete/1,
    ask/3, send/2, send_op/2, send_fn/2, mailbox_size/1, spawn_actor/3,
    mem_new/1, mem_insert/3, mem_all/2,
    mem_select/3, mem_delete/3, mem_get_rows/4,
    mem_fetch/6, mem_count_where/3, mem_project/7,
    mem_join/8, mem_join_select/9,
    quote_keep_all/1, quote_and/2,
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

%% fs_read_dir/1 — std.fs.readDir
%% Lists the immediate entries (file and subdirectory names) of a directory.
%% Returns Ridge Result shape: `{ok, [Binary]}` on success, `{error, Msg}` on
%% failure.  Entry names are bare basenames (no leading path component) and
%% are returned as UTF-8 binaries; the order matches `file:list_dir/1`'s,
%% which is unspecified — callers that need a deterministic order should
%% sort the result.
fs_read_dir(Path) ->
    case file:list_dir(Path) of
        {ok, Names} ->
            BinNames = [list_to_binary(N) || N <- Names],
            {ok, BinNames};
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
%%
%% Empty separator was the previous landmine: binary:split/3 rejects `<<>>`
%% with `badarg`, so `Text.split "" str` crashed at runtime.  An empty
%% separator now means "split on every grapheme cluster" — what the rest of
%% the std API treats as "the characters of `s`".  Multi-byte UTF-8 sequences
%% stay intact (per `string:next_grapheme/1`).  `split "" ""` yields `[]`.
text_split_all(<<>>, S) -> text_split_graphemes(S);
text_split_all(Sep, S) -> binary:split(S, Sep, [global]).

text_split_graphemes(S) when is_binary(S) ->
    text_split_graphemes_acc(S, []).

text_split_graphemes_acc(<<>>, Acc) ->
    lists:reverse(Acc);
text_split_graphemes_acc(Rest, Acc) ->
    case string:next_grapheme(Rest) of
        [] ->
            lists:reverse(Acc);
        [Grapheme | Tail] ->
            Bin = unicode:characters_to_binary([Grapheme]),
            TailBin = case Tail of
                B when is_binary(B) -> B;
                Other               -> unicode:characters_to_binary(Other)
            end,
            text_split_graphemes_acc(TailBin, [Bin | Acc])
    end.

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

%% text_join/2 — concatenate Xs with Sep between each element.
%%
%% lists:join/2 returns an iolist with the separator interleaved; we flatten
%% with iolist_to_binary so the Ridge surface is always a single binary.
%% Matches the Ridge FFI arg order (Sep, Xs).
text_join(_Sep, []) -> <<>>;
text_join(Sep, Xs)  -> iolist_to_binary(lists:join(Sep, Xs)).

%% text_slice/3 — substring counted in grapheme clusters.
%%
%% string:slice/3 saturates on the upper bound (Start >= length(S) returns
%% <<>>, Len greater than the remaining graphemes returns the rest), which
%% matches the Python s[a:a+b] / JS s.slice semantics callers expect.
%% A negative Start or Len would crash string:slice/3 with badarg, so we
%% clamp both to zero — that lets user code pass arithmetic results without
%% sprinkling guards.  Arg order matches the Ridge FFI (Start, Len, S).
text_slice(Start, Len, S) when Start >= 0, Len >= 0 ->
    Out = string:slice(S, Start, Len),
    unicode:characters_to_binary(Out);
text_slice(_, _, _) -> <<>>.

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

%% JsonValue accessors — companion destructors to the json_* constructors.
%%
%% Cross-module pattern matching on user-defined variant constructors
%% (`Json.JInt n`, `Json.JObject m`, etc.) is still deferred as of 0.2.x,
%% so a JsonValue returned from `Json.decode` cannot be destructured with
%% `match`.  These accessors give user code an Option-shaped escape hatch
%% that needs no constructor resolution: they pattern-match the tagged
%% tuple here in Erlang and surface `{some, V}` or `none` to the Ridge side.
%% Ridge types:
%%   asInt    : JsonValue -> Option Int
%%   asFloat  : JsonValue -> Option Float
%%   asBool   : JsonValue -> Option Bool
%%   asText   : JsonValue -> Option Text
%%   asList   : JsonValue -> Option (List JsonValue)
%%   asObject : JsonValue -> Option (Map Text JsonValue)
%%   isNull   : JsonValue -> Bool

json_as_int({json_int, N}) -> {some, N};
json_as_int(_)             -> none.

json_as_float({json_float, F}) -> {some, F};
json_as_float(_)               -> none.

json_as_bool({json_bool, B}) -> {some, B};
json_as_bool(_)              -> none.

json_as_text({json_text, T}) -> {some, T};
json_as_text(_)              -> none.

json_as_list({json_list, L}) -> {some, L};
json_as_list(_)              -> none.

json_as_object({json_object, M}) -> {some, M};
json_as_object(_)                -> none.

json_is_null(json_null) -> true;
json_is_null(_)         -> false.

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
%% Ridge records compile to atom-keyed maps, so the Request wire shape is
%% #{method => Bin, path => Bin, body => Bin}.
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
    #{method => Method, path => Path, body => Body}.

%% Internal: extract the body from the lines after the headers.
http_extract_body([]) -> <<>>;
http_extract_body([<<>> | Rest]) ->
    iolist_to_binary(lists:join(<<"\r\n">>, Rest));
http_extract_body([_ | Rest]) ->
    http_extract_body(Rest).

%% Internal: build an HTTP/1.0 response binary from a Ridge Response record.
%% Ridge records compile to atom-keyed maps, so the Response wire shape is
%% #{status => Int, body => Bin}.  Body coerced to binary so handlers that
%% return string literals (lists of integers) still serialise correctly.
%%
%% Security headers (T-N004 + T-N005 / Q-024): every response carries a
%% restrictive Content-Security-Policy and a 1-year Strict-Transport-
%% Security header by default. CSP `default-src 'self'` blocks third-party
%% script/style/image sources at the browser level. HSTS `max-age=31536000`
%% asks the browser to upgrade subsequent same-host requests to HTTPS for
%% one year. `includeSubDomains` and `preload` are deliberately omitted —
%% they are deployment-policy decisions that depend on whether the
%% operator owns every subdomain, so committing to them by default would
%% be wrong as often as it is right. Per-response override is deferred to
%% a future cut once Response gains a headers field.
http_build_response(#{status := Status, body := Body}) when is_integer(Status) ->
    BodyBin = iolist_to_binary(Body),
    StatusText = http_status_text(Status),
    iolist_to_binary([
        <<"HTTP/1.0 ">>, integer_to_binary(Status), <<" ">>, StatusText, <<"\r\n">>,
        <<"Content-Type: text/plain\r\n">>,
        <<"Content-Length: ">>, integer_to_binary(byte_size(BodyBin)), <<"\r\n">>,
        <<"Content-Security-Policy: default-src 'self'\r\n">>,
        <<"Strict-Transport-Security: max-age=31536000\r\n">>,
        <<"Connection: close\r\n">>,
        <<"\r\n">>,
        BodyBin
    ]);
http_build_response(Other) ->
    %% Fallback for unexpected shapes.
    http_build_response(#{status => 500,
        body => iolist_to_binary(io_lib:format("bad response: ~p", [Other]))}).

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

%% Internal: ensure the HTTP client transport stack is up.  `inets` provides
%% `httpc`; `ssl` is required by every `https://` URL that ends up calling
%% `ssl:connect/4` from `httpc_handler`.  Calling `ensure_all_started/1` is
%% idempotent — repeated invocations are no-ops once the application is up.
http_client_ensure_started() ->
    application:ensure_all_started(inets),
    application:ensure_all_started(ssl),
    ok.

%% Internal: shape an `httpc:request` return into the Ridge `Result Response Error`
%% wire.  `Response` is a record `{ status: Int, body: Text }`, which lowers
%% to an atom-keyed map; `Error` is the built-in `{ code: Text, message: Text }`,
%% also a map.  Earlier versions of these helpers emitted tagged tuples
%% (`{response_record, S, B}` / `{error_record, C, M}`), which crashed user
%% code with `badmap` the moment it touched `r.status` or `e.message` — the
%% same shape mismatch fixed for the server path in `http_parse_request`/
%% `http_build_response`.
http_client_format_ok(Status, RespBody) ->
    BodyBin = iolist_to_binary(RespBody),
    {ok, #{status => Status, body => BodyBin}}.

http_client_format_err(Reason) ->
    Msg = iolist_to_binary(io_lib:format("~p", [Reason])),
    {error, #{code => <<"http_error">>, message => Msg}}.

%% Internal: default request headers sent with every client request.
%% httpc's built-in default User-Agent (`httpc/X.Y`) is rejected by several
%% real-world APIs (GitHub returns HTTP 403 "User-Agent header required" for
%% it, for example).  A Ridge-identifying string keeps the out-of-the-box
%% experience working against those APIs.  Custom headers are deferred per
%% std.net.http scope guard.
http_client_default_headers() ->
    [{"User-Agent", "ridge-lang/0.2"}].

%% Internal: client helper for methods with no body.
http_request_no_body(Method, Url) ->
    http_client_ensure_started(),
    UrlStr = binary_to_list(Url),
    try httpc:request(Method, {UrlStr, http_client_default_headers()}, [], []) of
        {ok, {{_Vsn, Status, _Phrase}, _Headers, RespBody}} ->
            http_client_format_ok(Status, RespBody);
        {error, Reason} ->
            http_client_format_err(Reason)
    catch
        _:Err ->
            http_client_format_err(Err)
    end.

%% Internal: client helper for methods with a body.
http_request_with_body(Method, Url, Body) ->
    http_client_ensure_started(),
    UrlStr  = binary_to_list(Url),
    BodyStr = binary_to_list(Body),
    try httpc:request(Method,
            {UrlStr, http_client_default_headers(), "text/plain", BodyStr},
            [], []) of
        {ok, {{_Vsn, Status, _Phrase}, _Headers, RespBody}} ->
            http_client_format_ok(Status, RespBody);
        {error, Reason} ->
            http_client_format_err(Reason)
    catch
        _:Err ->
            http_client_format_err(Err)
    end.

%% --- Actor runtime ---
%%
%% Handle wire format (since 0.2.7):
%%
%%   Handle a ≡ {ridge_handle, Pid, MailboxConfig}
%%   MailboxConfig ≡ unbounded
%%                 | {bounded, pos_integer(), drop_newest | error}
%%
%% Handles are opaque at the Ridge surface (spec §7.2). Anything inside this
%% module that takes a handle expects the tuple shape above. The legacy
%% bare-Pid path is no longer reachable from Ridge-emitted code.

%% ask/3 — synchronous request/response. Bounded mailbox policies do not
%% apply: ask is a request/response primitive, not a backpressure surface.
%% Timeout exit is re-raised as a structured error for Ridge source attribution.
ask({ridge_handle, Pid, _Config}, Msg, Timeout) ->
    try gen_server:call(Pid, Msg, Timeout) of
        Reply -> Reply
    catch
        exit:{timeout, _} ->
            erlang:error({ridge_rt_ask_timeout, Msg, Timeout})
    end.

%% send/2 — fire-and-forget cast that ignores bounded-mailbox policies.
%% Retained as a backward-compatible bridge so callers built before 0.2.7
%% (and any hand-written Erlang glue) still work. Ridge `!` emits send_op/2
%% instead, which honours the mailbox configuration carried by the handle.
send({ridge_handle, Pid, _Config}, Msg) ->
    gen_server:cast(Pid, Msg),
    ok.

%% send_op/2 — target of the Ridge `!` operator. Honours the bounded-mailbox
%% policy carried by the handle. The drop_newest path silently drops the
%% incoming message on overflow; the error path raises in the caller so the
%% supervisor decides what to do. Both paths treat a dead actor as a no-op
%% to keep `gen_server:cast` semantics.
send_op({ridge_handle, Pid, unbounded}, Msg) ->
    gen_server:cast(Pid, Msg),
    ok;
send_op({ridge_handle, Pid, {bounded, N, drop_newest}}, Msg) ->
    case erlang:process_info(Pid, message_queue_len) of
        {message_queue_len, Len} when Len >= N -> ok;
        {message_queue_len, _Len}              -> gen_server:cast(Pid, Msg), ok;
        undefined                              -> ok
    end;
send_op({ridge_handle, Pid, {bounded, N, error}}, Msg) ->
    case erlang:process_info(Pid, message_queue_len) of
        {message_queue_len, Len} when Len >= N -> erlang:error({mailbox_full, Pid});
        {message_queue_len, _Len}              -> gen_server:cast(Pid, Msg), ok;
        undefined                              -> ok
    end.

%% send_fn/2 — target of the stdlib `Actor.send` fn. Result-returning variant
%% of send_op. Drop-newest reports success (the user opted into silent drop);
%% error reports {error, mailbox_full} so the caller can recover.
send_fn({ridge_handle, Pid, unbounded}, Msg) ->
    gen_server:cast(Pid, Msg),
    {ok};
send_fn({ridge_handle, Pid, {bounded, N, drop_newest}}, Msg) ->
    case erlang:process_info(Pid, message_queue_len) of
        {message_queue_len, Len} when Len >= N -> {ok};
        {message_queue_len, _Len}              -> gen_server:cast(Pid, Msg), {ok};
        undefined                              -> {ok}
    end;
send_fn({ridge_handle, Pid, {bounded, N, error}}, Msg) ->
    case erlang:process_info(Pid, message_queue_len) of
        {message_queue_len, Len} when Len >= N -> {error, mailbox_full};
        {message_queue_len, _Len}              -> gen_server:cast(Pid, Msg), {ok};
        undefined                              -> {ok}
    end.

%% mailbox_size/1 — target of the stdlib `Actor.mailboxSize` fn.
%% Returns {some, N} for a live actor or none for a dead one. The reading
%% is instantaneous and may briefly disagree with concurrent senders; the
%% spec documents this as a soft-bound invariant.
mailbox_size({ridge_handle, Pid, _Config}) ->
    case erlang:process_info(Pid, message_queue_len) of
        {message_queue_len, Len} -> {some, Len};
        undefined                -> none
    end.

%% spawn_actor/3 — wraps gen_server:start_link/3 and decorates the result
%% with the actor's declared mailbox configuration. The actor module exports
%% '__ridge_mailbox_config'/0 (emitted by ridge-codegen-erl); spawn_actor
%% looks it up once at spawn time so the resulting handle carries the config
%% with no per-send dispatch cost.
spawn_actor(Mod, Init, _Caps) ->
    {ok, Pid} = gen_server:start_link(Mod, Init, []),
    Config =
        case erlang:function_exported(Mod, '__ridge_mailbox_config', 0) of
            true  -> Mod:'__ridge_mailbox_config'();
            false -> unbounded
        end,
    {ridge_handle, Pid, Config}.

%% --- In-memory data store (std.data MemAdapter) ---
%%
%% The in-memory adapter keeps every table in one keeper process registered as
%% `ridge_mem_keeper`, holding a map of {StoreId, Table} => [Row]. mem_new/1
%% allocates a fresh StoreId, so independent adapters never share rows; the
%% MemAdapter handle is the record map #{id => StoreId}. Inserts and reads are
%% serialised through the keeper, so concurrent callers see consistent state.
%% Rows cross the boundary as opaque maps (#{<<"col">> => SqlValue}); the store
%% never inspects them. The keeper is spawned unlinked and lives for the node's
%% lifetime — this is a dev/test store, not durable storage.

%% mem_new/1 — std.data.memAdapter. Returns the MemAdapter record #{id => Id}.
mem_new(_Unit) ->
    mem_ensure(),
    Id = erlang:unique_integer([positive, monotonic]),
    #{id => Id}.

%% mem_insert/3 — append Row to Table in store Id. Result Unit Error.
mem_insert(Id, Table, Row) -> mem_call({insert, Id, Table, Row}).

%% mem_all/2 — every row of Table in store Id, in insertion order.
%% Result (List Row) Error; an unknown table reads as empty.
mem_all(Id, Table) -> mem_call({all, Id, Table}).

%% quote_keep_all/1 — std.repo.keepAll. An always-true quoted predicate, the
%% every-row case the full-table and aggregate verbs run through `select`. A
%% Quote is `#{tree => QExpr}`; the tree `{'QLitBool', true}` keeps every row.
%% Built here because a Quote/QExpr literal cannot be written in Ridge source.
quote_keep_all(_Unit) -> #{tree => {'QLitBool', true}}.

%% quote_and/2 — std.repo's query builder combines two quoted predicates with a
%% boolean AND. A `Quote`/`QExpr` literal cannot be written in Ridge source, so
%% the `QAnd` node is assembled here from the two captured trees. The builder
%% starts from `quote_keep_all` and folds each `where` clause in with this, so a
%% chain of filters becomes a nested `QAnd` the seam compiles or walks as one.
quote_and(A, B) ->
    #{tree => {'QAnd', maps:get(tree, A), maps:get(tree, B)}}.

%% mem_select/3 — the rows of Table that satisfy the captured predicate Tree.
%% The runtime walks the QExpr against each row (the in-memory dual of compiling
%% it to a SQL WHERE clause). Result (List Row) Error.
mem_select(Id, Table, Tree) -> mem_call({select, Id, Table, Tree}).

%% mem_get_rows/4 — the rows of Table whose Column holds exactly Key. std.data's
%% `get` takes the first. Result (List Row) Error.
mem_get_rows(Id, Table, Column, Key) -> mem_call({get_rows, Id, Table, Column, Key}).

%% mem_delete/3 — remove the rows of Table that satisfy Tree; answer how many
%% were removed. Result Int Error.
mem_delete(Id, Table, Tree) -> mem_call({delete, Id, Table, Tree}).

%% mem_fetch/6 — the rows of Table that satisfy Tree, ordered by Orders, then
%% offset and limited. Orders is a list of `{Asc, Column}` where Asc is the
%% boolean `true` for ascending; sorting is stable and applied major-to-minor
%% (the first key is the primary sort). Lim < 0 means no limit and Off =< 0 means
%% no offset. This is the in-memory dual of a backend pushing ORDER BY / LIMIT /
%% OFFSET into the query. Result (List Row) Error.
mem_fetch(Id, Table, Tree, Orders, Lim, Off) ->
    mem_call({fetch, Id, Table, Tree, Orders, Lim, Off}).

%% mem_count_where/3 — how many rows of Table satisfy Tree, counted without
%% returning them (the in-memory dual of SELECT COUNT(*)). Result Int Error.
mem_count_where(Id, Table, Tree) -> mem_call({count_where, Id, Table, Tree}).

%% mem_project/7 — the rows of Table that satisfy Tree, ordered and paged as
%% mem_fetch, then projected to the `{Alias, Column}` columns: each row keeps
%% only those columns, re-keyed by alias. Result (List Row) Error.
mem_project(Id, Table, Tree, Orders, Lim, Off, Cols) ->
    mem_call({project, Id, Table, Tree, Orders, Lim, Off, Cols}).

%% mem_join/8 — inner-join LeftTable and RightTable on the condition tree Cond,
%% keep the left rows matching the predicate tree Pred, order by the left-column
%% keys, then page. Each result is the `{LeftRow, RightRow}` pair of column maps.
%% Result (List {Row, Row}) Error.
mem_join(Id, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off) ->
    mem_call({join, Id, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off}).

%% mem_join_select/9 — as mem_join, then project each joined pair through the
%% projection tree Proj into one map keyed by the projection's aliases. Result
%% (List Row) Error.
mem_join_select(Id, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off, Proj) ->
    mem_call({join_select, Id, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off, Proj}).

%% Internal: send a request to the keeper and await its reply.
mem_call(Req) ->
    mem_ensure(),
    Ref = make_ref(),
    ridge_mem_keeper ! {Req, self(), Ref},
    receive
        {Ref, Reply} -> Reply
    after 5000 ->
        {error, #{code => <<"db.timeout">>,
                  message => <<"in-memory store request timed out">>}}
    end.

%% Internal: start the keeper on first use; idempotent and race-tolerant.
mem_ensure() ->
    case whereis(ridge_mem_keeper) of
        undefined ->
            spawn(fun mem_keeper_init/0),
            mem_wait_keeper(200);
        _Pid ->
            ok
    end.

mem_wait_keeper(0) -> ok;
mem_wait_keeper(N) ->
    case whereis(ridge_mem_keeper) of
        undefined -> timer:sleep(5), mem_wait_keeper(N - 1);
        _Pid      -> ok
    end.

%% Internal: register under the keeper name and run the store loop. If another
%% process won the registration race, badarg is caught and this one exits.
mem_keeper_init() ->
    case catch register(ridge_mem_keeper, self()) of
        true -> mem_keeper_loop(#{});
        _    -> ok
    end.

mem_keeper_loop(State) ->
    receive
        {{insert, Id, Table, Row}, From, Ref} ->
            Key  = {Id, Table},
            Rows = maps:get(Key, State, []),
            From ! {Ref, {ok, ok}},
            mem_keeper_loop(State#{Key => Rows ++ [Row]});
        {{all, Id, Table}, From, Ref} ->
            Rows = maps:get({Id, Table}, State, []),
            From ! {Ref, {ok, Rows}},
            mem_keeper_loop(State);
        {{select, Id, Table, Tree}, From, Ref} ->
            Rows = maps:get({Id, Table}, State, []),
            Matches = [R || R <- Rows, mem_pred(Tree, R)],
            From ! {Ref, {ok, Matches}},
            mem_keeper_loop(State);
        {{get_rows, Id, Table, Column, Key}, From, Ref} ->
            Rows = maps:get({Id, Table}, State, []),
            Matches = [R || R <- Rows, maps:get(Column, R, 'SqlNull') =:= Key],
            From ! {Ref, {ok, Matches}},
            mem_keeper_loop(State);
        {{delete, Id, Table, Tree}, From, Ref} ->
            Key  = {Id, Table},
            Rows = maps:get(Key, State, []),
            Kept = [R || R <- Rows, not mem_pred(Tree, R)],
            Removed = length(Rows) - length(Kept),
            From ! {Ref, {ok, Removed}},
            mem_keeper_loop(State#{Key => Kept});
        {{fetch, Id, Table, Tree, Orders, Lim, Off}, From, Ref} ->
            Rows = maps:get({Id, Table}, State, []),
            Matches = [R || R <- Rows, mem_pred(Tree, R)],
            Page = mem_paginate(mem_order(Orders, Matches), Lim, Off),
            From ! {Ref, {ok, Page}},
            mem_keeper_loop(State);
        {{count_where, Id, Table, Tree}, From, Ref} ->
            Rows = maps:get({Id, Table}, State, []),
            N = length([R || R <- Rows, mem_pred(Tree, R)]),
            From ! {Ref, {ok, N}},
            mem_keeper_loop(State);
        {{project, Id, Table, Tree, Orders, Lim, Off, Cols}, From, Ref} ->
            Rows = maps:get({Id, Table}, State, []),
            Matches = [R || R <- Rows, mem_pred(Tree, R)],
            Page = mem_paginate(mem_order(Orders, Matches), Lim, Off),
            Projected = [mem_project_row(Cols, R) || R <- Page],
            From ! {Ref, {ok, Projected}},
            mem_keeper_loop(State);
        {{join, Id, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off}, From, Ref} ->
            Pairs = mem_join_pairs(State, Id, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off),
            From ! {Ref, {ok, Pairs}},
            mem_keeper_loop(State);
        {{join_select, Id, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off, Proj}, From, Ref} ->
            Pairs = mem_join_pairs(State, Id, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off),
            Projected = [mem_join_project(Proj, L, R) || {L, R} <- Pairs],
            From ! {Ref, {ok, Projected}},
            mem_keeper_loop(State)
    end.

%% Build a projected row from `{Alias, Column}` pairs: each output column reads
%% the source column from the row and is keyed by its alias. A missing source
%% column reads as SQL NULL.
mem_project_row(Cols, Row) ->
    maps:from_list([{Alias, maps:get(Col, Row, 'SqlNull')} || {Alias, Col} <- Cols]).

%% --- In-memory inner join ---
%%
%% The nested-loop dual of a backend pushing a JOIN into SQL. Keep the left rows
%% the left-side predicate matches, pair each with every right row the condition
%% accepts, order the pairs by the left-column keys, then page. The condition is
%% a QExpr over both rows: a `QCol` reads the left row, a `QColR` the right.

mem_join_pairs(State, Id, LeftTable, RightTable, Cond, Pred, Orders, Lim, Off) ->
    LeftRows = maps:get({Id, LeftTable}, State, []),
    RightRows = maps:get({Id, RightTable}, State, []),
    LeftMatches = [L || L <- LeftRows, mem_pred(Pred, L)],
    Pairs = [{L, R} || L <- LeftMatches, R <- RightRows, mem_jpred(Cond, L, R)],
    mem_paginate(mem_order_pairs(Orders, Pairs), Lim, Off).

%% Order joined pairs by the left-column keys (the left query's ordering). The
%% key reads from the left row of each pair; lists:sort/2 is stable.
mem_order_pairs([], Pairs) -> Pairs;
mem_order_pairs(Orders, Pairs) ->
    lists:sort(fun({LA, _}, {LB, _}) -> mem_le(Orders, LA, LB) end, Pairs).

%% Project a joined pair through a QProj select-list into one map keyed by alias:
%% each column reads the left or right row depending on its `QCol`/`QColR` tag.
mem_join_project({'QProj', Cols}, L, R) ->
    maps:from_list([{Alias, mem_jcell(Col, L, R)} || {Alias, Col} <- Cols]);
mem_join_project(_Other, _L, _R) ->
    #{}.

mem_jcell({'QCol', C}, L, _R)  -> maps:get(C, L, 'SqlNull');
mem_jcell({'QColR', C}, _L, R) -> maps:get(C, R, 'SqlNull');
mem_jcell(_Other, _L, _R)      -> 'SqlNull'.

%% Evaluate a join condition node against the (left, right) pair. The structure
%% mirrors mem_pred/2; a `QCol` resolves against the left row and a `QColR`
%% against the right.
mem_jpred({'QAnd', A, B}, L, R)    -> mem_jpred(A, L, R) andalso mem_jpred(B, L, R);
mem_jpred({'QOr', A, B}, L, R)     -> mem_jpred(A, L, R) orelse mem_jpred(B, L, R);
mem_jpred({'QNot', X}, L, R)       -> not mem_jpred(X, L, R);
mem_jpred({'QEq', A, B}, L, R)     -> mem_jrelate(eq, A, B, L, R);
mem_jpred({'QNe', A, B}, L, R)     -> not mem_jrelate(eq, A, B, L, R);
mem_jpred({'QLt', A, B}, L, R)     -> mem_jrelate(lt, A, B, L, R);
mem_jpred({'QGt', A, B}, L, R)     -> mem_jrelate(lt, B, A, L, R);
mem_jpred({'QLe', A, B}, L, R)     -> not mem_jrelate(lt, B, A, L, R);
mem_jpred({'QGe', A, B}, L, R)     -> not mem_jrelate(lt, A, B, L, R);
mem_jpred({'QCol', C}, L, _R)      -> mem_truthy(maps:get(C, L, 'SqlNull'));
mem_jpred({'QColR', C}, _L, R)     -> mem_truthy(maps:get(C, R, 'SqlNull'));
mem_jpred({'QLitBool', B}, _L, _R) -> B;
mem_jpred(_Other, _L, _R)          -> false.

mem_jrelate(Op, A, B, L, R) ->
    case {mem_jscalar(A, L, R), mem_jscalar(B, L, R)} of
        {undefined, _} -> false;
        {_, undefined} -> false;
        {X, Y}         -> mem_sql_cmp(Op, X, Y)
    end.

mem_jscalar({'QCol', C}, L, _R) ->
    case maps:find(C, L) of
        {ok, V} -> V;
        error   -> undefined
    end;
mem_jscalar({'QColR', C}, _L, R) ->
    case maps:find(C, R) of
        {ok, V} -> V;
        error   -> undefined
    end;
mem_jscalar({'QLitInt', N}, _L, _R)   -> {'SqlInt', N};
mem_jscalar({'QLitText', S}, _L, _R)  -> {'SqlText', S};
mem_jscalar({'QLitBool', B}, _L, _R)  -> {'SqlBool', B};
mem_jscalar({'QLitFloat', F}, _L, _R) -> {'SqlFloat', F};
mem_jscalar(_Other, _L, _R)           -> undefined.

%% --- Quoted-predicate interpreter (the in-memory dual of Query.toSql) ---
%%
%% A captured predicate reaches the runtime as a QExpr tree: union variants are
%% tagged tuples ({'QCol', <<"col">>}, {'QLitInt', N}, {'QAnd', L, R}, …) and the
%% leaf bind values are SqlValue tuples ({'SqlInt', N}, {'SqlText', <<…>>}, …).
%% `mem_pred/2` answers whether one row satisfies the tree; the quotation checker
%% has already verified the operand types line up, so a missing column or a
%% cross-type comparison just fails to match rather than crashing.

%% Evaluate a predicate node against a row.
mem_pred({'QAnd', L, R}, Row) -> mem_pred(L, Row) andalso mem_pred(R, Row);
mem_pred({'QOr', L, R}, Row)  -> mem_pred(L, Row) orelse mem_pred(R, Row);
mem_pred({'QNot', X}, Row)    -> not mem_pred(X, Row);
mem_pred({'QEq', L, R}, Row)  -> mem_relate(eq, L, R, Row);
mem_pred({'QNe', L, R}, Row)  -> not mem_relate(eq, L, R, Row);
mem_pred({'QLt', L, R}, Row)  -> mem_relate(lt, L, R, Row);
mem_pred({'QGt', L, R}, Row)  -> mem_relate(lt, R, L, Row);
mem_pred({'QLe', L, R}, Row)  -> not mem_relate(lt, R, L, Row);
mem_pred({'QGe', L, R}, Row)  -> not mem_relate(lt, L, R, Row);
%% A bare leaf in predicate position is a boolean column or literal.
mem_pred({'QCol', C}, Row)      -> mem_truthy(maps:get(C, Row, 'SqlNull'));
mem_pred({'QLitBool', B}, _Row) -> B;
mem_pred(_Other, _Row)          -> false.

%% Compare two operands resolved against the row. A node that is not a scalar
%% (column or literal) has no value, so the comparison fails.
mem_relate(Op, L, R, Row) ->
    case {mem_scalar(L, Row), mem_scalar(R, Row)} of
        {undefined, _} -> false;
        {_, undefined} -> false;
        {A, B}         -> mem_sql_cmp(Op, A, B)
    end.

%% A comparison operand's bind value: a column reads the row, a literal builds
%% its SqlValue tuple. Anything else has no scalar value.
mem_scalar({'QCol', C}, Row) ->
    case maps:find(C, Row) of
        {ok, V} -> V;
        error   -> undefined
    end;
mem_scalar({'QLitInt', N}, _Row)   -> {'SqlInt', N};
mem_scalar({'QLitText', S}, _Row)  -> {'SqlText', S};
mem_scalar({'QLitBool', B}, _Row)  -> {'SqlBool', B};
mem_scalar({'QLitFloat', F}, _Row) -> {'SqlFloat', F};
mem_scalar(_Other, _Row)           -> undefined.

%% Equality is exact and type-aware (the tags must match); ordering is defined
%% only for the ordered base types and answers `false` for anything else.
mem_sql_cmp(eq, A, B) -> A =:= B;
mem_sql_cmp(lt, {'SqlInt', X}, {'SqlInt', Y})     -> X < Y;
mem_sql_cmp(lt, {'SqlText', X}, {'SqlText', Y})   -> X < Y;
mem_sql_cmp(lt, {'SqlFloat', X}, {'SqlFloat', Y}) -> X < Y;
mem_sql_cmp(lt, _A, _B) -> false.

%% A SqlValue used directly as a predicate: a SqlBool yields its boolean.
mem_truthy({'SqlBool', B}) -> B;
mem_truthy(_Other)         -> false.

%% --- In-memory ORDER BY / LIMIT / OFFSET ---
%%
%% The dual of a backend pushing ordering and paging into SQL. Ordering sorts the
%% matched rows by the key list (major key first); paging drops `Off` rows then
%% keeps `Lim` (a negative `Lim` keeps all).

%% Sort rows by the order keys. No keys means no reordering. lists:sort/2 is
%% stable, so rows equal under every key keep their insertion order.
mem_order([], Rows) -> Rows;
mem_order(Orders, Rows) -> lists:sort(fun(A, B) -> mem_le(Orders, A, B) end, Rows).

%% Whether row A should sort no later than row B under the key list. The first
%% key that distinguishes them decides; ties fall through to the next key. Each
%% key carries `Asc` as the boolean `true`: A precedes B on `<` when ascending
%% and on `>` when descending.
mem_le([], _A, _B) -> true;
mem_le([{Asc, Col} | Rest], A, B) ->
    case mem_order_cmp(mem_scalar({'QCol', Col}, A), mem_scalar({'QCol', Col}, B)) of
        eq -> mem_le(Rest, A, B);
        lt -> Asc;
        gt -> not Asc
    end.

%% Three-way compare of two column values. Incomparable or missing values (a
%% column absent from the row reads as `undefined`) compare equal, so they keep
%% insertion order rather than crashing the sort.
mem_order_cmp(V, V) -> eq;
mem_order_cmp(A, B) ->
    case mem_sql_cmp(lt, A, B) of
        true  -> lt;
        false ->
            case mem_sql_cmp(lt, B, A) of
                true  -> gt;
                false -> eq
            end
    end.

%% Drop `Off` rows (when positive), then keep `Lim` (a negative `Lim` keeps all).
mem_paginate(Rows, Lim, Off) ->
    Dropped = if Off > 0 -> mem_drop(Off, Rows); true -> Rows end,
    if Lim < 0 -> Dropped; true -> mem_take(Lim, Dropped) end.

mem_drop(0, Rows)     -> Rows;
mem_drop(_, [])       -> [];
mem_drop(N, [_ | T])  -> mem_drop(N - 1, T).

mem_take(0, _)        -> [];
mem_take(_, [])       -> [];
mem_take(N, [H | T])  -> [H | mem_take(N - 1, T)].

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
