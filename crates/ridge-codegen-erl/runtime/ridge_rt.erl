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
    mem_select/3, mem_delete/3, mem_update/4, mem_get_rows/4,
    mem_fetch/7, mem_count_where/3, mem_aggregate/5, mem_project/8,
    mem_join/10, mem_join_select/11, mem_left_join/10, mem_left_join_select/11,
    mem_right_join/10, mem_right_join_select/11,
    mem_full_join/10, mem_full_join_select/11,
    mem_aggregate_join/9, mem_aggregate_left_join/9, mem_aggregate_right_join/9,
    mem_aggregate_full_join/9,
    mem_count_join/6, mem_count_left_join/6, mem_count_right_join/6,
    mem_count_full_join/6,
    mem_group_summarize/6,
    mem_group_summarize_join/10, mem_group_summarize_left_join/10,
    mem_group_summarize_right_join/10, mem_group_summarize_full_join/10,
    mem_begin/1, mem_commit/1, mem_rollback/1, mem_close/1,
    mem_ddl_create/3, mem_ddl_drop/2, mem_ddl_add_column/3,
    mem_ddl_drop_column/3, mem_ddl_index/5,
    mem_migrations_applied/1, mem_record_migration/2,
    mem_raw_query/3, mem_raw_exec/3,
    mem_run_plan/2,
    quote_keep_all/1, quote_and/2,
    mk_error/2,
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

%% mk_error/2 — build an `Error` record from a code and a message. `Error` is a
%% builtin record `{ code: Text, message: Text }`, which codegen lowers to an
%% atom-keyed map (field access `e.code` compiles to `maps:get(code, _)`). A bare
%% record literal cannot be coerced to the nominal `Error` type outside an
%% instance-method body, so the unique-row terminals build their errors here.
mk_error(Code, Message) ->
    #{code => Code, message => Message}.

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

%% mem_update/4 — set the Changes columns on the rows of Table that satisfy Tree;
%% answer how many rows changed. Changes is a `#{Column => SqlValue}` map merged
%% over each matching row. Result Int Error.
mem_update(Id, Table, Changes, Tree) -> mem_call({update, Id, Table, Changes, Tree}).

%% mem_fetch/6 — the rows of Table that satisfy Tree, ordered by Orders, then
%% offset and limited. Orders is a list of `{Asc, Column}` where Asc is the
%% boolean `true` for ascending; sorting is stable and applied major-to-minor
%% (the first key is the primary sort). Lim < 0 means no limit and Off =< 0 means
%% no offset. This is the in-memory dual of a backend pushing ORDER BY / LIMIT /
%% OFFSET into the query. Result (List Row) Error.
mem_fetch(Id, Table, Tree, Orders, Lim, Off, Dist) ->
    mem_call({fetch, Id, Table, Tree, Orders, Lim, Off, Dist}).

%% mem_count_where/3 — how many rows of Table satisfy Tree, counted without
%% returning them (the in-memory dual of SELECT COUNT(*)). Result Int Error.
mem_count_where(Id, Table, Tree) -> mem_call({count_where, Id, Table, Tree}).

%% mem_aggregate/5 — fold a scalar aggregate (Func is <<"SUM">>/<<"AVG">>/
%% <<"MIN">>/<<"MAX">>) over Column across the rows of Table that satisfy Tree.
%% The single scalar comes back as a SqlValue, or 'SqlNull' when no row matches
%% (the in-memory dual of a SQL aggregate over an empty set). Result SqlValue
%% Error.
mem_aggregate(Id, Table, Tree, Func, Column) ->
    mem_call({aggregate, Id, Table, Tree, Func, Column}).

%% mem_project/7 — the rows of Table that satisfy Tree, ordered and paged as
%% mem_fetch, then projected to the `{Alias, Column}` columns: each row keeps
%% only those columns, re-keyed by alias. Result (List Row) Error.
mem_project(Id, Table, Tree, Orders, Lim, Off, Cols, Dist) ->
    mem_call({project, Id, Table, Tree, Orders, Lim, Off, Cols, Dist}).

%% mem_group_summarize/6 — group the rows of Table that satisfy Tree by KeyCol,
%% summarize each group into the `{Alias, Func, Column}` aggregates (Func is
%% <<"KEY">>/<<"COUNT">>/<<"SUM">>/<<"AVG">>/<<"MIN">>/<<"MAX">>), keep the groups
%% the Having tree admits, and return one row per group keyed by alias, ordered by
%% the key. The in-memory dual of SELECT … GROUP BY … HAVING …. Result (List Row)
%% Error.
mem_group_summarize(Id, Table, Tree, KeyCol, Cols, Having) ->
    mem_call({group_summarize, Id, Table, Tree, KeyCol, Cols, Having}).

%% mem_group_summarize_join/10 — the inner-join dual of mem_group_summarize: pair
%% LeftTable and RightTable on Cond (narrowed by Where2 and the left-side Pred),
%% group the pairs by KeyCol read off the KeySide table, summarize each group into
%% the `{Alias, Func, Column, IsRight}` aggregates (IsRight selects the table a
%% scalar aggregate folds), keep the groups Having admits, and return one row per
%% group ordered by the key. Result (List Row) Error.
mem_group_summarize_join(Id, LeftTable, RightTable, Cond, Where2, Pred, KeyCol, KeySide, Cols, Having) ->
    mem_call({group_summarize_join, Id, LeftTable, RightTable, Cond, Where2, Pred, KeyCol, KeySide, Cols, Having}).

%% mem_group_summarize_left_join/10 — as mem_group_summarize_join, but a left-outer
%% join keeps every left row Pred and Where2 admit; an unmatched one groups with its
%% right columns absent (read as SqlNull), so a right-side key groups it under NULL.
mem_group_summarize_left_join(Id, LeftTable, RightTable, Cond, Where2, Pred, KeyCol, KeySide, Cols, Having) ->
    mem_call({group_summarize_left_join, Id, LeftTable, RightTable, Cond, Where2, Pred, KeyCol, KeySide, Cols, Having}).

%% mem_group_summarize_right_join/10 — as mem_group_summarize_left_join, but the
%% right-outer mirror: every right row is grouped, the left query's Pred folds into
%% the join match so an unmatched right row keeps a NULL (absent) left side, and a
%% left-side key groups it under NULL.
mem_group_summarize_right_join(Id, LeftTable, RightTable, Cond, Where2, Pred, KeyCol, KeySide, Cols, Having) ->
    mem_call({group_summarize_right_join, Id, LeftTable, RightTable, Cond, Where2, Pred, KeyCol, KeySide, Cols, Having}).

%% mem_group_summarize_full_join/10 — as mem_group_summarize_right_join, but the
%% full-outer join: every row of both tables is grouped, the left query's Pred
%% restricting which left rows enter, and a key over a side groups the rows unmatched
%% on that side under the NULL (absent) key.
mem_group_summarize_full_join(Id, LeftTable, RightTable, Cond, Where2, Pred, KeyCol, KeySide, Cols, Having) ->
    mem_call({group_summarize_full_join, Id, LeftTable, RightTable, Cond, Where2, Pred, KeyCol, KeySide, Cols, Having}).

%% mem_begin/1 — open a transaction on store Id: snapshot its tables onto a
%% per-store stack the keeper holds. A nested begin snapshots again (a savepoint).
%% Result Unit Error.
mem_begin(Id) -> mem_call({begin_tx, Id}).

%% mem_commit/1 — commit the innermost open transaction on store Id: drop its
%% snapshot, keeping the current rows. Result Unit Error.
mem_commit(Id) -> mem_call({commit_tx, Id}).

%% mem_rollback/1 — roll back the innermost open transaction on store Id: restore
%% the tables to the snapshot taken at the matching begin. Result Unit Error.
mem_rollback(Id) -> mem_call({rollback_tx, Id}).

%% mem_close/1 — std.data.close. Forget every table (and any open transaction
%% snapshot) of store Id, freeing its rows. The in-memory store lives in the keeper
%% for the BEAM's lifetime, so a program opening many adapters without closing them
%% would accumulate; close releases the store. Result Unit Error.
mem_close(Id) -> mem_call({close_store, Id}).

%% --- schema / migrations ---
%% The in-memory store is schemaless: a create materialises an empty table so it
%% exists for reads and drops, a drop forgets it, and column/index changes are
%% no-ops (a missing column already reads as SqlNull, and there are no indexes).

%% mem_ddl_create/3 — materialise an empty table in store Id; columns ignored.
%% Result Unit Error.
mem_ddl_create(Id, Table, _Cols) -> mem_call({create_table, Id, Table}).

%% mem_ddl_drop/2 — forget Table and its rows in store Id. Result Unit Error.
mem_ddl_drop(Id, Table) -> mem_call({drop_table, Id, Table}).

%% mem_ddl_add_column/3 — no-op on the schemaless store. Result Unit Error.
mem_ddl_add_column(_Id, _Table, _Col) -> {ok, ok}.

%% mem_ddl_drop_column/3 — no-op on the schemaless store. Result Unit Error.
mem_ddl_drop_column(_Id, _Table, _Column) -> {ok, ok}.

%% mem_ddl_index/5 — no-op on the schemaless store. Result Unit Error.
mem_ddl_index(_Id, _Name, _Table, _Cols, _Unique) -> {ok, ok}.

%% mem_migrations_applied/1 — the names already recorded in store Id's tracking
%% table, oldest first; an absent table reads as empty. Result (List Text) Error.
mem_migrations_applied(Id) ->
    case mem_all(Id, <<"_ridge_migrations">>) of
        {ok, Rows} -> {ok, [mem_migration_name(R) || R <- Rows]};
        {error, E} -> {error, E}
    end.

%% mem_record_migration/2 — append Name to store Id's tracking table, the same
%% row shape a `name text` column holds. Result Unit Error.
mem_record_migration(Id, Name) ->
    mem_insert(Id, <<"_ridge_migrations">>, #{<<"name">> => {'SqlText', Name}}).

%% --- raw SQL ---
%% The in-memory store has no SQL engine, so the raw-SQL escape hatch cannot run
%% against it. Both verbs report a clear error pointing at a SQL backend rather
%% than silently doing nothing — raw SQL is a deliberate drop to a database's own
%% dialect (std.raw), so a program reaching for it on the memory store is told so.

%% mem_raw_query/3 — unsupported on the in-memory store. Result (List Row) Error.
mem_raw_query(_Id, _Sql, _Params) -> {error, raw_unsupported_error()}.

%% mem_raw_exec/3 — unsupported on the in-memory store. Result Int Error.
mem_raw_exec(_Id, _Sql, _Params) -> {error, raw_unsupported_error()}.

raw_unsupported_error() ->
    #{code => <<"raw.unsupported">>,
      message => <<"raw SQL needs a SQL backend; the in-memory adapter has none — run it against Postgres">>}.

%% The `name` text out of a tracking-table row.
mem_migration_name(Row) ->
    case maps:get(<<"name">>, Row, 'SqlNull') of
        {'SqlText', N} -> N;
        _              -> <<>>
    end.

%% --- query plan builders ---
%% A query plan is a small tree the set-operation terminals run. The three builders
%% assemble it backend-neutrally (no store access); mem_run_plan/pg_run_plan
%% interpret or compile it. The tree rides the QExpr term space but is only ever
%% handled by the plan interpreters, never by mem_pred/compile_where.

%% A query plan is now built in Ridge (std.query's planScan/planCombine/planRefine
%% over the typed QueryPlan tree) and crosses the FFI as a tagged tuple
%% ({'PlanScan', …}/{'PlanCombine', …}/{'PlanRefine', …}); mem_eval_plan/3
%% interprets those tags directly, so no Erlang-side plan builders are needed.

%% mem_run_plan/2 — interpret a query plan against the in-memory store and return
%% the combined rows. Result (List Row) Error.
mem_run_plan(Id, Plan) ->
    mem_call({run_plan, Id, Plan}).

%% mem_join/10 — inner-join LeftTable and RightTable on the condition tree Cond,
%% apply the two-row post-join WHERE tree Where2, keep the left rows matching the
%% predicate tree Pred, order by the column keys, drop duplicate pairs when Dist,
%% then page. Each result is the `{LeftRow, RightRow}` pair of column maps. Result
%% (List {Row, Row}) Error.
mem_join(Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Dist) ->
    mem_call({join, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Dist}).

%% mem_join_select/11 — as mem_join, then project each joined pair through the
%% projection tree Proj into one map keyed by the projection's aliases; Dist drops
%% duplicate projected rows (a `SELECT DISTINCT` over the projection). Result
%% (List Row) Error.
mem_join_select(Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist) ->
    mem_call({join_select, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist}).

%% mem_left_join/10 — left-outer-join LeftTable and RightTable on the condition
%% tree Cond, apply the two-row post-join WHERE tree Where2 (an unmatched left
%% row reads its right columns as NULL, so a Where2 over a right column drops it),
%% keep the left rows matching the predicate tree Pred, order, drop duplicate
%% pairs when Dist, then page. Each result is `{LeftRow, {some, RightRow}}` for a
%% match or `{LeftRow, none}` for a left row with no match. Result (List {Row,
%% Option Row}) Error.
mem_left_join(Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Dist) ->
    mem_call({left_join, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Dist}).

%% mem_left_join_select/11 — as mem_left_join, then project each kept row through
%% the projection tree Proj into one map keyed by the projection's aliases (an
%% unmatched left row's right-side columns project to SQL NULL); Dist drops
%% duplicate projected rows. Result (List Row) Error.
mem_left_join_select(Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist) ->
    mem_call({left_join_select, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist}).

%% mem_aggregate_join/9 — fold a scalar aggregate (Func is <<"SUM">>/<<"AVG">>/
%% <<"MIN">>/<<"MAX">>) over Column across the rows of the inner join of LeftTable
%% and RightTable. IsRight picks the column from the right row (true) or the left
%% row (false). The scalar comes back as `{some, SqlValue}`, or `none` over an
%% empty join (the in-memory dual of a SQL aggregate over no rows). Result (Option
%% SqlValue) Error.
mem_aggregate_join(Id, LeftTable, RightTable, Cond, Where2, Pred, Func, Column, IsRight) ->
    mem_call({aggregate_join, Id, LeftTable, RightTable, Cond, Where2, Pred, Func, Column, IsRight}).

%% mem_aggregate_left_join/9 — as mem_aggregate_join, but a left-outer join keeps
%% every left row. A left-column aggregate folds the unmatched left rows in; a
%% right-column one skips them (their right side is absent, a NULL the fold drops).
mem_aggregate_left_join(Id, LeftTable, RightTable, Cond, Where2, Pred, Func, Column, IsRight) ->
    mem_call({aggregate_left_join, Id, LeftTable, RightTable, Cond, Where2, Pred, Func, Column, IsRight}).

%% mem_count_join/6 — how many rows the inner join of LeftTable and RightTable
%% holds once Cond pairs them, Where2 narrows the pairs, and Pred filters the left
%% side. The join is neither ordered nor paged. Result Int Error.
mem_count_join(Id, LeftTable, RightTable, Cond, Where2, Pred) ->
    mem_call({count_join, Id, LeftTable, RightTable, Cond, Where2, Pred}).

%% mem_count_left_join/6 — as mem_count_join, but a left-outer join: every left row
%% Pred and Where2 admit is counted, an unmatched one (its right side absent)
%% included, so the count is the number of left-outer rows. Result Int Error.
mem_count_left_join(Id, LeftTable, RightTable, Cond, Where2, Pred) ->
    mem_call({count_left_join, Id, LeftTable, RightTable, Cond, Where2, Pred}).

%% mem_right_join/10 — right-outer-join LeftTable and RightTable: every right row is
%% kept, the left query's Pred folds into the join match so an unmatched right row
%% pairs with `none` for its left side. Each result is `{{some, LeftRow}, RightRow}`
%% for a match or `{none, RightRow}` for a right row with no match. Result (List
%% {Option Row, Row}) Error.
mem_right_join(Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Dist) ->
    mem_call({right_join, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Dist}).

%% mem_right_join_select/11 — as mem_right_join, then project each kept row through
%% the projection tree Proj (an unmatched right row's left-side columns project to
%% SQL NULL); Dist drops duplicate projected rows. Result (List Row) Error.
mem_right_join_select(Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist) ->
    mem_call({right_join_select, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist}).

%% mem_aggregate_right_join/9 — as mem_aggregate_left_join, but the right-outer
%% mirror: every right row is kept. A right-column aggregate folds the unmatched
%% right rows in; a left-column one skips them (their left side is absent, a NULL the
%% fold drops).
mem_aggregate_right_join(Id, LeftTable, RightTable, Cond, Where2, Pred, Func, Column, IsRight) ->
    mem_call({aggregate_right_join, Id, LeftTable, RightTable, Cond, Where2, Pred, Func, Column, IsRight}).

%% mem_count_right_join/6 — as mem_count_left_join, but a right-outer join: every
%% right row Pred (folded into the match) and Where2 admit is counted, an unmatched
%% one (its left side absent) included. Result Int Error.
mem_count_right_join(Id, LeftTable, RightTable, Cond, Where2, Pred) ->
    mem_call({count_right_join, Id, LeftTable, RightTable, Cond, Where2, Pred}).

%% mem_full_join/10 — full-outer-join LeftTable and RightTable: every row of both
%% tables is kept, the left query's Pred restricting which left rows enter the join.
%% Each result is `{{some, LeftRow}, {some, RightRow}}` for a match, `{{some, LeftRow},
%% none}` for a left row with no match, or `{none, {some, RightRow}}` for a right row
%% with no match. Result (List {Option Row, Option Row}) Error.
mem_full_join(Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Dist) ->
    mem_call({full_join, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Dist}).

%% mem_full_join_select/11 — as mem_full_join, then project each kept row through the
%% projection tree Proj (the columns of an unmatched side project to SQL NULL); Dist
%% drops duplicate projected rows. Result (List Row) Error.
mem_full_join_select(Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist) ->
    mem_call({full_join_select, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist}).

%% mem_aggregate_full_join/9 — as mem_aggregate_right_join, but the full-outer join: a
%% fold over either side skips the rows unmatched on that side (their column there is
%% absent, a NULL the fold drops).
mem_aggregate_full_join(Id, LeftTable, RightTable, Cond, Where2, Pred, Func, Column, IsRight) ->
    mem_call({aggregate_full_join, Id, LeftTable, RightTable, Cond, Where2, Pred, Func, Column, IsRight}).

%% mem_count_full_join/6 — as mem_count_right_join, but a full-outer join: every row of
%% both tables Pred and Where2 admit is counted. Result Int Error.
mem_count_full_join(Id, LeftTable, RightTable, Cond, Where2, Pred) ->
    mem_call({count_full_join, Id, LeftTable, RightTable, Cond, Where2, Pred}).

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

%% The slice of the keeper State belonging to store Id — its {Id, Table} entries.
%% A transaction snapshots and restores this slice, leaving other stores untouched.
mem_slice(Id, State) ->
    maps:filter(fun(K, _) -> mem_key_of(Id, K) end, State).

%% The transaction snapshot stack for store Id (newest first); [] when none open.
%% Kept in the keeper's own process dictionary, one stack per store, so nesting a
%% begin pushes another snapshot (a savepoint) without disturbing other stores.
mem_tx_stack(Id) ->
    case get({mem_tx, Id}) of
        undefined -> [];
        Stack     -> Stack
    end.

%% Whether map key K is a {Id, Table} entry of store Id (an atom bookkeeping key
%% never matches, so the snapshot/restore ignores non-table state).
mem_key_of(Id, {I, _Table}) -> I =:= Id;
mem_key_of(_Id, _K)         -> false.

mem_keeper_loop(State) ->
    receive
        {{begin_tx, Id}, From, Ref} ->
            put({mem_tx, Id}, [mem_slice(Id, State) | mem_tx_stack(Id)]),
            From ! {Ref, {ok, ok}},
            mem_keeper_loop(State);
        {{commit_tx, Id}, From, Ref} ->
            case mem_tx_stack(Id) of
                [_Top | Rest] -> put({mem_tx, Id}, Rest);
                _             -> ok
            end,
            From ! {Ref, {ok, ok}},
            mem_keeper_loop(State);
        {{rollback_tx, Id}, From, Ref} ->
            State1 =
                case mem_tx_stack(Id) of
                    [Snap | Rest] ->
                        put({mem_tx, Id}, Rest),
                        Without = maps:filter(fun(K, _) -> not mem_key_of(Id, K) end, State),
                        maps:merge(Without, Snap);
                    _ ->
                        State
                end,
            From ! {Ref, {ok, ok}},
            mem_keeper_loop(State1);
        {{close_store, Id}, From, Ref} ->
            %% Drop every table of store Id and any open transaction snapshot,
            %% leaving other stores untouched.
            erase({mem_tx, Id}),
            Without = maps:filter(fun(K, _) -> not mem_key_of(Id, K) end, State),
            From ! {Ref, {ok, ok}},
            mem_keeper_loop(Without);
        {{create_table, Id, Table}, From, Ref} ->
            Key = {Id, Table},
            From ! {Ref, {ok, ok}},
            mem_keeper_loop(State#{Key => maps:get(Key, State, [])});
        {{drop_table, Id, Table}, From, Ref} ->
            From ! {Ref, {ok, ok}},
            mem_keeper_loop(maps:remove({Id, Table}, State));
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
        {{update, Id, Table, Changes, Tree}, From, Ref} ->
            Key  = {Id, Table},
            Rows = maps:get(Key, State, []),
            {Updated, Changed} = mem_update_rows(Changes, Tree, Rows),
            From ! {Ref, {ok, Changed}},
            mem_keeper_loop(State#{Key => Updated});
        {{fetch, Id, Table, Tree, Orders, Lim, Off, Dist}, From, Ref} ->
            Rows = maps:get({Id, Table}, State, []),
            Matches = [R || R <- Rows, mem_pred(Tree, R)],
            Page = mem_paginate(mem_distinct(Dist, mem_order(Orders, Matches)), Lim, Off),
            From ! {Ref, {ok, Page}},
            mem_keeper_loop(State);
        {{count_where, Id, Table, Tree}, From, Ref} ->
            Rows = maps:get({Id, Table}, State, []),
            N = length([R || R <- Rows, mem_pred(Tree, R)]),
            From ! {Ref, {ok, N}},
            mem_keeper_loop(State);
        {{aggregate, Id, Table, Tree, Func, Column}, From, Ref} ->
            Rows = maps:get({Id, Table}, State, []),
            Matches = [R || R <- Rows, mem_pred(Tree, R)],
            Value = mem_aggregate_value(Func, Column, Matches),
            Wrapped = case Value of 'SqlNull' -> none; _ -> {some, Value} end,
            From ! {Ref, {ok, Wrapped}},
            mem_keeper_loop(State);
        {{group_summarize, Id, Table, Tree, KeyCol, Cols, Having}, From, Ref} ->
            Rows = maps:get({Id, Table}, State, []),
            Matches = [R || R <- Rows, mem_pred(Tree, R)],
            Groups = mem_group_by(KeyCol, Matches),
            Kept = [{K, GR} || {K, GR} <- Groups, mem_having(Having, K, GR)],
            Sorted = lists:sort(fun({KA, _}, {KB, _}) -> mem_order_cmp(KA, KB) =/= gt end, Kept),
            Result = [mem_group_row(Cols, K, GR) || {K, GR} <- Sorted],
            From ! {Ref, {ok, Result}},
            mem_keeper_loop(State);
        {{group_summarize_join, Id, LeftTable, RightTable, Cond, Where2, Pred, KeyCol, KeySide, Cols, Having}, From, Ref} ->
            Pairs = mem_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, []),
            Result = mem_group_join(Pairs, KeyCol, KeySide, Cols, Having),
            From ! {Ref, {ok, Result}},
            mem_keeper_loop(State);
        {{group_summarize_left_join, Id, LeftTable, RightTable, Cond, Where2, Pred, KeyCol, KeySide, Cols, Having}, From, Ref} ->
            Pairs0 = mem_left_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, []),
            Pairs = [{L, mem_right_row(OptR)} || {L, OptR} <- Pairs0],
            Result = mem_group_join(Pairs, KeyCol, KeySide, Cols, Having),
            From ! {Ref, {ok, Result}},
            mem_keeper_loop(State);
        {{run_plan, Id, Plan}, From, Ref} ->
            Rows = mem_eval_plan(State, Id, Plan),
            From ! {Ref, {ok, Rows}},
            mem_keeper_loop(State);
        {{project, Id, Table, Tree, Orders, Lim, Off, Cols, Dist}, From, Ref} ->
            Rows = maps:get({Id, Table}, State, []),
            Matches = [R || R <- Rows, mem_pred(Tree, R)],
            Ordered = mem_order(Orders, Matches),
            Projected = [mem_project_row(Cols, R) || R <- Ordered],
            Page = mem_paginate(mem_distinct(Dist, Projected), Lim, Off),
            From ! {Ref, {ok, Page}},
            mem_keeper_loop(State);
        {{join, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Dist}, From, Ref} ->
            Pairs = mem_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders),
            Page = mem_paginate(mem_distinct(Dist, Pairs), Lim, Off),
            From ! {Ref, {ok, Page}},
            mem_keeper_loop(State);
        {{join_select, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist}, From, Ref} ->
            Pairs = mem_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders),
            Projected = [mem_join_project(Proj, L, R) || {L, R} <- Pairs],
            Page = mem_paginate(mem_distinct(Dist, Projected), Lim, Off),
            From ! {Ref, {ok, Page}},
            mem_keeper_loop(State);
        {{left_join, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Dist}, From, Ref} ->
            Pairs = mem_left_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders),
            Page = mem_paginate(mem_distinct(Dist, Pairs), Lim, Off),
            From ! {Ref, {ok, Page}},
            mem_keeper_loop(State);
        {{left_join_select, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist}, From, Ref} ->
            Rows = mem_left_join_select_rows(State, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist),
            From ! {Ref, {ok, Rows}},
            mem_keeper_loop(State);
        {{aggregate_join, Id, LeftTable, RightTable, Cond, Where2, Pred, Func, Column, IsRight}, From, Ref} ->
            Pairs = mem_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, []),
            Rows = [mem_agg_side_row(IsRight, L, R) || {L, R} <- Pairs],
            Value = mem_aggregate_value(Func, Column, Rows),
            Wrapped = case Value of 'SqlNull' -> none; _ -> {some, Value} end,
            From ! {Ref, {ok, Wrapped}},
            mem_keeper_loop(State);
        {{aggregate_left_join, Id, LeftTable, RightTable, Cond, Where2, Pred, Func, Column, IsRight}, From, Ref} ->
            Pairs = mem_left_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, []),
            Rows = [mem_agg_side_row(IsRight, L, mem_right_row(OptR)) || {L, OptR} <- Pairs],
            Value = mem_aggregate_value(Func, Column, Rows),
            Wrapped = case Value of 'SqlNull' -> none; _ -> {some, Value} end,
            From ! {Ref, {ok, Wrapped}},
            mem_keeper_loop(State);
        {{count_join, Id, LeftTable, RightTable, Cond, Where2, Pred}, From, Ref} ->
            Pairs = mem_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, []),
            From ! {Ref, {ok, length(Pairs)}},
            mem_keeper_loop(State);
        {{count_left_join, Id, LeftTable, RightTable, Cond, Where2, Pred}, From, Ref} ->
            Pairs = mem_left_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, []),
            From ! {Ref, {ok, length(Pairs)}},
            mem_keeper_loop(State);
        {{right_join, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Dist}, From, Ref} ->
            Pairs = mem_right_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders),
            Page = mem_paginate(mem_distinct(Dist, Pairs), Lim, Off),
            From ! {Ref, {ok, Page}},
            mem_keeper_loop(State);
        {{right_join_select, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist}, From, Ref} ->
            Rows = mem_right_join_select_rows(State, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist),
            From ! {Ref, {ok, Rows}},
            mem_keeper_loop(State);
        {{aggregate_right_join, Id, LeftTable, RightTable, Cond, Where2, Pred, Func, Column, IsRight}, From, Ref} ->
            Pairs = mem_right_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, []),
            Rows = [mem_agg_side_row(IsRight, mem_left_row(OptL), R) || {OptL, R} <- Pairs],
            Value = mem_aggregate_value(Func, Column, Rows),
            Wrapped = case Value of 'SqlNull' -> none; _ -> {some, Value} end,
            From ! {Ref, {ok, Wrapped}},
            mem_keeper_loop(State);
        {{count_right_join, Id, LeftTable, RightTable, Cond, Where2, Pred}, From, Ref} ->
            Pairs = mem_right_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, []),
            From ! {Ref, {ok, length(Pairs)}},
            mem_keeper_loop(State);
        {{group_summarize_right_join, Id, LeftTable, RightTable, Cond, Where2, Pred, KeyCol, KeySide, Cols, Having}, From, Ref} ->
            Pairs0 = mem_right_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, []),
            Pairs = [{mem_left_row(OptL), R} || {OptL, R} <- Pairs0],
            Result = mem_group_join(Pairs, KeyCol, KeySide, Cols, Having),
            From ! {Ref, {ok, Result}},
            mem_keeper_loop(State);
        {{full_join, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Dist}, From, Ref} ->
            Pairs = mem_full_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders),
            Page = mem_paginate(mem_distinct(Dist, Pairs), Lim, Off),
            From ! {Ref, {ok, Page}},
            mem_keeper_loop(State);
        {{full_join_select, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist}, From, Ref} ->
            Rows = mem_full_join_select_rows(State, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist),
            From ! {Ref, {ok, Rows}},
            mem_keeper_loop(State);
        {{aggregate_full_join, Id, LeftTable, RightTable, Cond, Where2, Pred, Func, Column, IsRight}, From, Ref} ->
            Pairs = mem_full_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, []),
            Rows = [mem_agg_side_row(IsRight, mem_left_row(OptL), mem_right_row(OptR)) || {OptL, OptR} <- Pairs],
            Value = mem_aggregate_value(Func, Column, Rows),
            Wrapped = case Value of 'SqlNull' -> none; _ -> {some, Value} end,
            From ! {Ref, {ok, Wrapped}},
            mem_keeper_loop(State);
        {{count_full_join, Id, LeftTable, RightTable, Cond, Where2, Pred}, From, Ref} ->
            Pairs = mem_full_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, []),
            From ! {Ref, {ok, length(Pairs)}},
            mem_keeper_loop(State);
        {{group_summarize_full_join, Id, LeftTable, RightTable, Cond, Where2, Pred, KeyCol, KeySide, Cols, Having}, From, Ref} ->
            Pairs0 = mem_full_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, []),
            Pairs = [{mem_left_row(OptL), mem_right_row(OptR)} || {OptL, OptR} <- Pairs0],
            Result = mem_group_join(Pairs, KeyCol, KeySide, Cols, Having),
            From ! {Ref, {ok, Result}},
            mem_keeper_loop(State)
    end.

%% The row a join aggregate folds its column from: the right row when IsRight,
%% otherwise the left. For a left join the right row is normalised through
%% `mem_right_row` first (an unmatched left row's `none` becomes `#{}`, so its
%% column reads SqlNull and the fold skips it).
mem_agg_side_row(true,  _L, R) -> R;
mem_agg_side_row(false, L, _R) -> L.

%% Build a projected row from `{Alias, Column}` pairs: each output column reads
%% the source column from the row and is keyed by its alias. A missing source
%% column reads as SQL NULL.
mem_project_row(Cols, Row) ->
    maps:from_list([{Alias, maps:get(Col, Row, 'SqlNull')} || {Alias, Col} <- Cols]).

%% Drop duplicate rows, keeping the first occurrence of each so the result stays
%% deterministic. Rows compare by full term equality, the same notion of equality
%% a `SELECT DISTINCT` applies over the selected columns.
mem_distinct(false, Rows) -> Rows;
mem_distinct(true, Rows) ->
    {Out, _} = lists:foldl(
        fun(R, {Acc, Seen}) ->
            case lists:member(R, Seen) of
                true  -> {Acc, Seen};
                false -> {[R | Acc], [R | Seen]}
            end
        end, {[], []}, Rows),
    lists:reverse(Out).

%% Evaluate a query plan against the store snapshot State, returning its rows. A
%% scan reads one table (filter, order, dedup, page); a combine evaluates both
%% branches and applies the set operation; a refine applies an outer filter, order,
%% dedup, and page on a plan's result. Nested combines recurse.
mem_eval_plan(State, Id, {'PlanScan', Table, Pred, Orders, Lim, Off, Dist}) ->
    Rows = maps:get({Id, Table}, State, []),
    Matches = [R || R <- Rows, mem_pred(Pred, R)],
    mem_paginate(mem_distinct(Dist, mem_order(Orders, Matches)), Lim, Off);
mem_eval_plan(State, Id, {'PlanCombine', Op, Left, Right}) ->
    L = mem_eval_plan(State, Id, Left),
    R = mem_eval_plan(State, Id, Right),
    mem_set_op(Op, L, R);
mem_eval_plan(State, Id, {'PlanRefine', Inner, Pred, Orders, Lim, Off, Dist}) ->
    Rows = mem_eval_plan(State, Id, Inner),
    Matches = [R || R <- Rows, mem_pred(Pred, R)],
    mem_paginate(mem_distinct(Dist, mem_order(Orders, Matches)), Lim, Off);
mem_eval_plan(State, Id, {'PlanJoin', <<"INNER">>, Left, Right, Cond, Where2, Orders, Lim, Off, Dist}) ->
    LeftRows = mem_eval_plan(State, Id, Left),
    RightRows = mem_eval_plan(State, Id, Right),
    Pairs = [{L, R} || L <- LeftRows, R <- RightRows,
                       mem_jpred(Cond, L, R), mem_jpred(Where2, L, R)],
    Flat = [mem_prefix_pair(L, R) || {L, R} <- mem_order_pairs(Orders, Pairs)],
    mem_paginate(mem_distinct(Dist, Flat), Lim, Off);
mem_eval_plan(State, Id, {'PlanProject', Proj, Child, Lim, Off, Dist}) ->
    Rows = mem_eval_plan(State, Id, Child),
    Projected = [mem_project_prefixed(Proj, Row) || Row <- Rows],
    mem_paginate(mem_distinct(Dist, Projected), Lim, Off);
mem_eval_plan(State, Id, {'PlanAggregate', <<"COUNT">>, _Column, _IsRight, Child}) ->
    Rows = mem_eval_plan(State, Id, Child),
    [#{<<"agg">> => {'SqlInt', length(Rows)}}];
mem_eval_plan(State, Id, {'PlanAggregate', Func, Column, IsRight, Child}) ->
    Rows = mem_eval_plan(State, Id, Child),
    case mem_aggregate_value(Func, mem_agg_prefixed_col(IsRight, Column), Rows) of
        'SqlNull' -> [];
        Value     -> [#{<<"agg">> => Value}]
    end.

%% The prefixed column name a join aggregate folds: the right source's column (the
%% t1$ prefix) when IsRight, otherwise the left source's (t0$). A `PlanAggregate`
%% over a join folds its child's flat source-prefixed rows, so the column it reads
%% carries the side's prefix, mirroring how `mem_pcell` resolves a projection cell.
mem_agg_prefixed_col(true,  Column) -> <<"t1$", Column/binary>>;
mem_agg_prefixed_col(false, Column) -> <<"t0$", Column/binary>>.

%% Project a flat, source-prefixed join row through a projection tree into one row
%% keyed by the projection's output aliases. A `QCol` names a left-source column
%% (the t0$ prefix the join flattened the left side under), a `QColR` a right-source
%% column (t1$); a missing column reads SQL NULL. The prefixed dual of
%% `mem_join_project`, which reads the unprefixed {LeftMap, RightMap} pair directly.
mem_project_prefixed({'QProj', Cols}, Row) ->
    maps:from_list([{Alias, mem_pcell(Col, Row)} || {Alias, Col} <- Cols]);
mem_project_prefixed(_Other, _Row) ->
    #{}.

mem_pcell({'QCol', C}, Row)  -> maps:get(<<"t0$", C/binary>>, Row, 'SqlNull');
mem_pcell({'QColR', C}, Row) -> maps:get(<<"t1$", C/binary>>, Row, 'SqlNull');
mem_pcell(_Other, _Row)      -> 'SqlNull'.

%% Flatten a joined {LeftMap, RightMap} pair into one row map with each side's columns
%% prefixed (t0$ for the left source, t1$ for the right) so the two sides never
%% collide on a shared column name. The Ridge decoder strips the prefix per side.
mem_prefix_pair(L, R) ->
    maps:merge(mem_prefix_keys(<<"t0$">>, L), mem_prefix_keys(<<"t1$">>, R)).

mem_prefix_keys(Prefix, M) ->
    maps:fold(fun(K, V, Acc) -> Acc#{<<Prefix/binary, K/binary>> => V} end, #{}, M).

%% Apply a set operation to two row lists. UNION, INTERSECT, and EXCEPT de-duplicate
%% (set semantics); UNION ALL keeps every row. Rows compare by full term equality,
%% the same notion mem_distinct uses.
mem_set_op(<<"UNION">>, L, R)     -> mem_distinct(true, L ++ R);
mem_set_op(<<"UNION ALL">>, L, R) -> L ++ R;
mem_set_op(<<"INTERSECT">>, L, R) -> mem_distinct(true, [X || X <- L, lists:member(X, R)]);
mem_set_op(<<"EXCEPT">>, L, R)    -> mem_distinct(true, [X || X <- L, not lists:member(X, R)]);
mem_set_op(_Op, L, _R)            -> L.

%% Fold a scalar aggregate over Column across Rows, returning a SqlValue. NULLs (a
%% missing column or an explicit SqlNull) are skipped, as in SQL; an empty set —
%% no rows, or every value NULL — is SqlNull. SUM keeps the column's numeric type
%% (an all-integer sum stays an integer, a float anywhere makes it a float); AVG
%% is always a float; MIN/MAX keep the values' own type, comparing numbers
%% numerically and text lexicographically.
mem_aggregate_value(Func, Column, Rows) ->
    Values = [V || R <- Rows, V <- [maps:get(Column, R, 'SqlNull')], V =/= 'SqlNull'],
    mem_agg(Func, Values).

mem_agg(_Func, [])         -> 'SqlNull';
mem_agg(<<"SUM">>, Values) -> mem_sum(Values);
mem_agg(<<"AVG">>, Values) -> {'SqlFloat', mem_numsum(Values) / length(Values)};
mem_agg(<<"MIN">>, Values) -> mem_extreme(min, Values);
mem_agg(<<"MAX">>, Values) -> mem_extreme(max, Values);
mem_agg(_Other, _Values)   -> 'SqlNull'.

%% SUM stays an integer while every addend is one and becomes a float as soon as
%% any value is a float, mirroring Postgres where SUM(int) is integral and
%% SUM(float8) is floating.
mem_sum(Values) ->
    Sum = mem_numsum(Values),
    case lists:any(fun is_float_val/1, Values) of
        true  -> {'SqlFloat', float(Sum)};
        false -> {'SqlInt', Sum}
    end.

mem_numsum(Values) ->
    lists:foldl(fun(V, Acc) -> Acc + mem_num(V) end, 0, Values).

mem_num({'SqlInt', N})   -> N;
mem_num({'SqlFloat', F}) -> F.

is_float_val({'SqlFloat', _}) -> true;
is_float_val(_)               -> false.

%% MIN/MAX by direction over the values' comparison keys, keeping the original
%% SqlValue so the result carries the column's type. Numbers compare numerically,
%% text lexicographically.
mem_extreme(Dir, [V | Rest]) -> mem_extreme(Dir, Rest, V).

mem_extreme(_Dir, [], Best) -> Best;
mem_extreme(Dir, [V | Rest], Best) ->
    case mem_better(Dir, mem_key(V), mem_key(Best)) of
        true  -> mem_extreme(Dir, Rest, V);
        false -> mem_extreme(Dir, Rest, Best)
    end.

mem_better(min, A, B) -> A < B;
mem_better(max, A, B) -> A > B.

mem_key({'SqlInt', N})   -> N;
mem_key({'SqlFloat', F}) -> F;
mem_key({'SqlText', S})  -> S;
mem_key({'SqlBool', B})  -> B.

%% --- In-memory GROUP BY / HAVING ---
%%
%% The nested-loop dual of a backend pushing GROUP BY into SQL. Partition the
%% matching rows by the key column's value, summarize each group into its
%% aggregates, drop the groups the HAVING tree rejects, and key each output row by
%% the projection's aliases. The keeper sorts the surviving groups by key so the
%% result is deterministic, matching the `ORDER BY <key>` the SQL backend appends.

%% Partition rows by the key column's value, preserving first-seen key order.
mem_group_by(KeyCol, Rows) ->
    lists:foldl(
        fun(R, Acc) ->
            K = maps:get(KeyCol, R, 'SqlNull'),
            case lists:keyfind(K, 1, Acc) of
                {K, GR} -> lists:keyreplace(K, 1, Acc, {K, GR ++ [R]});
                false   -> Acc ++ [{K, [R]}]
            end
        end,
        [],
        Rows).

%% Build one output row for a group: each `{Alias, Func, Column, IsRight}` becomes
%% `Alias => value`, where the value is the group key, its row count, or a scalar
%% aggregate over the group's rows. `IsRight` tags a join column's side and is unused
%% for a single-table group (every column is the left side).
mem_group_row(Cols, Key, GroupRows) ->
    maps:from_list([{Alias, mem_group_value(Func, Column, Key, GroupRows)}
                    || {Alias, Func, Column, _IsRight} <- Cols]).

mem_group_value(<<"KEY">>, _Col, Key, _GR)   -> Key;
mem_group_value(<<"COUNT">>, _Col, _Key, GR) -> {'SqlInt', length(GR)};
mem_group_value(Func, Col, _Key, GR)         -> mem_aggregate_value(Func, Col, GR).

%% Evaluate a HAVING predicate tree over one group (its key and rows). The leaves
%% are aggregate nodes — `QGroupKey`, `QAggCount`, `QAgg{Sum,Avg,Min,Max}` — that
%% reduce the group; comparisons and connectives combine them. The always-true
%% tree (the `keepAll` default) keeps every group.
mem_having({'QLitBool', true}, _Key, _GR) -> true;
mem_having({'QAnd', L, R}, Key, GR) -> mem_having(L, Key, GR) andalso mem_having(R, Key, GR);
mem_having({'QOr', L, R}, Key, GR)  -> mem_having(L, Key, GR) orelse mem_having(R, Key, GR);
mem_having({'QNot', X}, Key, GR)    -> not mem_having(X, Key, GR);
mem_having({'QEq', L, R}, Key, GR)  -> mem_hrelate(eq, L, R, Key, GR);
mem_having({'QNe', L, R}, Key, GR)  -> not mem_hrelate(eq, L, R, Key, GR);
mem_having({'QLt', L, R}, Key, GR)  -> mem_hrelate(lt, L, R, Key, GR);
mem_having({'QGt', L, R}, Key, GR)  -> mem_hrelate(lt, R, L, Key, GR);
mem_having({'QLe', L, R}, Key, GR)  -> not mem_hrelate(lt, R, L, Key, GR);
mem_having({'QGe', L, R}, Key, GR)  -> not mem_hrelate(lt, L, R, Key, GR);
mem_having(_Other, _Key, _GR)       -> true.

mem_hrelate(Op, L, R, Key, GR) ->
    case {mem_hscalar(L, Key, GR), mem_hscalar(R, Key, GR)} of
        {undefined, _} -> false;
        {_, undefined} -> false;
        {A, B}         -> mem_sql_cmp(Op, A, B)
    end.

%% Resolve a HAVING operand to a SqlValue: an aggregate over the group, the group
%% key, or a literal. A nullary aggregate node (`QGroupKey`, `QAggCount`) arrives
%% as a bare atom; the scalar aggregates wrap their `QCol`.
mem_hscalar('QGroupKey', Key, _GR) -> Key;
mem_hscalar('QAggCount', _Key, GR) -> {'SqlInt', length(GR)};
mem_hscalar({'QAggSum', {'QCol', C}}, _Key, GR) -> mem_agg_or_undef(<<"SUM">>, C, GR);
mem_hscalar({'QAggAvg', {'QCol', C}}, _Key, GR) -> mem_agg_or_undef(<<"AVG">>, C, GR);
mem_hscalar({'QAggMin', {'QCol', C}}, _Key, GR) -> mem_agg_or_undef(<<"MIN">>, C, GR);
mem_hscalar({'QAggMax', {'QCol', C}}, _Key, GR) -> mem_agg_or_undef(<<"MAX">>, C, GR);
mem_hscalar({'QLitInt', N}, _Key, _GR)   -> {'SqlInt', N};
mem_hscalar({'QLitText', S}, _Key, _GR)  -> {'SqlText', S};
mem_hscalar({'QLitBool', B}, _Key, _GR)  -> {'SqlBool', B};
mem_hscalar({'QLitFloat', F}, _Key, _GR) -> {'SqlFloat', F};
mem_hscalar(_Other, _Key, _GR)           -> undefined.

%% An aggregate over a group as a comparison operand: an all-NULL fold has no
%% value, so the comparison fails rather than crashing.
mem_agg_or_undef(Func, Col, GR) ->
    case mem_aggregate_value(Func, Col, GR) of
        'SqlNull' -> undefined;
        V         -> V
    end.

%% --- In-memory grouped join ---
%%
%% Group a join's `{L,R}` pairs by a key column read off one side, narrow the groups
%% by a HAVING tree over the group aggregates, and summarise each surviving group.
%% A grouped aggregate folds values rather than producing one per row, so it reads a
%% right column as the plain right row (an unmatched left join row's right side is
%% `#{}`, whose columns read SqlNull and so drop out of the fold), exactly as the
%% join scalar aggregates do.
mem_group_join(Pairs, KeyCol, KeySide, Cols, Having) ->
    Groups = mem_group_pairs(KeyCol, KeySide, Pairs),
    Kept = [{K, GP} || {K, GP} <- Groups, mem_having_join(Having, K, GP)],
    Sorted = lists:sort(fun({KA, _}, {KB, _}) -> mem_order_cmp(KA, KB) =/= gt end, Kept),
    [mem_group_join_row(Cols, K, GP) || {K, GP} <- Sorted].

%% Partition the pairs by the key value, read from the right side when KeySide is
%% true (a join grouped by a right column) and the left otherwise. First-seen order.
mem_group_pairs(KeyCol, KeySide, Pairs) ->
    lists:foldl(
        fun({L, R}, Acc) ->
            K = mem_pair_key(KeySide, KeyCol, L, R),
            case lists:keyfind(K, 1, Acc) of
                {K, GP} -> lists:keyreplace(K, 1, Acc, {K, GP ++ [{L, R}]});
                false   -> Acc ++ [{K, [{L, R}]}]
            end
        end,
        [],
        Pairs).

mem_pair_key(true,  KeyCol, _L, R) -> maps:get(KeyCol, R, 'SqlNull');
mem_pair_key(false, KeyCol, L, _R) -> maps:get(KeyCol, L, 'SqlNull').

%% One output row per join group: each `{Alias, Func, Column, IsRight}` folds the
%% column from its side, COUNT counts the pairs, KEY answers the group key.
mem_group_join_row(Cols, Key, GP) ->
    maps:from_list([{Alias, mem_group_join_value(Func, Column, IsRight, Key, GP)}
                    || {Alias, Func, Column, IsRight} <- Cols]).

mem_group_join_value(<<"KEY">>, _Col, _IsRight, Key, _GP)   -> Key;
mem_group_join_value(<<"COUNT">>, _Col, _IsRight, _Key, GP) -> {'SqlInt', length(GP)};
mem_group_join_value(Func, Col, IsRight, _Key, GP) ->
    Rows = [mem_agg_side_row(IsRight, L, R) || {L, R} <- GP],
    mem_aggregate_value(Func, Col, Rows).

%% HAVING over a join group: as mem_having, but its scalar-aggregate leaves fold a
%% left (`QCol`) or right (`QColR`) column off the `{L,R}` pairs.
mem_having_join({'QLitBool', true}, _Key, _GP) -> true;
mem_having_join({'QAnd', L, R}, Key, GP) -> mem_having_join(L, Key, GP) andalso mem_having_join(R, Key, GP);
mem_having_join({'QOr', L, R}, Key, GP)  -> mem_having_join(L, Key, GP) orelse mem_having_join(R, Key, GP);
mem_having_join({'QNot', X}, Key, GP)    -> not mem_having_join(X, Key, GP);
mem_having_join({'QEq', L, R}, Key, GP)  -> mem_hrelate_join(eq, L, R, Key, GP);
mem_having_join({'QNe', L, R}, Key, GP)  -> not mem_hrelate_join(eq, L, R, Key, GP);
mem_having_join({'QLt', L, R}, Key, GP)  -> mem_hrelate_join(lt, L, R, Key, GP);
mem_having_join({'QGt', L, R}, Key, GP)  -> mem_hrelate_join(lt, R, L, Key, GP);
mem_having_join({'QLe', L, R}, Key, GP)  -> not mem_hrelate_join(lt, R, L, Key, GP);
mem_having_join({'QGe', L, R}, Key, GP)  -> not mem_hrelate_join(lt, L, R, Key, GP);
mem_having_join(_Other, _Key, _GP)       -> true.

mem_hrelate_join(Op, L, R, Key, GP) ->
    case {mem_hscalar_join(L, Key, GP), mem_hscalar_join(R, Key, GP)} of
        {undefined, _} -> false;
        {_, undefined} -> false;
        {A, B}         -> mem_sql_cmp(Op, A, B)
    end.

mem_hscalar_join('QGroupKey', Key, _GP) -> Key;
mem_hscalar_join('QAggCount', _Key, GP) -> {'SqlInt', length(GP)};
mem_hscalar_join({'QAggSum', Node}, _Key, GP) -> mem_agg_node_join(<<"SUM">>, Node, GP);
mem_hscalar_join({'QAggAvg', Node}, _Key, GP) -> mem_agg_node_join(<<"AVG">>, Node, GP);
mem_hscalar_join({'QAggMin', Node}, _Key, GP) -> mem_agg_node_join(<<"MIN">>, Node, GP);
mem_hscalar_join({'QAggMax', Node}, _Key, GP) -> mem_agg_node_join(<<"MAX">>, Node, GP);
mem_hscalar_join({'QLitInt', N}, _Key, _GP)   -> {'SqlInt', N};
mem_hscalar_join({'QLitText', S}, _Key, _GP)  -> {'SqlText', S};
mem_hscalar_join({'QLitBool', B}, _Key, _GP)  -> {'SqlBool', B};
mem_hscalar_join({'QLitFloat', F}, _Key, _GP) -> {'SqlFloat', F};
mem_hscalar_join(_Other, _Key, _GP)           -> undefined.

%% A scalar aggregate over a join group's left (`QCol`) or right (`QColR`) column.
mem_agg_node_join(Func, {'QCol', C}, GP)  -> mem_agg_or_undef(Func, C, [L || {L, _R} <- GP]);
mem_agg_node_join(Func, {'QColR', C}, GP) -> mem_agg_or_undef(Func, C, [R || {_L, R} <- GP]);
mem_agg_node_join(_Func, _Node, _GP)      -> undefined.

%% Merge the Changes columns into every row matching the predicate tree, leaving
%% the rest untouched; return `{UpdatedRows, ChangedCount}`. An empty Changes map
%% is a no-op — nothing changes and the count is zero — matching the SQL backend,
%% which cannot emit an empty SET.
mem_update_rows(Changes, _Tree, Rows) when map_size(Changes) =:= 0 ->
    {Rows, 0};
mem_update_rows(Changes, Tree, Rows) ->
    lists:mapfoldl(
        fun(R, Count) ->
            case mem_pred(Tree, R) of
                true  -> {maps:merge(R, Changes), Count + 1};
                false -> {R, Count}
            end
        end,
        0,
        Rows).

%% --- In-memory inner join ---
%%
%% The nested-loop dual of a backend pushing a JOIN into SQL. Keep the left rows
%% the left-side predicate matches, pair each with every right row the condition
%% accepts and the two-row post-join WHERE keeps, order the pairs by the
%% left-column keys, then page. Both trees are QExprs over both rows: a `QCol`
%% reads the left row, a `QColR` the right. `Where2` is always-true until a join
%% `filter` narrows it.

mem_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders) ->
    LeftRows = maps:get({Id, LeftTable}, State, []),
    RightRows = maps:get({Id, RightTable}, State, []),
    LeftMatches = [L || L <- LeftRows, mem_pred(Pred, L)],
    Pairs = [{L, R} || L <- LeftMatches, R <- RightRows,
                       mem_jpred(Cond, L, R), mem_jpred(Where2, L, R)],
    mem_order_pairs(Orders, Pairs).

%% --- In-memory left-outer join ---
%%
%% As mem_join_pairs, but a left row with no matching right row is kept: it pairs
%% with `none` instead of being dropped, and a row with matches pairs with each
%% as `{some, RightRow}`. Ordering and paging act on the left row of each pair
%% just as for the inner join.

mem_left_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders) ->
    LeftRows = maps:get({Id, LeftTable}, State, []),
    RightRows = maps:get({Id, RightTable}, State, []),
    LeftMatches = [L || L <- LeftRows, mem_pred(Pred, L)],
    Pairs = lists:append([mem_left_pairs_for(L, RightRows, Cond, Where2) || L <- LeftMatches]),
    mem_order_pairs(Orders, Pairs).

%% The pairs a single left row contributes under `LEFT JOIN … ON Cond WHERE
%% Where2`. A left row with condition-matching right rows yields one
%% `{L, {some, R}}` per match the post-join Where2 also keeps; if every match
%% fails Where2 the left row drops out entirely (it joined, so there is no NULL
%% row). A left row with no condition match yields the single `{L, none}` row,
%% kept only when Where2 holds with the right side read as NULL (the empty map) —
%% so a Where2 over a right column drops the unmatched rows, mirroring SQL's
%% three-valued `WHERE` after a left join.
mem_left_pairs_for(L, RightRows, Cond, Where2) ->
    case [R || R <- RightRows, mem_jpred(Cond, L, R)] of
        []      ->
            case mem_jpred(Where2, L, #{}) of
                true  -> [{L, none}];
                false -> []
            end;
        Matches -> [{L, {some, R}} || R <- Matches, mem_jpred(Where2, L, R)]
    end.

%% --- In-memory left-outer join projection ---
%%
%% As mem_left_join_pairs, but each kept row is projected through Proj. An
%% unmatched left row pairs with the empty right map, so the projection's
%% `QColR` columns read SQL NULL — the dual of a `LEFT JOIN` returning NULL for
%% the right side, which decodes to `None` in the projected shape's `Option`
%% fields.
mem_left_join_select_rows(State, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist) ->
    LeftRows = maps:get({Id, LeftTable}, State, []),
    RightRows = maps:get({Id, RightTable}, State, []),
    LeftMatches = [L || L <- LeftRows, mem_pred(Pred, L)],
    Pairs = lists:append([mem_left_select_pairs(L, RightRows, Cond, Where2) || L <- LeftMatches]),
    Ordered = mem_order_pairs(Orders, Pairs),
    Projected = [mem_join_project(Proj, L, R) || {L, R} <- Ordered],
    mem_paginate(mem_distinct(Dist, Projected), Lim, Off).

%% The pairs a single left row contributes for a projection, under the same
%% `ON Cond WHERE Where2` rule as `mem_left_pairs_for`: one `{L, R}` per
%% condition match the post-join Where2 keeps; or `[{L, #{}}]` (the empty right
%% map, so the right columns project to SQL NULL) when no right row matches the
%% condition and Where2 holds with the right side NULL; otherwise nothing.
mem_left_select_pairs(L, RightRows, Cond, Where2) ->
    case [R || R <- RightRows, mem_jpred(Cond, L, R)] of
        []      ->
            case mem_jpred(Where2, L, #{}) of
                true  -> [{L, #{}}];
                false -> []
            end;
        Matches -> [{L, R} || R <- Matches, mem_jpred(Where2, L, R)]
    end.

%% --- In-memory right-outer join ---
%%
%% The mirror of mem_left_join_pairs with the preserved side flipped to the right
%% table: every right row is kept, and the left query's Pred folds into the match
%% (so an unmatched right row keeps a `none` left side rather than being dropped, the
%% way Pred in the post-join WHERE would). Each pair is `{OptLeft, RightRow}` — the
%% left side wrapped `{some, L}` for a match or `none` for an unmatched right row.

mem_right_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders) ->
    LeftRows = maps:get({Id, LeftTable}, State, []),
    RightRows = maps:get({Id, RightTable}, State, []),
    LeftMatches = [L || L <- LeftRows, mem_pred(Pred, L)],
    Pairs = lists:append([mem_right_pairs_for(R, LeftMatches, Cond, Where2) || R <- RightRows]),
    mem_order_pairs(Orders, Pairs).

%% The pairs a single right row contributes under `… RIGHT JOIN R ON Cond AND Pred
%% WHERE Where2`. A right row with condition-matching left rows (already narrowed by
%% Pred) yields one `{{some, L}, R}` per match the post-join Where2 also keeps; a
%% right row with no match yields the single `{none, R}` row, kept when Where2 holds
%% with the left side read as NULL (the empty map) — so a Where2 over a left column
%% drops the unmatched rows, mirroring SQL's three-valued WHERE after a right join.
mem_right_pairs_for(R, LeftMatches, Cond, Where2) ->
    case [L || L <- LeftMatches, mem_jpred(Cond, L, R)] of
        []      ->
            case mem_jpred(Where2, #{}, R) of
                true  -> [{none, R}];
                false -> []
            end;
        Matches -> [{{some, L}, R} || L <- Matches, mem_jpred(Where2, L, R)]
    end.

%% --- In-memory right-outer join projection ---
%%
%% As mem_right_join_select_rows, but each kept row is projected through Proj. An
%% unmatched right row pairs with the empty left map, so the projection's `QCol`
%% columns read SQL NULL — the dual of a `RIGHT JOIN` returning NULL for the left
%% side, which decodes to `None` in the projected shape's `Option` fields.
mem_right_join_select_rows(State, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist) ->
    LeftRows = maps:get({Id, LeftTable}, State, []),
    RightRows = maps:get({Id, RightTable}, State, []),
    LeftMatches = [L || L <- LeftRows, mem_pred(Pred, L)],
    Pairs = lists:append([mem_right_select_pairs(R, LeftMatches, Cond, Where2) || R <- RightRows]),
    Ordered = mem_order_pairs(Orders, Pairs),
    Projected = [mem_join_project(Proj, L, R) || {L, R} <- Ordered],
    mem_paginate(mem_distinct(Dist, Projected), Lim, Off).

%% The pairs a single right row contributes for a projection, under the same
%% `ON Cond AND Pred WHERE Where2` rule as `mem_right_pairs_for`: one `{L, R}` per
%% match the post-join Where2 keeps; or `[{#{}, R}]` (the empty left map, so the
%% left columns project to SQL NULL) when no left row matches and Where2 holds with
%% the left side NULL; otherwise nothing.
mem_right_select_pairs(R, LeftMatches, Cond, Where2) ->
    case [L || L <- LeftMatches, mem_jpred(Cond, L, R)] of
        []      ->
            case mem_jpred(Where2, #{}, R) of
                true  -> [{#{}, R}];
                false -> []
            end;
        Matches -> [{L, R} || L <- Matches, mem_jpred(Where2, L, R)]
    end.

%% --- In-memory full-outer join ---
%%
%% The union of mem_left_join_pairs and mem_right_join_pairs: every row of both tables
%% is kept. The left query's Pred restricts which left rows enter the join — a left row
%% it rejects never appears, not even unmatched. (A right join can fold Pred into the
%% ON because its unmatched left rows are dropped anyway; a full join keeps them, so
%% Pred must filter the left input instead.) The matched and left-only rows come from
%% the left walk; the right-only rows (a right row matching no surviving left row) come
%% from the right walk, so neither is counted twice. Each pair is `{OptLeft, OptRight}`.
mem_full_join_pairs(State, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders) ->
    LeftRows = maps:get({Id, LeftTable}, State, []),
    RightRows = maps:get({Id, RightTable}, State, []),
    LeftMatches = [L || L <- LeftRows, mem_pred(Pred, L)],
    LeftSide = lists:append([mem_full_left_pairs_for(L, RightRows, Cond, Where2) || L <- LeftMatches]),
    RightOnly = lists:append([mem_full_right_only_for(R, LeftMatches, Cond, Where2) || R <- RightRows]),
    mem_order_pairs(Orders, LeftSide ++ RightOnly).

%% The matched and left-only pairs a single (surviving) left row contributes — the dual
%% of mem_left_pairs_for with the right side wrapped: one `{{some, L}, {some, R}}` per
%% condition match the post-join Where2 also keeps; or the single `{{some, L}, none}`
%% (kept when Where2 holds with the right side read as NULL) when no right row matches.
mem_full_left_pairs_for(L, RightRows, Cond, Where2) ->
    case [R || R <- RightRows, mem_jpred(Cond, L, R)] of
        []      ->
            case mem_jpred(Where2, L, #{}) of
                true  -> [{{some, L}, none}];
                false -> []
            end;
        Matches -> [{{some, L}, {some, R}} || R <- Matches, mem_jpred(Where2, L, R)]
    end.

%% The right-only pair a single right row contributes: `{none, {some, R}}` when no
%% surviving left row matches the condition (and Where2 holds with the left side read
%% as NULL). A right row that DID match a left row is already emitted by the left walk,
%% so it contributes nothing here — that keeps a matched row from being counted twice.
mem_full_right_only_for(R, LeftMatches, Cond, Where2) ->
    case [L || L <- LeftMatches, mem_jpred(Cond, L, R)] of
        []      ->
            case mem_jpred(Where2, #{}, R) of
                true  -> [{none, {some, R}}];
                false -> []
            end;
        _Matches -> []
    end.

%% --- In-memory full-outer join projection ---
%%
%% As mem_full_join_pairs, but each kept row is projected through Proj. The columns of
%% an unmatched side read the empty map (SQL NULL), decoding to `None` in the projected
%% shape's `Option` fields.
mem_full_join_select_rows(State, Id, LeftTable, RightTable, Cond, Where2, Pred, Orders, Lim, Off, Proj, Dist) ->
    LeftRows = maps:get({Id, LeftTable}, State, []),
    RightRows = maps:get({Id, RightTable}, State, []),
    LeftMatches = [L || L <- LeftRows, mem_pred(Pred, L)],
    LeftSide = lists:append([mem_full_left_select_pairs(L, RightRows, Cond, Where2) || L <- LeftMatches]),
    RightOnly = lists:append([mem_full_right_only_select_pairs(R, LeftMatches, Cond, Where2) || R <- RightRows]),
    Ordered = mem_order_pairs(Orders, LeftSide ++ RightOnly),
    Projected = [mem_join_project(Proj, L, R) || {L, R} <- Ordered],
    mem_paginate(mem_distinct(Dist, Projected), Lim, Off).

%% The matched and left-only `{L, R}` map pairs a single left row contributes for a
%% projection (the empty right map where the left row matched none).
mem_full_left_select_pairs(L, RightRows, Cond, Where2) ->
    case [R || R <- RightRows, mem_jpred(Cond, L, R)] of
        []      ->
            case mem_jpred(Where2, L, #{}) of
                true  -> [{L, #{}}];
                false -> []
            end;
        Matches -> [{L, R} || R <- Matches, mem_jpred(Where2, L, R)]
    end.

%% The right-only `{#{}, R}` map pair (empty left map) a right row contributes for a
%% projection when no surviving left row matches; a matched right row is emitted by the
%% left walk instead.
mem_full_right_only_select_pairs(R, LeftMatches, Cond, Where2) ->
    case [L || L <- LeftMatches, mem_jpred(Cond, L, R)] of
        []      ->
            case mem_jpred(Where2, #{}, R) of
                true  -> [{#{}, R}];
                false -> []
            end;
        _Matches -> []
    end.

%% Order joined pairs by the side-tagged key list. Each key reads the left or the
%% right row of a pair by its side tag, so a join orders by a column from either
%% table; lists:sort/2 is stable, so pairs equal under every key keep their order.
mem_order_pairs([], Pairs) -> Pairs;
mem_order_pairs(Orders, Pairs) ->
    lists:sort(fun(A, B) -> mem_le_pair(Orders, A, B) end, Pairs).

%% Whether pair A sorts no later than pair B under the side-tagged key list. The
%% first key that distinguishes them decides; ties fall through to the next. A
%% `false` tag reads the pair's left row, a `true` tag its right row (normalised
%% from a left join's `none`/`{some, R}` wrapper), so a right-side key over an
%% unmatched left-join row reads as a missing value and keeps its place.
mem_le_pair([], _A, _B) -> true;
mem_le_pair([{Asc, IsRight, Col} | Rest], {LA, RA} = A, {LB, RB} = B) ->
    {RowA, RowB} =
        case IsRight of
            true  -> {mem_right_row(RA), mem_right_row(RB)};
            false -> {mem_left_row(LA), mem_left_row(LB)}
        end,
    case mem_order_cmp(mem_scalar({'QCol', Col}, RowA), mem_scalar({'QCol', Col}, RowB)) of
        eq -> mem_le_pair(Rest, A, B);
        lt -> Asc;
        gt -> not Asc
    end.

%% The right row of a join pair as a map: an inner join carries the row directly;
%% a left join wraps a match as `{some, R}` and a non-match as `none` (or the empty
%% map in the projection path), both of which read as the empty map so a right-side
%% key over an unmatched row has no value.
mem_right_row(R) when is_map(R) -> R;
mem_right_row({some, R})        -> R;
mem_right_row(_)                -> #{}.

%% The left row of a join pair as a map: an inner or left join carries the row
%% directly; a right join wraps a match as `{some, L}` and a non-match as `none`,
%% both normalised here so a left-side key over an unmatched right-join row reads as
%% the empty map (no value, kept in place). The dual of `mem_right_row`.
mem_left_row(L) when is_map(L) -> L;
mem_left_row({some, L})        -> L;
mem_left_row(_)                -> #{}.

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
