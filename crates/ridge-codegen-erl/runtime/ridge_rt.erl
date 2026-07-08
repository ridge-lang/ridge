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
    time_to_micros/1, time_from_micros/1,
    decimal_from_text/1, decimal_to_text/1, decimal_from_int/1, decimal_parse_raw/1,
    decimal_to_float/1, decimal_cmp/2,
    decimal_add/2, decimal_sub/2, decimal_mul/2, decimal_neg/1, decimal_abs/1,
    decimal_round/3, decimal_div/4,
    uuid_from_text/1, uuid_to_text/1, uuid_nil/1, uuid_gen/1, uuid_cmp/2,
    bytes_from_hex/1, bytes_to_hex/1, bytes_from_utf8/1, bytes_to_utf8/1,
    bytes_empty/1, bytes_gen/1, bytes_length/1, bytes_concat/2, bytes_cmp/2,
    int_parse/0, int_parse/1, float_parse/1, float_to_text/1, bool_to_text/1,
    sql_literal/1, sql_value_source/1,
    text_split_all/2, text_replace_all/3, text_join/2, text_slice/3,
    text_like/2,
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
    mem_delete/3, mem_update/4, mem_get_rows/4,
    mem_begin/1, mem_commit/1, mem_rollback/1, mem_close/1,
    mem_ddl_create/3, mem_ddl_drop/2, mem_ddl_add_column/3,
    mem_ddl_drop_column/3, mem_ddl_index/5, mem_ddl_drop_index/2,
    mem_migrations_applied/1, mem_record_migration/2, mem_unrecord_migration/2,
    mem_raw_query/3, mem_raw_exec/3,
    error_field/2,
    mem_run_plan/2,
    mem_run_mutation/2,
    mem_run_mutation_returning/3,
    eval_plan_pure/1,
    quote_keep_all/1, quote_and/2, quote_not_true/1,
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

%% time_to_micros/1 — SqlType Timestamp codec (std.sql).
%% Projects the epoch-microsecond instant out of a Timestamp for the SqlInstant value.
time_to_micros({timestamp, Micros}) -> Micros.

%% time_from_micros/1 — SqlType Timestamp codec (std.sql).
%% Rebuilds a Timestamp from an epoch-microsecond instant read off a SqlInstant.
time_from_micros(Micros) -> {timestamp, Micros}.

%% --- Decimal ---
%%
%% A Decimal is an exact base-10 number held as {decimal, Unscaled, Scale}, both
%% Erlang integers with Scale >= 0, denoting Unscaled * 10^-Scale. Erlang bignums
%% give the unscaled value arbitrary precision, so a Decimal neither rounds a
%% base-10 value the way a Float does nor overflows the way a fixed-width int
%% would.

%% decimal_from_text/1 — std.decimal.fromText.
%% Text -> Result Decimal Error.
decimal_from_text(Bin) ->
    case decimal_parse(string:trim(binary_to_list(Bin))) of
        {ok, U, S} -> {ok, {decimal, U, S}};
        error      -> {error, {error_record, <<"decimal.parse">>,
                               <<"invalid decimal literal">>}}
    end.

%% decimal_to_text/1 — std.decimal.toText. Canonical text, scale preserved.
decimal_to_text({decimal, U, 0}) -> integer_to_binary(U);
decimal_to_text({decimal, U, S}) when S > 0 ->
    Sign = case U < 0 of true -> <<"-">>; false -> <<>> end,
    Digits = decimal_pad_left(integer_to_binary(abs(U)), S + 1),
    Len = byte_size(Digits),
    IntPart = binary:part(Digits, 0, Len - S),
    FracPart = binary:part(Digits, Len - S, S),
    <<Sign/binary, IntPart/binary, ".", FracPart/binary>>.

%% decimal_from_int/1 — std.decimal.fromInt. An integer at scale 0.
decimal_from_int(N) -> {decimal, N, 0}.

%% decimal_parse_raw/1 — std.decimal.parseRaw. Total in shape, raises on malformed
%% input (the `19.99m` literal path, where the lexer has already validated the text).
decimal_parse_raw(Bin) ->
    case decimal_parse(string:trim(binary_to_list(Bin))) of
        {ok, U, S} -> {decimal, U, S};
        error      -> error(badarg)
    end.

%% decimal_to_float/1 — std.decimal.toFloat. Lossy narrowing to an IEEE double.
decimal_to_float({decimal, U, S}) -> U / decimal_pow10(S).

%% decimal_cmp/2 — std.decimal raw compare. Aligns scales, then compares the
%% unscaled values. Returns -1 | 0 | 1, so 1.5 and 1.50 compare equal.
decimal_cmp({decimal, U1, S1}, {decimal, U2, S2}) ->
    S = max(S1, S2),
    A = U1 * decimal_pow10(S - S1),
    B = U2 * decimal_pow10(S - S2),
    if A < B -> -1; A > B -> 1; true -> 0 end.

%% decimal_add/2 — std.decimal.add. Aligns scales, then adds; the result scale is
%% the larger of the two, so no digits are lost.
decimal_add({decimal, U1, S1}, {decimal, U2, S2}) ->
    S = max(S1, S2),
    {decimal, U1 * decimal_pow10(S - S1) + U2 * decimal_pow10(S - S2), S}.

%% decimal_sub/2 — std.decimal.sub. Add the negation.
decimal_sub(A, B) -> decimal_add(A, decimal_neg(B)).

%% decimal_mul/2 — std.decimal.mul. Exact: the scales add and the unscaled values
%% multiply.
decimal_mul({decimal, U1, S1}, {decimal, U2, S2}) -> {decimal, U1 * U2, S1 + S2}.

%% decimal_neg/1 — std.decimal.neg.
decimal_neg({decimal, U, S}) -> {decimal, -U, S}.

%% decimal_abs/1 — std.decimal.abs.
decimal_abs({decimal, U, S}) -> {decimal, abs(U), S}.

%% decimal_round/3 — std.decimal.round. Rounds to T fractional digits with Mode.
%% Asking for more digits than the value has just pads with zeros.
decimal_round(_Mode, T, {decimal, U, S}) when S =< T ->
    {decimal, U * decimal_pow10(T - S), T};
decimal_round(Mode, T, {decimal, U, S}) ->
    P = decimal_pow10(S - T),
    Sign = if U < 0 -> -1; true -> 1 end,
    AbsU = abs(U),
    Q = AbsU div P,
    R = AbsU rem P,
    {decimal, Sign * decimal_round_step(Mode, Sign, Q, R, P), T}.

%% decimal_round_step/5 — apply a rounding mode to a truncated quotient Q whose
%% dropped remainder is R over a unit P (the discarded fraction is R/P, with
%% 0 =< R < P). Sign is the sign of the whole value, for the directional modes.
decimal_round_step(_Mode, _Sign, Q, 0, _P) -> Q;
decimal_round_step('Down', _Sign, Q, _R, _P) -> Q;
decimal_round_step('Up', _Sign, Q, _R, _P) -> Q + 1;
decimal_round_step('Ceiling', Sign, Q, _R, _P) ->
    case Sign > 0 of true -> Q + 1; false -> Q end;
decimal_round_step('Floor', Sign, Q, _R, _P) ->
    case Sign < 0 of true -> Q + 1; false -> Q end;
decimal_round_step('HalfUp', _Sign, Q, R, P) ->
    case 2 * R >= P of true -> Q + 1; false -> Q end;
decimal_round_step('HalfDown', _Sign, Q, R, P) ->
    case 2 * R > P of true -> Q + 1; false -> Q end;
decimal_round_step('HalfEven', _Sign, Q, R, P) ->
    Twice = 2 * R,
    if Twice > P -> Q + 1;
       Twice < P -> Q;
       true -> case Q rem 2 of 0 -> Q; _ -> Q + 1 end
    end.

%% decimal_div/4 — std.decimal.div. Divides to T fractional digits, rounding the
%% result with Mode. A zero divisor is an error record (Ridge `Err`).
decimal_div(_Mode, _T, _A, {decimal, 0, _Sb}) ->
    {error, {error_record, <<"decimal.divide_by_zero">>, <<"division by zero">>}};
decimal_div(Mode, T, {decimal, Ua, Sa}, {decimal, Ub, Sb}) ->
    E = T + Sb - Sa,
    {Num, Den} =
        case E >= 0 of
            true  -> {Ua * decimal_pow10(E), Ub};
            false -> {Ua, Ub * decimal_pow10(-E)}
        end,
    Sign = case (Num < 0) =/= (Den < 0) of true -> -1; false -> 1 end,
    AN = abs(Num),
    AD = abs(Den),
    Q = AN div AD,
    R = AN rem AD,
    {ok, {decimal, Sign * decimal_round_step(Mode, Sign, Q, R, AD), T}}.

%% decimal_parse/1 — parse a char list into {ok, Unscaled, Scale} or error.
%% Grammar: [sign] digits [ . digits ] [ (e|E) [sign] digits ]. Total — any
%% malformed input yields error instead of raising.
decimal_parse(Str) ->
    try
        {Sign, R1} = decimal_sign(Str),
        {IntDigits, R2} = decimal_digits(R1),
        {FracDigits, R3} =
            case R2 of
                [$. | AfterDot] -> decimal_digits(AfterDot);
                _               -> {"", R2}
            end,
        {Exp, R4} = decimal_exponent(R3),
        AllDigits = IntDigits ++ FracDigits,
        case {R4, AllDigits} of
            {[], []} -> error;
            {[], _} ->
                Unscaled = Sign * list_to_integer(AllDigits),
                {U, S} = decimal_normalize(Unscaled, length(FracDigits) - Exp),
                {ok, U, S};
            _ -> error
        end
    catch
        _:_ -> error
    end.

decimal_sign([$- | R]) -> {-1, R};
decimal_sign([$+ | R]) -> {1, R};
decimal_sign(R)        -> {1, R}.

decimal_digits(Str) -> decimal_digits(Str, []).
decimal_digits([C | R], Acc) when C >= $0, C =< $9 -> decimal_digits(R, [C | Acc]);
decimal_digits(R, Acc) -> {lists:reverse(Acc), R}.

%% Optional exponent. On a valid `e[sign]digits` returns {Value, Rest}; with no
%% exponent, {0, Str}. A bare `e` with no digits is left unconsumed so the
%% upstream "consumed everything" check rejects the input.
decimal_exponent([E | R]) when E =:= $e; E =:= $E ->
    {ESign, R1} = decimal_sign(R),
    case decimal_digits(R1) of
        {[], _}  -> {0, [E | R]};
        {ED, R2} -> {ESign * list_to_integer(ED), R2}
    end;
decimal_exponent(Str) -> {0, Str}.

%% Keep the scale non-negative: a positive net exponent scales the unscaled value
%% up and leaves scale 0.
decimal_normalize(U, Scale) when Scale < 0 -> {U * decimal_pow10(-Scale), 0};
decimal_normalize(U, Scale)                -> {U, Scale}.

decimal_pow10(0)             -> 1;
decimal_pow10(N) when N > 0  -> 10 * decimal_pow10(N - 1).

decimal_pad_left(Bin, MinLen) ->
    Len = byte_size(Bin),
    case Len >= MinLen of
        true  -> Bin;
        false -> <<(binary:copy(<<"0">>, MinLen - Len))/binary, Bin/binary>>
    end.

%% Compare two canonical decimal texts by value (the in-memory adapter's ordering
%% and equality over a decimal column). Reuses decimal_cmp, so 1.5 and 1.50 are
%% equal.
decimal_text_cmp(X, Y) -> decimal_cmp(decimal_of_text(X), decimal_of_text(Y)).

%% A comparison key for min/max over a decimal column: the nearest float. The
%% aggregate returns the original exact value, so this only orders the compare.
decimal_text_to_float(S) -> decimal_to_float(decimal_of_text(S)).

%% Parse a canonical decimal text (as produced by decimal_to_text) back to the
%% scaled-integer form. Such text always parses; a malformed value falls back to
%% zero rather than raising.
decimal_of_text(S) ->
    case decimal_parse(binary_to_list(S)) of
        {ok, U, Sc} -> {decimal, U, Sc};
        error       -> {decimal, 0, 0}
    end.

%% --- UUID ---
%% A Uuid is carried as {uuid, CanonicalBin}, where CanonicalBin is the lowercase
%% 8-4-4-4-12 hyphenated text. That is the shape the SQL codec moves across a
%% `uuid` column and the form Postgres reads and writes over the text protocol.

%% uuid_from_text/1 — std.uuid.fromText. Text -> Result Uuid Error. Accepts the
%% canonical hyphenated form in either case and normalises it to lowercase.
uuid_from_text(Bin) ->
    case uuid_canonicalize(Bin) of
        {ok, Canon} -> {ok, {uuid, Canon}};
        error       -> {error, {error_record, <<"uuid.parse">>,
                                <<"invalid uuid">>}}
    end.

%% uuid_to_text/1 — std.uuid.toText. The canonical lowercase text.
uuid_to_text({uuid, Bin}) -> Bin.

%% uuid_nil/1 — std.uuid.nil. The all-zero uuid.
uuid_nil(_Unit) -> {uuid, <<"00000000-0000-0000-0000-000000000000">>}.

%% uuid_gen/1 — std.uuid.gen. A random version-4 uuid from a cryptographic source.
%% The version nibble is pinned to 4 and the variant bits to the RFC 4122 form.
uuid_gen(_Unit) ->
    <<A:48, _:4, B:12, _:2, C:62>> = crypto:strong_rand_bytes(16),
    {uuid, uuid_format(<<A:48, 4:4, B:12, 2:2, C:62>>)}.

%% uuid_cmp/2 — std.uuid.compare. The canonical lowercase text orders the same as
%% the 128-bit value, so a plain binary compare yields the uuid ordering.
uuid_cmp({uuid, A}, {uuid, B}) ->
    if A < B -> -1; A > B -> 1; true -> 0 end.

%% Validate a uuid string and return its lowercase canonical form, or error.
uuid_canonicalize(Bin) ->
    Lower = string:lowercase(string:trim(Bin)),
    case uuid_valid(Lower) of
        true  -> {ok, Lower};
        false -> error
    end.

%% Whether a binary is a canonical 8-4-4-4-12 hyphenated hex uuid.
uuid_valid(<<G1:8/binary, "-", G2:4/binary, "-", G3:4/binary, "-", G4:4/binary, "-", G5:12/binary>>) ->
    uuid_hex(G1) andalso uuid_hex(G2) andalso uuid_hex(G3)
        andalso uuid_hex(G4) andalso uuid_hex(G5);
uuid_valid(_) -> false.

uuid_hex(Bin) ->
    lists:all(fun(C) -> (C >= $0 andalso C =< $9) orelse (C >= $a andalso C =< $f) end,
              binary_to_list(Bin)).

%% Format 16 bytes as canonical 8-4-4-4-12 lowercase hex.
uuid_format(<<A:4/binary, B:2/binary, C:2/binary, D:2/binary, E:6/binary>>) ->
    iolist_to_binary([uuid_hexstr(A), "-", uuid_hexstr(B), "-", uuid_hexstr(C), "-",
                      uuid_hexstr(D), "-", uuid_hexstr(E)]).

uuid_hexstr(Bin) ->
    iolist_to_binary([io_lib:format("~2.16.0b", [B]) || <<B>> <= Bin]).

%% --- Bytes ---
%% A Bytes is a raw binary; its canonical text form is lowercase hex. That hex is
%% what the SQL codec moves across a `bytea` column (with a `\x` prefix on the
%% Postgres wire); the value itself stays raw here, so length and comparison are
%% over the bytes, not their hex spelling.

%% bytes_from_hex/1 — std.bytes.fromHex. Text -> Result Bytes Error. An even-length
%% run of hex digits in either case decodes to the raw bytes; anything else errors.
bytes_from_hex(Bin) ->
    case bytes_decode_hex(Bin) of
        {ok, Raw} -> {ok, Raw};
        error     -> {error, {error_record, <<"bytes.parse">>,
                              <<"invalid hex">>}}
    end.

%% bytes_to_hex/1 — std.bytes.toHex (and the SQL codec's canonical form). Lowercase
%% hex, two digits per byte, no separator.
bytes_to_hex(Raw) ->
    iolist_to_binary([io_lib:format("~2.16.0b", [B]) || <<B>> <= Raw]).

%% bytes_from_utf8/1 — std.bytes.fromUtf8. A Ridge Text is already a UTF-8 binary,
%% so its bytes are the same binary reinterpreted as raw.
bytes_from_utf8(Bin) -> Bin.

%% bytes_to_utf8/1 — std.bytes.toUtf8. Validates the bytes are well-formed UTF-8
%% and returns them as a Text, or an Err when they are not.
bytes_to_utf8(Raw) ->
    case unicode:characters_to_binary(Raw, utf8, utf8) of
        Bin when is_binary(Bin) -> {ok, Bin};
        _ -> {error, {error_record, <<"bytes.utf8">>,
                      <<"not valid UTF-8">>}}
    end.

%% bytes_empty/1 — std.bytes.empty. The empty byte string.
bytes_empty(_Unit) -> <<>>.

%% bytes_gen/1 — std.bytes.gen. n cryptographically-random bytes; a non-positive n
%% yields the empty byte string.
bytes_gen(N) when is_integer(N), N > 0 -> crypto:strong_rand_bytes(N);
bytes_gen(_) -> <<>>.

%% bytes_length/1 — std.bytes.length. The number of bytes.
bytes_length(Raw) -> byte_size(Raw).

%% bytes_concat/2 — std.bytes.concat. Two byte strings end to end.
bytes_concat(A, B) -> <<A/binary, B/binary>>.

%% bytes_cmp/2 — std.bytes.compare. Byte-by-byte unsigned order, which matches how
%% Postgres orders a `bytea` column; a plain binary compare yields exactly that.
bytes_cmp(A, B) ->
    if A < B -> -1; A > B -> 1; true -> 0 end.

%% Decode an even-length hex string (either case) to raw bytes, or error.
bytes_decode_hex(Bin) ->
    S = string:trim(Bin),
    case byte_size(S) rem 2 of
        0 -> bytes_decode_hex_pairs(S, []);
        _ -> error
    end.

bytes_decode_hex_pairs(<<>>, Acc) ->
    {ok, iolist_to_binary(lists:reverse(Acc))};
bytes_decode_hex_pairs(<<H1, H2, Rest/binary>>, Acc) ->
    case {bytes_hex_nibble(H1), bytes_hex_nibble(H2)} of
        {N1, N2} when N1 =/= error, N2 =/= error ->
            bytes_decode_hex_pairs(Rest, [<<((N1 bsl 4) bor N2)>> | Acc]);
        _ -> error
    end.

bytes_hex_nibble(C) when C >= $0, C =< $9 -> C - $0;
bytes_hex_nibble(C) when C >= $a, C =< $f -> C - $a + 10;
bytes_hex_nibble(C) when C >= $A, C =< $F -> C - $A + 10;
bytes_hex_nibble(_) -> error.

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

%% sql_literal/1 — render a SqlValue as an inline SQL literal for the DDL positions a
%% bind parameter cannot fill (a column DEFAULT, a CHECK literal). A text value is
%% single-quoted with embedded quotes doubled; the others spell their value bare.
sql_literal({'SqlInt', N})      -> integer_to_binary(N);
sql_literal({'SqlText', S})     -> <<"'", (binary:replace(S, <<"'">>, <<"''">>, [global]))/binary, "'">>;
sql_literal({'SqlBool', true})  -> <<"TRUE">>;
sql_literal({'SqlBool', false}) -> <<"FALSE">>;
sql_literal({'SqlFloat', F})    -> float_to_text(F);
sql_literal({'SqlInstant', N})  -> <<"'", (iolist_to_binary(calendar:system_time_to_rfc3339(N, [{unit, microsecond}, {offset, "Z"}])))/binary, "'">>;
sql_literal({'SqlDecimal', S})  -> S;
sql_literal({'SqlUuid', S})     -> <<"'", S/binary, "'">>;
sql_literal('SqlNull')          -> <<"NULL">>.

%% sql_value_source/1 — render a SqlValue as the Ridge *source* expression that
%% rebuilds it (the source dual of sql_literal). Each renders the matching factory
%% call, parenthesised for an argument position. A text value is written as a Ridge
%% string literal: wrapped in quotes with backslash then quote escaped, the same
%% escape the schema renderer's `sourceString` runs, so a first render and a
%% re-render agree byte for byte.
sql_value_source({'SqlInt', N})      -> <<"(sqlInt ", (integer_to_binary(N))/binary, ")">>;
sql_value_source({'SqlText', S})     -> <<"(sqlText ", (source_text_literal(S))/binary, ")">>;
sql_value_source({'SqlBool', true})  -> <<"(sqlBool true)">>;
sql_value_source({'SqlBool', false}) -> <<"(sqlBool false)">>;
sql_value_source({'SqlFloat', F})    -> <<"(sqlFloat ", (float_to_text(F))/binary, ")">>;
sql_value_source({'SqlInstant', N})  -> <<"(sqlInstant ", (integer_to_binary(N))/binary, ")">>;
sql_value_source({'SqlDecimal', S})  -> <<"(sqlDecimal ", (source_text_literal(S))/binary, ")">>;
sql_value_source({'SqlUuid', S})     -> <<"(sqlUuid ", (source_text_literal(S))/binary, ")">>;
sql_value_source('SqlNull')          -> <<"(sqlNull ())">>.

%% source_text_literal/1 — a Text as a Ridge string literal: backslash doubled
%% first, then embedded quotes escaped, then wrapped in quotes. Matches
%% schema.ridge's `sourceString` exactly.
source_text_literal(S) ->
    Escaped = binary:replace(
        binary:replace(S, <<"\\">>, <<"\\\\">>, [global]),
        <<"\"">>, <<"\\\"">>, [global]),
    <<"\"", Escaped/binary, "\"">>.

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

%% text_like/2 — SQL LIKE matching, the surface `Text.like s pattern`. `%` matches
%% any run of characters (including empty), `_` any single character, and `\` escapes
%% the next pattern character so a literal `%`/`_`/`\` can be matched. This is the
%% Postgres default-escape semantics, so a `Repo.filter` reified to a `QLike` SQL
%% `LIKE` and the in-memory `Seq` path agree byte for byte.
text_like(S, Pattern) when is_binary(S), is_binary(Pattern) ->
    rt_like_match(unicode:characters_to_list(S), unicode:characters_to_list(Pattern));
text_like(_, _) -> false.

%% Match a subject character list against a pattern character list.
rt_like_match([], [])              -> true;
rt_like_match(S,  [$% | PRest])    -> rt_like_pct(S, PRest);
rt_like_match([C | SRest], [$\\, PC | PRest]) when C =:= PC -> rt_like_match(SRest, PRest);
rt_like_match(_,  [$\\, _PC | _])  -> false;
rt_like_match([_ | SRest], [$_ | PRest]) -> rt_like_match(SRest, PRest);
rt_like_match([C | SRest], [C  | PRest]) -> rt_like_match(SRest, PRest);
rt_like_match(_,  _)               -> false.

%% `%` matches zero or more characters: succeed if the rest of the pattern matches at
%% any suffix of the subject (including the empty one).
rt_like_pct(S, PRest) ->
    rt_like_match(S, PRest) orelse
        case S of
            []          -> false;
            [_ | SRest] -> rt_like_pct(SRest, PRest)
        end.

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

%% quote_not_true/1 — std.repo's `every` wraps its predicate in `IS NOT TRUE` to
%% probe for a violating row. A `Quote`/`QExpr` literal cannot be written in Ridge
%% source, so the `QNotTrue` node is built here from the captured tree. In the SQL
%% backend it renders `(<expr> IS NOT TRUE)`, the three-valued test that counts an
%% unknown (NULL) predicate as a violation; in this interpreter `mem_pred` reads it
%% as `not <expr>` (a column absent from the row already compares as false), so the
%% two backends agree on which rows violate `every`.
quote_not_true(A) ->
    #{tree => {'QNotTrue', maps:get(tree, A)}}.

%% mk_error/2 — build an `Error` record from a code and a message. `Error` is a
%% builtin record `{ code: Text, message: Text }`, which codegen lowers to an
%% atom-keyed map (field access `e.code` compiles to `maps:get(code, _)`). A bare
%% record literal cannot be coerced to the nominal `Error` type outside an
%% instance-method body, so the unique-row terminals build their errors here.
mk_error(Code, Message) ->
    #{code => Code, message => Message}.

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

%% mem_ddl_drop_index/2 — no-op on the schemaless store (there are no indexes).
%% Result Unit Error.
mem_ddl_drop_index(_Id, _Name) -> {ok, ok}.

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

%% mem_unrecord_migration/2 — remove Name from store Id's tracking table, the
%% inverse of mem_record_migration/2 (a rollback forgetting a reverted migration).
%% Result Unit Error.
mem_unrecord_migration(Id, Name) -> mem_call({unrecord_migration, Id, Name}).

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

%% error_field/2 — read an extension field a backend attached to a raw error map,
%% keyed by its binary name. Postgres records the constraint, table, and column a
%% failing statement named (its ErrorResponse fields) under these keys; the
%% in-memory and codec errors carry none, so the lookup answers an empty binary.
error_field(Key, Err) when is_map(Err) ->
    maps:get(Key, Err, <<"">>);
error_field(_Key, _Err) -> <<"">>.

%% The `name` text out of a tracking-table row.
mem_migration_name(Row) ->
    case maps:get(<<"name">>, Row, 'SqlNull') of
        {'SqlText', N} -> N;
        _              -> <<>>
    end.

%% --- query plan ---
%% A query plan is built in Ridge (std.query's planScan/planCombine/planRefine
%% over the typed QueryPlan tree) and crosses the FFI as a tagged tuple
%% ({'PlanScan', …}/{'PlanCombine', …}/{'PlanRefine', …}); mem_eval_plan/3
%% interprets those tags directly, so no Erlang-side plan builders are needed.

%% mem_run_plan/2 — interpret a query plan against the in-memory store and return
%% the combined rows. Result (List Row) Error.
mem_run_plan(Id, Plan) ->
    mem_call({run_plan, Id, Plan}).

%% mem_run_mutation/2 — run a MutationPlan against store Id; answer the affected
%% row count. The write-side dual of mem_run_plan. Result Int Error.
mem_run_mutation(Id, Plan) ->
    mem_call({run_mutation, Id, Plan}).

%% mem_run_mutation_returning/3 — run a MutationPlan against store Id; answer the rows
%% it touched, each projected to Cols (every column when Cols is empty). The in-memory
%% dual of a RETURNING clause. Result (List Row) Error.
mem_run_mutation_returning(Id, Plan, Cols) ->
    mem_call({run_mutation_returning, Id, Plan, Cols}).

%% eval_plan_pure/1 — interpret a plan with no keeper store, for the in-memory `Seq`
%% query source. The plan is rooted at a `PlanList` (rows carried inline), so the empty
%% state is never consulted; returns the rows directly (no Result — a pure in-memory
%% walk over inline rows cannot fail).
eval_plan_pure(Plan) ->
    mem_eval_plan(#{}, 0, Plan).

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
        {{get_rows, Id, Table, Column, Key}, From, Ref} ->
            Rows = maps:get({Id, Table}, State, []),
            Matches = [R || R <- Rows, maps:get(Column, R, 'SqlNull') =:= Key],
            From ! {Ref, {ok, Matches}},
            mem_keeper_loop(State);
        {{delete, Id, Table, Tree}, From, Ref} ->
            Key  = {Id, Table},
            Rows = maps:get(Key, State, []),
            Kept = [R || R <- Rows, not mem_pred(State, Id, Tree, R)],
            Removed = length(Rows) - length(Kept),
            From ! {Ref, {ok, Removed}},
            mem_keeper_loop(State#{Key => Kept});
        {{unrecord_migration, Id, Name}, From, Ref} ->
            %% Remove every tracking-table row whose `name` is Name, mutating the
            %% current tables the same way `delete` does so a transaction snapshot
            %% still restores it on rollback.
            Key  = {Id, <<"_ridge_migrations">>},
            Rows = maps:get(Key, State, []),
            Kept = [R || R <- Rows, maps:get(<<"name">>, R, 'SqlNull') =/= {'SqlText', Name}],
            From ! {Ref, {ok, ok}},
            mem_keeper_loop(State#{Key => Kept});
        {{update, Id, Table, Changes, Tree}, From, Ref} ->
            Key  = {Id, Table},
            Rows = maps:get(Key, State, []),
            {Updated, Changed} = mem_update_rows(State, Id, Changes, Tree, Rows),
            From ! {Ref, {ok, Changed}},
            mem_keeper_loop(State#{Key => Updated});
        {{run_plan, Id, Plan}, From, Ref} ->
            Rows = mem_eval_plan(State, Id, Plan),
            From ! {Ref, {ok, Rows}},
            mem_keeper_loop(State);
        {{run_mutation, Id, Plan}, From, Ref} ->
            {State1, Count} = mem_apply_mutation(State, Id, Plan),
            From ! {Ref, {ok, Count}},
            mem_keeper_loop(State1);
        {{run_mutation_returning, Id, Plan, Cols}, From, Ref} ->
            {State1, Rows} = mem_mutation_affected(State, Id, Plan),
            Returned = [mem_returning_row(Cols, R) || R <- Rows],
            From ! {Ref, {ok, Returned}},
            mem_keeper_loop(State1)
    end.

%% mem_apply_mutation/3 — apply a MutationPlan to the store, returning the updated state
%% and the affected row count. A thin wrapper over mem_mutation_affected: the count is the
%% number of rows it touched, so the count path and the RETURNING path agree on what a
%% write affected.
mem_apply_mutation(State, Id, Plan) ->
    {State1, Rows} = mem_mutation_affected(State, Id, Plan),
    {State1, length(Rows)}.

%% mem_mutation_affected/3 — apply a MutationPlan to the store, returning the updated state
%% and the rows it touched: the inserted rows of an insert or upsert, the changed rows of
%% an update, the removed rows of a delete. The write-side dual of mem_eval_plan; its row
%% count is what mem_apply_mutation answers and its rows what the RETURNING path projects.
%% The predicate is the same QExpr a read walks (mem_pred), so a correlated EXISTS in a
%% mutation predicate is evaluated identically to one in a query.
mem_mutation_affected(State, Id, {'MutInsert', Table, Rows, IdentityCols}) ->
    Key = {Id, Table},
    Existing = maps:get(Key, State, []),
    Filled = mem_fill_identity(Existing, Rows, IdentityCols),
    {State#{Key => Existing ++ Filled}, Filled};
mem_mutation_affected(State, Id, {'MutUpsert', Table, Rows, ConflictCols, UpdateCols}) ->
    Key = {Id, Table},
    Existing = maps:get(Key, State, []),
    {Final, AffRev} = lists:foldl(
        fun(NewRow, {Acc, Aff}) -> mem_upsert_one(Acc, NewRow, ConflictCols, UpdateCols, Aff) end,
        {Existing, []},
        Rows),
    {State#{Key => Final}, lists:reverse(AffRev)};
mem_mutation_affected(State, Id, {'MutUpdate', Table, Changes, Tree}) ->
    Key = {Id, Table},
    Rows = maps:get(Key, State, []),
    {Updated, Affected} = mem_update_rows_aff(State, Id, Changes, Tree, Rows),
    {State#{Key => Updated}, Affected};
mem_mutation_affected(State, Id, {'MutDelete', Table, Tree}) ->
    Key = {Id, Table},
    Rows = maps:get(Key, State, []),
    {Removed, Kept} = lists:partition(fun(R) -> mem_pred(State, Id, Tree, R) end, Rows),
    {State#{Key => Kept}, Removed};
mem_mutation_affected(State, Id, {'MutDeleteKeys', Table, KeyCols, SeedRows}) ->
    Key = {Id, Table},
    Rows = maps:get(Key, State, []),
    Matches = fun(R) -> lists:any(fun(Seed) -> mem_row_conflicts(R, Seed, KeyCols) end, SeedRows) end,
    {Removed, Kept} = lists:partition(Matches, Rows),
    {State#{Key => Kept}, Removed}.

%% Fill the database-generated identity columns an insert omitted. For each identity
%% column the store assigns the next integer — one past the highest value already stored in
%% that column — threading the counter across the batch so a bulk insert gets a contiguous
%% run. A row that already carries the column keeps its own value; a column the store has
%% never seen starts at 1. With no identity columns the rows pass through untouched, which
%% is the raw-insert path that carries every column itself.
mem_fill_identity(_Existing, Rows, []) -> Rows;
mem_fill_identity(Existing, Rows, IdentityCols) ->
    Start = maps:from_list([{C, mem_next_id(Existing, C)} || C <- IdentityCols]),
    {Filled, _} = lists:mapfoldl(
        fun(Row, Counters) -> mem_fill_row_identity(Row, IdentityCols, Counters) end,
        Start, Rows),
    Filled.

%% Fill one row's omitted identity columns from the per-column counters, advancing each
%% counter it consumes. A column already present in the row is left as the caller set it.
mem_fill_row_identity(Row, IdentityCols, Counters) ->
    lists:foldl(
        fun(C, {AccRow, AccCounters}) ->
            case maps:is_key(C, AccRow) of
                true  -> {AccRow, AccCounters};
                false ->
                    N = maps:get(C, AccCounters),
                    {AccRow#{C => {'SqlInt', N}}, AccCounters#{C => N + 1}}
            end
        end, {Row, Counters}, IdentityCols).

%% One past the highest integer already stored in a column — the next identity value. A
%% column with no integer cells yet (an empty table) starts at 1.
mem_next_id(Rows, Col) ->
    Vals = [N || R <- Rows, {'SqlInt', N} <- [maps:get(Col, R, undefined)]],
    case Vals of
        [] -> 1;
        _  -> lists:max(Vals) + 1
    end.

%% Apply one upsert row against the rows accumulated so far, threading the rows it affected
%% (newest first; the caller reverses). With no existing row conflicting on every conflict
%% column, the new row is appended and recorded (an insert). With a conflict and update
%% columns, the matching row's update columns are overwritten from the new row and the
%% merged row is recorded (a DO UPDATE). With a conflict and no update columns, the existing
%% row is left and nothing is recorded (a DO NOTHING) — matching Postgres's ON CONFLICT
%% affected-row set.
mem_upsert_one(Rows, NewRow, ConflictCols, UpdateCols, Aff) ->
    {NewRows, Match} = lists:mapfoldl(
        fun(R, none) ->
                case mem_row_conflicts(R, NewRow, ConflictCols) of
                    true when UpdateCols =:= [] -> {R, nothing};
                    true  -> M = mem_apply_excluded(R, NewRow, UpdateCols), {M, {updated, M}};
                    false -> {R, none}
                end;
           (R, Done) -> {R, Done}
        end, none, Rows),
    case Match of
        none              -> {Rows ++ [NewRow], [NewRow | Aff]};
        nothing           -> {NewRows, Aff};
        {updated, Merged} -> {NewRows, [Merged | Aff]}
    end.

%% Like mem_update_rows, but returning the rows it changed (post-merge) rather than the
%% changed count — the affected rows a RETURNING update hands back. An empty changes map
%% touches nothing.
mem_update_rows_aff(_State, _Id, Changes, _Tree, Rows) when map_size(Changes) =:= 0 ->
    {Rows, []};
mem_update_rows_aff(State, Id, Changes, Tree, Rows) ->
    {Updated, AffRev} = lists:mapfoldl(
        fun(R, Acc) ->
            case mem_pred(State, Id, Tree, R) of
                true  -> M = maps:merge(R, Changes), {M, [M | Acc]};
                false -> {R, Acc}
            end
        end, [], Rows),
    {Updated, lists:reverse(AffRev)}.

%% Project a touched row to the RETURNING columns: every column when Cols is empty
%% (RETURNING *), otherwise just the named columns (a column the row lacks is dropped,
%% as a SELECT of it would read nothing).
mem_returning_row([], Row) -> Row;
mem_returning_row(Cols, Row) ->
    maps:from_list([{C, maps:get(C, Row)} || C <- Cols, maps:is_key(C, Row)]).

%% Whether an existing row conflicts with a new row on every conflict column — the
%% in-memory reading of a unique constraint over those columns. An empty conflict target
%% (a bare ON CONFLICT) never matches: the schemaless store carries no constraints, so it
%% cannot know which rows a "any constraint" conflict would hit, and treats the upsert as
%% a plain insert.
mem_row_conflicts(_R, _NewRow, []) -> false;
mem_row_conflicts(R, NewRow, ConflictCols) ->
    lists:all(
        fun(C) ->
            maps:is_key(C, R) andalso maps:is_key(C, NewRow)
                andalso maps:get(C, R) =:= maps:get(C, NewRow)
        end, ConflictCols).

%% Overwrite the named columns of an existing row with the new row's values — the
%% in-memory `SET col = EXCLUDED.col` for each update column.
mem_apply_excluded(OldRow, NewRow, UpdateCols) ->
    lists:foldl(
        fun(C, Acc) ->
            case maps:find(C, NewRow) of
                {ok, V} -> Acc#{C => V};
                error   -> Acc
            end
        end, OldRow, UpdateCols).

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
    Matches = [R || R <- Rows, mem_pred(State, Id, Pred, R)],
    mem_paginate(mem_distinct(Dist, mem_order(Orders, Matches)), Lim, Off);
mem_eval_plan(State, Id, {'PlanCombine', Op, Left, Right}) ->
    L = mem_eval_plan(State, Id, Left),
    R = mem_eval_plan(State, Id, Right),
    mem_set_op(Op, L, R);
mem_eval_plan(State, Id, {'PlanRefine', Inner, Pred, Orders, Lim, Off, Dist}) ->
    Rows = mem_eval_plan(State, Id, Inner),
    Matches = [R || R <- Rows, mem_pred(State, Id, Pred, R)],
    mem_paginate(mem_distinct(Dist, mem_order(Orders, Matches)), Lim, Off);
mem_eval_plan(_State, _Id, {'PlanList', Rows}) ->
    %% The in-memory `Seq` source: the rows `from` snapshotted, carried inline in the
    %% plan. No store lookup — they are returned as-is for the wrapping verbs to refine.
    Rows;
mem_eval_plan(State, Id, {'PlanExists', Child}) ->
    %% An existence probe: yield one trivial row when the sub-plan matches anything, none
    %% otherwise, so the caller's emptiness check answers the same Bool the SQL probe does.
    case mem_eval_plan(State, Id, Child) of
        []      -> [];
        [_ | _] -> [#{<<"exists">> => {'SqlInt', 1}}]
    end;
mem_eval_plan(State, Id, {'PlanJoin', _Kind, Left, _Right, _Cond, _Where2, Orders, Lim, Off, Dist, _LeftCols, _RightCols} = Plan)
  when element(1, Left) =:= 'PlanJoin' ->
    %% A nested join of three or more tables (its left child is itself a join): flatten
    %% the left-nested spine into ordered leaf scans and the per-step {Kind, Cond,
    %% Where2}, evaluate each leaf, then fold the leaves left to right into flat rows
    %% with each leaf's columns under its `t<i>$` prefix — the same flat shape a binary
    %% join produces, one leaf wider. Each step joins its new leaf by its own kind: an
    %% inner step keeps only the matches, an outer step keeps the unmatched side with
    %% the absent side's columns dropped — a right or full step null-extends the whole
    %% accumulated composite as a unit, so its leaves all read absent together.
    {Leaves, Steps} = mem_flatten_join(Plan),
    LeafRows = [mem_eval_plan(State, Id, Leaf) || Leaf <- Leaves],
    Flat = mem_nary_product(State, Id, LeafRows, Steps),
    mem_paginate(mem_distinct(Dist, mem_order_nary(Orders, Flat)), Lim, Off);
mem_eval_plan(State, Id, {'PlanJoin', <<"INNER">>, Left, Right, Cond, Where2, Orders, Lim, Off, Dist, _LeftCols, _RightCols}) ->
    LeftRows = mem_eval_plan(State, Id, Left),
    RightRows = mem_eval_plan(State, Id, Right),
    Pairs = [{L, R} || L <- LeftRows, R <- RightRows,
                       mem_jpred(Cond, L, R), mem_where2_pair(State, Id, Where2, L, R)],
    Flat = [mem_prefix_pair(L, R) || {L, R} <- mem_order_pairs(Orders, Pairs)],
    mem_paginate(mem_distinct(Dist, Flat), Lim, Off);
mem_eval_plan(State, Id, {'PlanJoin', <<"LEFT">>, Left, Right, Cond, Where2, Orders, Lim, Off, Dist, _LeftCols, _RightCols}) ->
    LeftRows = mem_eval_plan(State, Id, Left),
    RightRows = mem_eval_plan(State, Id, Right),
    Pairs = lists:append([mem_left_pairs_for(State, Id, L, RightRows, Cond, Where2) || L <- LeftRows]),
    Flat = [mem_prefix_left_pair(L, OptR) || {L, OptR} <- mem_order_pairs(Orders, Pairs)],
    mem_paginate(mem_distinct(Dist, Flat), Lim, Off);
mem_eval_plan(State, Id, {'PlanJoin', <<"RIGHT">>, Left, Right, Cond, Where2, Orders, Lim, Off, Dist, _LeftCols, _RightCols}) ->
    LeftRows = mem_eval_plan(State, Id, Left),
    RightRows = mem_eval_plan(State, Id, Right),
    Pairs = lists:append([mem_right_pairs_for(State, Id, R, LeftRows, Cond, Where2) || R <- RightRows]),
    Flat = [mem_prefix_right_pair(OptL, R) || {OptL, R} <- mem_order_pairs(Orders, Pairs)],
    mem_paginate(mem_distinct(Dist, Flat), Lim, Off);
mem_eval_plan(State, Id, {'PlanJoin', <<"FULL">>, Left, Right, Cond, Where2, Orders, Lim, Off, Dist, _LeftCols, _RightCols}) ->
    LeftRows = mem_eval_plan(State, Id, Left),
    RightRows = mem_eval_plan(State, Id, Right),
    LeftSide = lists:append([mem_full_left_pairs_for(State, Id, L, RightRows, Cond, Where2) || L <- LeftRows]),
    RightOnly = lists:append([mem_full_right_only_for(State, Id, R, LeftRows, Cond, Where2) || R <- RightRows]),
    Flat = [mem_prefix_full_pair(OptL, OptR) || {OptL, OptR} <- mem_order_pairs(Orders, LeftSide ++ RightOnly)],
    mem_paginate(mem_distinct(Dist, Flat), Lim, Off);
mem_eval_plan(State, Id, {'PlanProject', Proj, Child, Lim, Off, Dist}) ->
    Rows = mem_eval_plan(State, Id, Child),
    Projected = [mem_project_prefixed(Proj, Row) || Row <- Rows],
    mem_paginate(mem_distinct(Dist, Projected), Lim, Off);
mem_eval_plan(State, Id, {'PlanAggregate', <<"COUNT">>, _Column, _IsRight, Child}) ->
    Rows = mem_eval_plan(State, Id, Child),
    [#{<<"agg">> => {'SqlInt', length(Rows)}}];
mem_eval_plan(State, Id, {'PlanAggregate', Func, Column, _Leaf, Child}) ->
    Rows = mem_eval_plan(State, Id, Child),
    case mem_agg_value_q(Func, Column, Rows) of
        'SqlNull' -> [];
        Value     -> [#{<<"agg">> => Value}]
    end;
mem_eval_plan(State, Id, {'PlanGroup', KeyCol, KeyLeaf, Cols, Having, Child}) ->
    Rows = mem_eval_plan(State, Id, Child),
    mem_group_nary(Rows, KeyLeaf, KeyCol, Cols, Having).

%% The prefixed column name a join aggregate folds: the column under its leaf's
%% `t<Leaf>$` prefix (t0$ for the first leaf, t1$ for a binary join's right, higher
%% for a deeper composite). A `PlanAggregate` over a join folds its child's flat
%% source-prefixed rows, so the column it reads carries the leaf's prefix, mirroring
%% how `mem_pcell` resolves a projection cell.
mem_agg_prefixed_col(Leaf, Column) ->
    <<"t", (integer_to_binary(Leaf))/binary, "$", Column/binary>>.

%% The column a scalar `PlanAggregate` folds: the leaf-prefixed name when the child's
%% rows carry it (a join's flat source-prefixed rows), or the bare column when none do
%% (an unprefixed single-leaf `Seq` source). An outer join's unmatched rows can lack the
%% folded leaf's columns, so it asks whether ANY row carries the prefixed key rather than
%% probing one row — the first might be an unmatched outer row missing that leaf. A `Seq`
%% row never carries a `t<n>$` prefix, so none match and the bare column is read; a join
%% folding a side with no matched rows reads SqlNull either way. Mirrors how `mem_pcell`
%% resolves a left-source projection cell.
mem_agg_col(Leaf, Column, Rows) ->
    Prefixed = mem_agg_prefixed_col(Leaf, Column),
    case lists:any(fun(Row) -> maps:is_key(Prefixed, Row) end, Rows) of
        true  -> Prefixed;
        false -> Column
    end.

%% Project a row through a projection tree into one row keyed by the projection's
%% output aliases. A `QCol` names a left-source column (the t0$ prefix a join
%% flattens the left side under, or the bare column for an unprefixed single-leaf
%% source like an in-memory `Seq`), a `QColR` a right-source column (t1$), a
%% `QColAt I` the I-th leaf of a multi-table composite (t<I>$); a missing column
%% reads SQL NULL.
mem_project_prefixed({'QProj', Cols}, Row) ->
    maps:from_list([{Alias, mem_pcell(Col, Row)} || {Alias, Col} <- Cols]);
mem_project_prefixed(_Other, _Row) ->
    #{}.

%% A left-source `QCol` resolves under the `t0$` join prefix when present, and
%% falls back to the bare column name otherwise — a join row always carries the
%% prefixed key, an unprefixed `Seq` row only the bare one, so one clause projects
%% both. (`'SqlNull'` is a real stored value, so the sentinel for "absent" is
%% `undefined`, kept distinct from a genuine NULL cell.)
mem_pcell({'QCol', C}, Row)     ->
    case maps:get(<<"t0$", C/binary>>, Row, undefined) of
        undefined -> maps:get(C, Row, 'SqlNull');
        Val       -> Val
    end;
mem_pcell({'QColR', C}, Row)    -> maps:get(<<"t1$", C/binary>>, Row, 'SqlNull');
mem_pcell({'QColAt', I, C}, Row) -> maps:get(mem_agg_prefixed_col(I, C), Row, 'SqlNull');
mem_pcell({'QLitInt', N}, _Row)   -> {'SqlInt', N};
mem_pcell({'QLitText', S}, _Row)  -> {'SqlText', S};
mem_pcell({'QLitBool', B}, _Row)  -> {'SqlBool', B};
mem_pcell({'QLitFloat', F}, _Row) -> {'SqlFloat', F};
mem_pcell({'QLitDecimal', D}, _Row) -> {'SqlDecimal', decimal_to_text(D)};
mem_pcell({'QLitUuid', U}, _Row) -> {'SqlUuid', uuid_to_text(U)};
mem_pcell({'QLitInstant', TS}, _Row) -> {'SqlInstant', time_to_micros(TS)};
%% Computed projection cells over a join's flat source-prefixed rows: arithmetic
%% folds its operands (each resolved by the same prefix rules), a CASE picks a
%% branch by its condition read as an N-ary predicate. A cell with no value — a
%% missing column or a runtime division by zero — projects SQL NULL, never
%% dropping the row or crashing the keeper.
mem_pcell({'QAdd', A, B}, Row) -> mem_pcell_arith('+', A, B, Row);
mem_pcell({'QSub', A, B}, Row) -> mem_pcell_arith('-', A, B, Row);
mem_pcell({'QMul', A, B}, Row) -> mem_pcell_arith('*', A, B, Row);
mem_pcell({'QDiv', A, B}, Row) -> mem_pcell_arith('/', A, B, Row);
mem_pcell({'QMod', A, B}, Row) -> mem_pcell_arith('%', A, B, Row);
mem_pcell({'QCase', C, T, E}, Row) ->
    case mem_npred(C, Row) of
        true -> mem_pcell(T, Row);
        _    -> mem_pcell(E, Row)
    end;
mem_pcell(_Other, _Row)         -> 'SqlNull'.

mem_pcell_arith(Op, A, B, Row) ->
    case mem_arith_apply(Op, mem_pcell(A, Row), mem_pcell(B, Row)) of
        undefined -> 'SqlNull';
        Value     -> Value
    end.

%% Flatten a joined {LeftMap, RightMap} pair into one row map with each side's columns
%% prefixed (t0$ for the left source, t1$ for the right) so the two sides never
%% collide on a shared column name. The Ridge decoder strips the prefix per side.
mem_prefix_pair(L, R) ->
    maps:merge(mem_prefix_keys(<<"t0$">>, L), mem_prefix_keys(<<"t1$">>, R)).

mem_prefix_keys(Prefix, M) ->
    maps:fold(fun(K, V, Acc) -> Acc#{<<Prefix/binary, K/binary>> => V} end, #{}, M).

%% Flatten an outer-join pair into one prefixed row, OMITTING the columns of an
%% unmatched side: a left-join row keeps the right columns (t1$) only when the right
%% matched (`{some, R}`), so a missing t1$ prefix in the flat row is the decoder's
%% signal that the right side matched no row. The right- and full-join duals mirror it.
mem_prefix_left_pair(L, {some, R}) -> mem_prefix_pair(L, R);
mem_prefix_left_pair(L, none)      -> mem_prefix_keys(<<"t0$">>, L).

mem_prefix_right_pair({some, L}, R) -> mem_prefix_pair(L, R);
mem_prefix_right_pair(none, R)      -> mem_prefix_keys(<<"t1$">>, R).

mem_prefix_full_pair(OptL, OptR) ->
    maps:merge(mem_prefix_opt(<<"t0$">>, OptL), mem_prefix_opt(<<"t1$">>, OptR)).

mem_prefix_opt(Prefix, {some, M}) -> mem_prefix_keys(Prefix, M);
mem_prefix_opt(_Prefix, none)     -> #{}.

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
%% Fold an aggregate over a quoted value (a `QExpr` — a column or a computed
%% expression over the columns) — the scalar-aggregate and grouped paths alike, where
%% the value is evaluated per row rather than read by a pre-resolved name. A
%% single-table or `Seq` row is read bare; a join's flat leaf-prefixed row resolves
%% each column to its leaf.
mem_agg_value_q(Func, Column, Rows) ->
    mem_agg(Func, mem_agg_values(Column, Rows)).

%% The non-null folded values of an aggregate's quoted key over the rows. The key is
%% a QExpr — a column or a computed expression over the columns — evaluated per row
%% through `mem_nscalar` when the rows are a join's flat leaf-prefixed rows and
%% through `mem_scalar` when they are a single table's or `Seq`'s bare rows, so a
%% bare column resolves to the right cell either way. A row that yields no value (a
%% missing column, a runtime divide-by-zero) drops out of the fold.
mem_agg_values(Column, Rows) ->
    Eval = case mem_rows_prefixed(Rows) of
        true  -> fun(R) -> mem_nscalar(Column, R) end;
        false -> fun(R) -> mem_scalar(Column, R) end
    end,
    [V || R <- Rows, V <- [Eval(R)], V =/= undefined, V =/= 'SqlNull'].

%% Whether the rows are a join's flat leaf-prefixed rows (any key shaped `t<n>$...`)
%% rather than a single table's or `Seq`'s bare rows — decides which scalar
%% evaluator folds an aggregate's key.
mem_rows_prefixed(Rows) ->
    lists:any(fun(Row) -> lists:any(fun mem_is_leaf_key/1, maps:keys(Row)) end, Rows).

mem_is_leaf_key(<<"t", Rest/binary>>) -> mem_leaf_key_tail(Rest);
mem_is_leaf_key(_)                    -> false.

mem_leaf_key_tail(<<C, Rest/binary>>) when C >= $0, C =< $9 -> mem_leaf_key_tail(Rest);
mem_leaf_key_tail(<<"$", _/binary>>)                        -> true;
mem_leaf_key_tail(_)                                        -> false.

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
mem_key({'SqlInstant', N}) -> N;
mem_key({'SqlFloat', F}) -> F;
mem_key({'SqlText', S})  -> S;
mem_key({'SqlDecimal', S}) -> decimal_text_to_float(S);
mem_key({'SqlUuid', S}) -> S;
mem_key({'SqlBool', B})  -> B.

%% An aggregate over a group as a comparison operand: the folded value (a column or
%% a computed expression, evaluated per row) reduced over the group rows. An all-NULL
%% fold has no value, so the comparison fails rather than crashing.
mem_agg_q_or_undef(Func, Col, GR) ->
    case mem_agg_value_q(Func, Col, GR) of
        'SqlNull' -> undefined;
        V         -> V
    end.

%% --- In-memory grouped join ---
%%
%% Group a source's rows by a key column, narrow the groups by a HAVING tree over the
%% group aggregates, and summarise each surviving group. One interpreter for a binary or
%% a deeper composite join — whose flat rows prefix every leaf's columns (`t0$` for the
%% first, `t1$` for a binary right, higher for a composite) — and for a single-leaf
%% in-memory `Seq`, whose rows are unprefixed: `mem_agg_col` resolves the key column to
%% its leaf-prefixed name when the rows carry it and the bare name otherwise, and each
%% grouped aggregate folds its captured `QExpr` value through `mem_agg_value_q`, which
%% reads each column off the right leaf. An unmatched outer row simply lacks that leaf's
%% columns, which read SqlNull and drop out of the fold.
mem_group_nary(Rows, KeyLeaf, KeyCol, Cols, Having) ->
    KeyName = mem_agg_col(KeyLeaf, KeyCol, Rows),
    Groups = mem_group_nary_by(KeyName, Rows),
    Kept = [{K, GR} || {K, GR} <- Groups, mem_having_nary(Having, K, GR)],
    Sorted = lists:sort(fun({KA, _}, {KB, _}) -> mem_order_cmp(KA, KB) =/= gt end, Kept),
    [mem_group_nary_row(Cols, K, GR) || {K, GR} <- Sorted].

%% Partition the rows by the leaf-prefixed key column value, first-seen order.
mem_group_nary_by(KeyName, Rows) ->
    lists:foldl(
        fun(R, Acc) ->
            K = maps:get(KeyName, R, 'SqlNull'),
            case lists:keyfind(K, 1, Acc) of
                {K, GR} -> lists:keyreplace(K, 1, Acc, {K, GR ++ [R]});
                false   -> Acc ++ [{K, [R]}]
            end
        end,
        [],
        Rows).

%% One output row per join group: each `{Alias, Func, Value, Leaf}` folds its captured
%% `QExpr` value (a column or a computed expression, each column read off its leaf),
%% COUNT counts the rows, KEY answers the group key.
mem_group_nary_row(Cols, Key, GR) ->
    maps:from_list([{Alias, mem_group_nary_value(Func, Column, Leaf, Key, GR)}
                    || {Alias, Func, Column, Leaf} <- Cols]).

mem_group_nary_value(<<"KEY">>, _Col, _Leaf, Key, _GR)   -> Key;
mem_group_nary_value(<<"COUNT">>, _Col, _Leaf, _Key, GR) -> {'SqlInt', length(GR)};
mem_group_nary_value(Func, Col, _Leaf, _Key, GR) ->
    mem_agg_value_q(Func, Col, GR).

%% HAVING over a join group: as mem_having, but its scalar-aggregate leaves fold a
%% captured value (a leaf-qualified column or a computed expression over the leaves)
%% off the flat group rows.
mem_having_nary({'QLitBool', true}, _Key, _GR) -> true;
mem_having_nary({'QAnd', L, R}, Key, GR) -> mem_having_nary(L, Key, GR) andalso mem_having_nary(R, Key, GR);
mem_having_nary({'QOr', L, R}, Key, GR)  -> mem_having_nary(L, Key, GR) orelse mem_having_nary(R, Key, GR);
mem_having_nary({'QNot', X}, Key, GR)    -> not mem_having_nary(X, Key, GR);
mem_having_nary({'QEq', L, R}, Key, GR)  -> mem_hrelate_nary(eq, L, R, Key, GR);
mem_having_nary({'QNe', L, R}, Key, GR)  -> not mem_hrelate_nary(eq, L, R, Key, GR);
mem_having_nary({'QLt', L, R}, Key, GR)  -> mem_hrelate_nary(lt, L, R, Key, GR);
mem_having_nary({'QGt', L, R}, Key, GR)  -> mem_hrelate_nary(lt, R, L, Key, GR);
mem_having_nary({'QLe', L, R}, Key, GR)  -> not mem_hrelate_nary(lt, R, L, Key, GR);
mem_having_nary({'QGe', L, R}, Key, GR)  -> not mem_hrelate_nary(lt, L, R, Key, GR);
mem_having_nary(_Other, _Key, _GR)       -> true.

mem_hrelate_nary(Op, L, R, Key, GR) ->
    case {mem_hscalar_nary(L, Key, GR), mem_hscalar_nary(R, Key, GR)} of
        {undefined, _} -> false;
        {_, undefined} -> false;
        {A, B}         -> mem_sql_cmp(Op, A, B)
    end.

mem_hscalar_nary('QGroupKey', Key, _GR) -> Key;
mem_hscalar_nary('QAggCount', _Key, GR) -> {'SqlInt', length(GR)};
mem_hscalar_nary({'QAggSum', Node}, _Key, GR) -> mem_agg_q_or_undef(<<"SUM">>, Node, GR);
mem_hscalar_nary({'QAggAvg', Node}, _Key, GR) -> mem_agg_q_or_undef(<<"AVG">>, Node, GR);
mem_hscalar_nary({'QAggMin', Node}, _Key, GR) -> mem_agg_q_or_undef(<<"MIN">>, Node, GR);
mem_hscalar_nary({'QAggMax', Node}, _Key, GR) -> mem_agg_q_or_undef(<<"MAX">>, Node, GR);
mem_hscalar_nary({'QLitInt', N}, _Key, _GR)   -> {'SqlInt', N};
mem_hscalar_nary({'QLitText', S}, _Key, _GR)  -> {'SqlText', S};
mem_hscalar_nary({'QLitBool', B}, _Key, _GR)  -> {'SqlBool', B};
mem_hscalar_nary({'QLitFloat', F}, _Key, _GR) -> {'SqlFloat', F};
mem_hscalar_nary({'QLitDecimal', D}, _Key, _GR) -> {'SqlDecimal', decimal_to_text(D)};
mem_hscalar_nary({'QLitUuid', U}, _Key, _GR) -> {'SqlUuid', uuid_to_text(U)};
mem_hscalar_nary({'QLitInstant', TS}, _Key, _GR) -> {'SqlInstant', time_to_micros(TS)};
mem_hscalar_nary(_Other, _Key, _GR)           -> undefined.

%% Merge the Changes columns into every row matching the predicate tree, leaving
%% the rest untouched; return `{UpdatedRows, ChangedCount}`. An empty Changes map
%% is a no-op — nothing changes and the count is zero — matching the SQL backend,
%% which cannot emit an empty SET.
mem_update_rows(_State, _Id, Changes, _Tree, Rows) when map_size(Changes) =:= 0 ->
    {Rows, 0};
mem_update_rows(State, Id, Changes, Tree, Rows) ->
    lists:mapfoldl(
        fun(R, Count) ->
            case mem_pred(State, Id, Tree, R) of
                true  -> {maps:merge(R, Changes), Count + 1};
                false -> {R, Count}
            end
        end,
        0,
        Rows).

%% --- In-memory left-outer join ---
%%
%% The nested-loop dual of a backend pushing a LEFT JOIN into SQL: keep the left
%% rows the left-side predicate matches, pair each with every right row the
%% condition accepts and the two-row post-join WHERE keeps, but keep a left row
%% with no matching right row as `{L, none}` instead of dropping it. Both trees are
%% QExprs over both rows: a `QCol` reads the left row, a `QColR` the right.
%% `Where2` is always-true until a join `filter` narrows it.

%% The pairs a single left row contributes under `LEFT JOIN … ON Cond WHERE
%% Where2`. A left row with condition-matching right rows yields one
%% `{L, {some, R}}` per match the post-join Where2 also keeps; if every match
%% fails Where2 the left row drops out entirely (it joined, so there is no NULL
%% row). A left row with no condition match yields the single `{L, none}` row,
%% kept only when Where2 holds with the right side read as NULL (the empty map) —
%% so a Where2 over a right column drops the unmatched rows, mirroring SQL's
%% three-valued `WHERE` after a left join.
mem_left_pairs_for(State, Id, L, RightRows, Cond, Where2) ->
    case [R || R <- RightRows, mem_jpred(Cond, L, R)] of
        []      ->
            case mem_where2_pair(State, Id, Where2, L, #{}) of
                true  -> [{L, none}];
                false -> []
            end;
        Matches -> [{L, {some, R}} || R <- Matches, mem_where2_pair(State, Id, Where2, L, R)]
    end.

%% --- In-memory right-outer join ---
%%
%% Every right row is kept, and the left query's Pred folds into the match (so an
%% unmatched right row keeps a `none` left side rather than being dropped, the way
%% Pred in the post-join WHERE would). Each pair is `{OptLeft, RightRow}` — the
%% left side wrapped `{some, L}` for a match or `none` for an unmatched right row.

%% The pairs a single right row contributes under `… RIGHT JOIN R ON Cond AND Pred
%% WHERE Where2`. A right row with condition-matching left rows (already narrowed by
%% Pred) yields one `{{some, L}, R}` per match the post-join Where2 also keeps; a
%% right row with no match yields the single `{none, R}` row, kept when Where2 holds
%% with the left side read as NULL (the empty map) — so a Where2 over a left column
%% drops the unmatched rows, mirroring SQL's three-valued WHERE after a right join.
mem_right_pairs_for(State, Id, R, LeftMatches, Cond, Where2) ->
    case [L || L <- LeftMatches, mem_jpred(Cond, L, R)] of
        []      ->
            case mem_where2_pair(State, Id, Where2, #{}, R) of
                true  -> [{none, R}];
                false -> []
            end;
        Matches -> [{{some, L}, R} || L <- Matches, mem_where2_pair(State, Id, Where2, L, R)]
    end.

%% --- In-memory full-outer join ---
%%
%% Every row of both tables is kept. The left query's Pred restricts which left rows
%% enter the join — a left row it rejects never appears, not even unmatched. (A right
%% join can fold Pred into the ON because its unmatched left rows are dropped anyway; a
%% full join keeps them, so Pred must filter the left input instead.) The matched and
%% left-only rows come from the left walk; the right-only rows (a right row matching no
%% surviving left row) come from the right walk, so neither is counted twice. Each pair
%% is `{OptLeft, OptRight}`.

%% The matched and left-only pairs a single (surviving) left row contributes — the dual
%% of mem_left_pairs_for with the right side wrapped: one `{{some, L}, {some, R}}` per
%% condition match the post-join Where2 also keeps; or the single `{{some, L}, none}`
%% (kept when Where2 holds with the right side read as NULL) when no right row matches.
mem_full_left_pairs_for(State, Id, L, RightRows, Cond, Where2) ->
    case [R || R <- RightRows, mem_jpred(Cond, L, R)] of
        []      ->
            case mem_where2_pair(State, Id, Where2, L, #{}) of
                true  -> [{{some, L}, none}];
                false -> []
            end;
        Matches -> [{{some, L}, {some, R}} || R <- Matches, mem_where2_pair(State, Id, Where2, L, R)]
    end.

%% The right-only pair a single right row contributes: `{none, {some, R}}` when no
%% surviving left row matches the condition (and Where2 holds with the left side read
%% as NULL). A right row that DID match a left row is already emitted by the left walk,
%% so it contributes nothing here — that keeps a matched row from being counted twice.
mem_full_right_only_for(State, Id, R, LeftMatches, Cond, Where2) ->
    case [L || L <- LeftMatches, mem_jpred(Cond, L, R)] of
        []      ->
            case mem_where2_pair(State, Id, Where2, #{}, R) of
                true  -> [{none, {some, R}}];
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

%% Whether pair A sorts no later than pair B under the leaf-tagged key list. The
%% first key that distinguishes them decides; ties fall through to the next. Leaf 0
%% reads the pair's left row, any other leaf its right row (a binary pair has only
%% these two, normalised from a left join's `none`/`{some, R}` wrapper), so a
%% right-side key over an unmatched left-join row reads as a missing value and keeps
%% its place.
mem_le_pair([], _A, _B) -> true;
mem_le_pair([{Asc, _Leaf, Key} | Rest], {LA, RA} = A, {LB, RB} = B) ->
    KA = mem_jscalar(Key, mem_left_row(LA), mem_right_row(RA)),
    KB = mem_jscalar(Key, mem_left_row(LB), mem_right_row(RB)),
    case mem_order_cmp(KA, KB) of
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

%% Evaluate a join condition node against the (left, right) pair. The structure
%% mirrors mem_pred/2; a `QCol` resolves against the left row and a `QColR`
%% against the right.
mem_jpred({'QAnd', A, B}, L, R)    -> mem_jpred(A, L, R) andalso mem_jpred(B, L, R);
mem_jpred({'QOr', A, B}, L, R)     -> mem_jpred(A, L, R) orelse mem_jpred(B, L, R);
mem_jpred({'QNot', X}, L, R)       -> not mem_jpred(X, L, R);
mem_jpred({'QNotTrue', X}, L, R)   -> not mem_jpred(X, L, R);
mem_jpred({'QEq', A, B}, L, R)     -> mem_jrelate(eq, A, B, L, R);
mem_jpred({'QNe', A, B}, L, R)     -> not mem_jrelate(eq, A, B, L, R);
mem_jpred({'QLt', A, B}, L, R)     -> mem_jrelate(lt, A, B, L, R);
mem_jpred({'QGt', A, B}, L, R)     -> mem_jrelate(lt, B, A, L, R);
mem_jpred({'QLe', A, B}, L, R)     -> not mem_jrelate(lt, B, A, L, R);
mem_jpred({'QGe', A, B}, L, R)     -> not mem_jrelate(lt, A, B, L, R);
mem_jpred({'QLike', V, P}, L, R)   -> mem_like_pred(mem_jscalar(V, L, R), mem_jscalar(P, L, R));
mem_jpred({'QIn', V, Items}, L, R) -> mem_in_pred(mem_jscalar(V, L, R), [mem_jscalar(I, L, R) || I <- Items]);
mem_jpred({'QCol', C}, L, _R)      -> mem_truthy(maps:get(C, L, 'SqlNull'));
mem_jpred({'QColR', C}, _L, R)     -> mem_truthy(maps:get(C, R, 'SqlNull'));
mem_jpred({'QLitBool', B}, _L, _R) -> B;
mem_jpred({'QCase', C, T, E}, L, R) ->
    case mem_jpred(C, L, R) of
        true -> mem_jpred(T, L, R);
        _    -> mem_jpred(E, L, R)
    end;
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
mem_jscalar({'QLitDecimal', D}, _L, _R) -> {'SqlDecimal', decimal_to_text(D)};
mem_jscalar({'QLitUuid', U}, _L, _R) -> {'SqlUuid', uuid_to_text(U)};
mem_jscalar({'QLitInstant', TS}, _L, _R) -> {'SqlInstant', time_to_micros(TS)};
mem_jscalar({'QAdd', A, B}, L, R) -> mem_arith_apply('+', mem_jscalar(A, L, R), mem_jscalar(B, L, R));
mem_jscalar({'QSub', A, B}, L, R) -> mem_arith_apply('-', mem_jscalar(A, L, R), mem_jscalar(B, L, R));
mem_jscalar({'QMul', A, B}, L, R) -> mem_arith_apply('*', mem_jscalar(A, L, R), mem_jscalar(B, L, R));
mem_jscalar({'QDiv', A, B}, L, R) -> mem_arith_apply('/', mem_jscalar(A, L, R), mem_jscalar(B, L, R));
mem_jscalar({'QMod', A, B}, L, R) -> mem_arith_apply('%', mem_jscalar(A, L, R), mem_jscalar(B, L, R));
mem_jscalar({'QCase', C, T, E}, L, R) ->
    case mem_jpred(C, L, R) of
        true -> mem_jscalar(T, L, R);
        _    -> mem_jscalar(E, L, R)
    end;
mem_jscalar(_Other, _L, _R)           -> undefined.

%% --- N-ary (3+ table) inner join evaluation ---
%%
%% A join of three or more tables reaches the runtime as a left-nested PlanJoin. It
%% is evaluated by flattening that spine into ordered leaf scans and the per-step
%% {Cond, Where2}, then building the N-way product of the leaf rows into one flat row
%% per combination, each leaf's columns merged under its own `t<i>$` prefix — the same
%% flat shape mem_prefix_pair produces for two tables, generalised to N. A condition
%% reads its source by leaf index against that flat row (`QCol` leaf 0, `QColR` leaf 1,
%% `QColAt i` leaf i), the dual of how the renderer qualifies columns to `t<i>`.

%% The `t<i>$` key prefix of leaf `i`.
mem_leaf_prefix(Idx) -> <<"t", (integer_to_binary(Idx))/binary, "$">>.

%% Flatten a left-nested join into its ordered leaf scan-plans and the per-step
%% {Kind, Cond, Where2} that joins each leaf after the first. The innermost join's two
%% scans are leaves 0 and 1; each enclosing join adds its right scan as the next leaf,
%% carrying that node's join kind so the fold can apply each step's shape.
mem_flatten_join({'PlanJoin', Kind, Left, Right, Cond, Where2, _O, _L, _Off, _D, _LC, _RC}) ->
    case element(1, Left) of
        'PlanJoin' ->
            {InnerLeaves, InnerSteps} = mem_flatten_join(Left),
            {InnerLeaves ++ [Right], InnerSteps ++ [{Kind, Cond, Where2}]};
        _ ->
            {[Left, Right], [{Kind, Cond, Where2}]}
    end.

%% Fold the per-leaf row lists left to right into flat rows. Leaf 0 seeds the
%% accumulator under `t0$`; each later leaf is joined on by its step's kind.
mem_nary_product(_St, _Id, [], _Steps) -> [];
mem_nary_product(St, Id, [Rows0 | RestLeaves], Steps) ->
    Acc0 = [mem_prefix_keys(<<"t0$">>, R) || R <- Rows0],
    mem_nary_fold(St, Id, Acc0, 1, RestLeaves, Steps).

mem_nary_fold(_St, _Id, Acc, _Idx, [], _Steps) -> Acc;
mem_nary_fold(_St, _Id, Acc, _Idx, _Leaves, []) -> Acc;
mem_nary_fold(St, Id, Acc, Idx, [LeafRows | RestLeaves], [{Kind, Cond, Where2} | RestSteps]) ->
    Acc2 = mem_join_step(St, Id, Idx, Kind, Acc, LeafRows, Cond, Where2),
    mem_nary_fold(St, Id, Acc2, Idx + 1, RestLeaves, RestSteps).

%% One step of the fold: join the next leaf's rows onto the accumulated flat rows by
%% the step's kind. An inner step keeps only the combinations the condition and
%% post-join WHERE accept; an outer step keeps the unmatched side, with the absent
%% side's columns dropped — for a right or full step the whole accumulated composite is
%% dropped as a unit, so its leaves all read absent together. The condition reads its
%% sources by leaf index against the merged row (`QCol` leaf 0, `QColR` leaf 1,
%% `QColAt i` leaf i), the same way the inner product does.
mem_join_step(St, Id, Idx, <<"INNER">>, Acc, LeafRows, Cond, Where2) ->
    Prefix = mem_leaf_prefix(Idx),
    [Merged
     || AccRow <- Acc,
        Rk <- LeafRows,
        Merged <- [maps:merge(AccRow, mem_prefix_keys(Prefix, Rk))],
        mem_npred(Cond, Merged),
        mem_where2_flat(St, Id, Idx + 1, Where2, Merged)];
mem_join_step(St, Id, Idx, <<"LEFT">>, Acc, LeafRows, Cond, Where2) ->
    lists:append([mem_left_step(St, Id, Idx, AccRow, LeafRows, Cond, Where2) || AccRow <- Acc]);
mem_join_step(St, Id, Idx, <<"RIGHT">>, Acc, LeafRows, Cond, Where2) ->
    lists:append([mem_right_step(St, Id, Idx, Rk, Acc, Cond, Where2) || Rk <- LeafRows]);
mem_join_step(St, Id, Idx, <<"FULL">>, Acc, LeafRows, Cond, Where2) ->
    LeftSide  = lists:append([mem_left_step(St, Id, Idx, AccRow, LeafRows, Cond, Where2) || AccRow <- Acc]),
    RightOnly = lists:append([mem_full_right_only_step(St, Id, Idx, Rk, Acc, Cond, Where2) || Rk <- LeafRows]),
    LeftSide ++ RightOnly.

%% The rows an accumulated composite row contributes under a LEFT step (and the left
%% walk of a FULL step): one merged row per leaf row the condition and post-join WHERE
%% accept, or the composite alone (the new leaf's columns absent) when no leaf row
%% matches and the WHERE holds with that leaf read as absent — the N-ary dual of
%% mem_left_pairs_for, the accumulated composite standing in for the single left row.
mem_left_step(St, Id, Idx, AccRow, LeafRows, Cond, Where2) ->
    Prefix = mem_leaf_prefix(Idx),
    Merges = [maps:merge(AccRow, mem_prefix_keys(Prefix, Rk)) || Rk <- LeafRows],
    case [M || M <- Merges, mem_npred(Cond, M)] of
        []      ->
            case mem_where2_flat(St, Id, Idx + 1, Where2, AccRow) of
                true  -> [AccRow];
                false -> []
            end;
        Matches -> [M || M <- Matches, mem_where2_flat(St, Id, Idx + 1, Where2, M)]
    end.

%% The rows a new-leaf row contributes under a RIGHT step: one merged row per
%% accumulated composite row the condition and post-join WHERE accept, or the leaf
%% alone (the whole composite absent as a unit) when no composite row matches and the
%% WHERE holds with the composite read as absent — the N-ary dual of
%% mem_right_pairs_for, the accumulated composite standing in for the single left row.
mem_right_step(St, Id, Idx, Rk, Acc, Cond, Where2) ->
    LeafMap = mem_prefix_keys(mem_leaf_prefix(Idx), Rk),
    Merges = [maps:merge(AccRow, LeafMap) || AccRow <- Acc],
    case [M || M <- Merges, mem_npred(Cond, M)] of
        []      ->
            case mem_where2_flat(St, Id, Idx + 1, Where2, LeafMap) of
                true  -> [LeafMap];
                false -> []
            end;
        Matches -> [M || M <- Matches, mem_where2_flat(St, Id, Idx + 1, Where2, M)]
    end.

%% The right-only rows of a FULL step: a new-leaf row that matched no composite row,
%% kept when the WHERE holds with the composite absent. A leaf row that did match is
%% already emitted by the left walk, so it contributes nothing here — that keeps a
%% matched row from being counted twice, the N-ary dual of mem_full_right_only_for.
mem_full_right_only_step(St, Id, Idx, Rk, Acc, Cond, Where2) ->
    LeafMap = mem_prefix_keys(mem_leaf_prefix(Idx), Rk),
    Merges = [maps:merge(AccRow, LeafMap) || AccRow <- Acc],
    case [M || M <- Merges, mem_npred(Cond, M)] of
        []      ->
            case mem_where2_flat(St, Id, Idx + 1, Where2, LeafMap) of
                true  -> [LeafMap];
                false -> []
            end;
        _Matches -> []
    end.

%% Order flat N-ary rows by the leaf-tagged key list. Each key names its leaf by
%% index and reads off the row's `t<leaf>$`-prefixed cell, so a key over any leaf of
%% the flattened spine sorts the joined rows.
mem_order_nary([], Rows) -> Rows;
mem_order_nary(Orders, Rows) ->
    lists:sort(fun(A, B) -> mem_le_nary(Orders, A, B) end, Rows).

mem_le_nary([], _A, _B) -> true;
mem_le_nary([{Asc, _Leaf, Key} | Rest], A, B) ->
    case mem_order_cmp(mem_nscalar(Key, A), mem_nscalar(Key, B)) of
        eq -> mem_le_nary(Rest, A, B);
        lt -> Asc;
        gt -> not Asc
    end.

%% Evaluate a join condition against one flat, leaf-prefixed N-ary row — the dual of
%% mem_jpred for the multi-way join. A column names its leaf by index and resolves
%% against the row's `t<i>$col` cell.
mem_npred({'QAnd', A, B}, Row)   -> mem_npred(A, Row) andalso mem_npred(B, Row);
mem_npred({'QOr', A, B}, Row)    -> mem_npred(A, Row) orelse mem_npred(B, Row);
mem_npred({'QNot', X}, Row)      -> not mem_npred(X, Row);
mem_npred({'QNotTrue', X}, Row)  -> not mem_npred(X, Row);
mem_npred({'QEq', A, B}, Row)    -> mem_nrelate(eq, A, B, Row);
mem_npred({'QNe', A, B}, Row)    -> not mem_nrelate(eq, A, B, Row);
mem_npred({'QLt', A, B}, Row)    -> mem_nrelate(lt, A, B, Row);
mem_npred({'QGt', A, B}, Row)    -> mem_nrelate(lt, B, A, Row);
mem_npred({'QLe', A, B}, Row)    -> not mem_nrelate(lt, B, A, Row);
mem_npred({'QGe', A, B}, Row)    -> not mem_nrelate(lt, A, B, Row);
mem_npred({'QLike', V, P}, Row)  -> mem_like_pred(mem_nscalar(V, Row), mem_nscalar(P, Row));
mem_npred({'QIn', V, Items}, Row) -> mem_in_pred(mem_nscalar(V, Row), [mem_nscalar(I, Row) || I <- Items]);
mem_npred({'QCol', C}, Row)      -> mem_truthy(mem_left_cell(C, Row));
mem_npred({'QColR', C}, Row)     -> mem_truthy(maps:get(<<"t1$", C/binary>>, Row, 'SqlNull'));
mem_npred({'QColAt', I, C}, Row) -> mem_truthy(maps:get(<<(mem_leaf_prefix(I))/binary, C/binary>>, Row, 'SqlNull'));
mem_npred({'QLitBool', B}, _Row) -> B;
mem_npred({'QCase', C, T, E}, Row) ->
    case mem_npred(C, Row) of
        true -> mem_npred(T, Row);
        _    -> mem_npred(E, Row)
    end;
mem_npred(_Other, _Row)          -> false.

mem_nrelate(Op, A, B, Row) ->
    case {mem_nscalar(A, Row), mem_nscalar(B, Row)} of
        {undefined, _} -> false;
        {_, undefined} -> false;
        {X, Y}         -> mem_sql_cmp(Op, X, Y)
    end.

mem_nscalar({'QCol', C}, Row)      -> mem_left_cell(C, Row);
mem_nscalar({'QColR', C}, Row)     -> mem_ncell(<<"t1$", C/binary>>, Row);
mem_nscalar({'QColAt', I, C}, Row) -> mem_ncell(<<(mem_leaf_prefix(I))/binary, C/binary>>, Row);
mem_nscalar({'QLitInt', N}, _Row)   -> {'SqlInt', N};
mem_nscalar({'QLitText', S}, _Row)  -> {'SqlText', S};
mem_nscalar({'QLitBool', B}, _Row)  -> {'SqlBool', B};
mem_nscalar({'QLitFloat', F}, _Row) -> {'SqlFloat', F};
mem_nscalar({'QLitDecimal', D}, _Row) -> {'SqlDecimal', decimal_to_text(D)};
mem_nscalar({'QLitUuid', U}, _Row) -> {'SqlUuid', uuid_to_text(U)};
mem_nscalar({'QLitInstant', TS}, _Row) -> {'SqlInstant', time_to_micros(TS)};
mem_nscalar({'QAdd', A, B}, Row) -> mem_arith_apply('+', mem_nscalar(A, Row), mem_nscalar(B, Row));
mem_nscalar({'QSub', A, B}, Row) -> mem_arith_apply('-', mem_nscalar(A, Row), mem_nscalar(B, Row));
mem_nscalar({'QMul', A, B}, Row) -> mem_arith_apply('*', mem_nscalar(A, Row), mem_nscalar(B, Row));
mem_nscalar({'QDiv', A, B}, Row) -> mem_arith_apply('/', mem_nscalar(A, Row), mem_nscalar(B, Row));
mem_nscalar({'QMod', A, B}, Row) -> mem_arith_apply('%', mem_nscalar(A, Row), mem_nscalar(B, Row));
mem_nscalar({'QCase', C, T, E}, Row) ->
    case mem_npred(C, Row) of
        true -> mem_nscalar(T, Row);
        _    -> mem_nscalar(E, Row)
    end;
mem_nscalar(_Other, _Row)           -> undefined.

mem_ncell(Key, Row) ->
    case maps:find(Key, Row) of
        {ok, V} -> V;
        error   -> undefined
    end.

%% A left/sole-entity column. A join row carries it under the `t0$` leaf prefix; a
%% single-table or `Seq` row carries it under the bare name. Try the prefix, fall
%% back to bare, so one resolver reads a `QCol` over both row shapes — the n-ary
%% scalar and predicate evaluators run over a projection's bare scan rows as well
%% as a join's prefixed ones.
mem_left_cell(C, Row) ->
    case mem_ncell(<<"t0$", C/binary>>, Row) of
        undefined -> mem_ncell(C, Row);
        Val       -> Val
    end.

%% --- Quoted-predicate interpreter (the in-memory dual of Query.toSql) ---
%%
%% A captured predicate reaches the runtime as a QExpr tree: union variants are
%% tagged tuples ({'QCol', <<"col">>}, {'QLitInt', N}, {'QAnd', L, R}, …) and the
%% leaf bind values are SqlValue tuples ({'SqlInt', N}, {'SqlText', <<…>>}, …).
%% `mem_pred/2` answers whether one row satisfies the tree; the quotation checker
%% has already verified the operand types line up, so a missing column or a
%% cross-type comparison just fails to match rather than crashing.

%% Store-aware predicate evaluation — the dual of `mem_pred/2` that can resolve a
%% correlated `QExists` against the store snapshot. The boolean connectives recurse
%% with the store threaded so an `EXISTS` nested under `AND`/`OR`/`NOT`/`CASE` is
%% reached; a `QExists` probes the inner table's rows (keyed by the same store id as
%% the outer scan) for one the correlated predicate admits — the outer row is the
%% left side and each inner row the right, the two-row shape `mem_jpred` evaluates.
%% Every other node is a leaf comparison that cannot carry an `EXISTS`, so it
%% delegates to the store-free path.
mem_pred(State, Id, {'QAnd', L, R}, Row) ->
    mem_pred(State, Id, L, Row) andalso mem_pred(State, Id, R, Row);
mem_pred(State, Id, {'QOr', L, R}, Row) ->
    mem_pred(State, Id, L, Row) orelse mem_pred(State, Id, R, Row);
mem_pred(State, Id, {'QNot', X}, Row) ->
    not mem_pred(State, Id, X, Row);
mem_pred(State, Id, {'QNotTrue', X}, Row) ->
    not mem_pred(State, Id, X, Row);
mem_pred(State, Id, {'QCase', C, T, E}, Row) ->
    case mem_pred(State, Id, C, Row) of
        true -> mem_pred(State, Id, T, Row);
        _    -> mem_pred(State, Id, E, Row)
    end;
mem_pred(State, Id, {'QExists', _Table, _Corr} = Node, Row) ->
    %% The single-table outer row is leaf 0; hand the probe to mem_corr with the row
    %% flattened under `t0$` and the inner table taking leaf 1, so a nested EXISTS in
    %% the correlated predicate keeps climbing leaves the same way.
    mem_corr(State, Id, 1, Node, mem_prefix_keys(<<"t0$">>, Row));
mem_pred(_State, _Id, Other, Row) ->
    mem_pred(Other, Row).

%% Store-aware evaluation of a (possibly nested) correlated predicate against a flat,
%% leaf-prefixed row — the dual of mem_npred that can resolve a `QExists`. The boolean
%% connectives thread the store so an EXISTS under AND/OR/NOT/CASE is reached; a
%% `QExists` probes the inner table (keyed by the outer scan's store id) for a row the
%% deeper predicate admits, merging it under the next leaf's `t<i>$` prefix so its
%% columns read as that leaf. `NextLeaf` is the index that inner table takes; a nested
%% EXISTS inside it takes the one after. Leaf comparisons carry no EXISTS, so they
%% delegate to the store-free `mem_npred` over the same flat row.
mem_corr(St, Id, NextLeaf, {'QAnd', A, B}, Row) ->
    mem_corr(St, Id, NextLeaf, A, Row) andalso mem_corr(St, Id, NextLeaf, B, Row);
mem_corr(St, Id, NextLeaf, {'QOr', A, B}, Row) ->
    mem_corr(St, Id, NextLeaf, A, Row) orelse mem_corr(St, Id, NextLeaf, B, Row);
mem_corr(St, Id, NextLeaf, {'QNot', X}, Row) ->
    not mem_corr(St, Id, NextLeaf, X, Row);
mem_corr(St, Id, NextLeaf, {'QNotTrue', X}, Row) ->
    not mem_corr(St, Id, NextLeaf, X, Row);
mem_corr(St, Id, NextLeaf, {'QCase', C, T, E}, Row) ->
    case mem_corr(St, Id, NextLeaf, C, Row) of
        true -> mem_corr(St, Id, NextLeaf, T, Row);
        _    -> mem_corr(St, Id, NextLeaf, E, Row)
    end;
mem_corr(St, Id, NextLeaf, {'QExists', Table, Corr}, Row) ->
    InnerRows = maps:get({Id, Table}, St, []),
    Prefix = mem_leaf_prefix(NextLeaf),
    lists:any(
        fun(Inner) ->
            mem_corr(St, Id, NextLeaf + 1, Corr, maps:merge(Row, mem_prefix_keys(Prefix, Inner)))
        end,
        InnerRows);
mem_corr(_St, _Id, _NextLeaf, Other, Row) ->
    mem_npred(Other, Row).

%% Whether a predicate tree carries a correlated `QExists` anywhere a connective can
%% reach — the guard that decides whether a join's WHERE needs the store-aware path.
mem_has_exists({'QExists', _, _})  -> true;
mem_has_exists({'QAnd', A, B})     -> mem_has_exists(A) orelse mem_has_exists(B);
mem_has_exists({'QOr', A, B})      -> mem_has_exists(A) orelse mem_has_exists(B);
mem_has_exists({'QNot', X})        -> mem_has_exists(X);
mem_has_exists({'QNotTrue', X})    -> mem_has_exists(X);
mem_has_exists({'QCase', C, T, E}) -> mem_has_exists(C) orelse mem_has_exists(T) orelse mem_has_exists(E);
mem_has_exists(_)                  -> false.

%% A binary join's post-join WHERE over a (left, right) pair. A WHERE carrying a
%% correlated EXISTS flattens the pair to the leaf-prefixed shape `mem_corr` reads
%% (left = leaf 0, right = leaf 1, the next inner table = leaf 2); an EXISTS-free WHERE
%% takes the fast store-free `mem_jpred` path unchanged.
mem_where2_pair(St, Id, Where2, L, R) ->
    case mem_has_exists(Where2) of
        true ->
            Flat = maps:merge(mem_prefix_keys(<<"t0$">>, L), mem_prefix_keys(<<"t1$">>, R)),
            mem_corr(St, Id, 2, Where2, Flat);
        false ->
            mem_jpred(Where2, L, R)
    end.

%% An N-ary join's post-join WHERE over the already-flat composite row. A WHERE with a
%% correlated EXISTS takes the store-aware path (the inner table joins at `NextLeaf`,
%% one past the composite's leaves); an EXISTS-free WHERE stays on `mem_npred`.
mem_where2_flat(St, Id, NextLeaf, Where2, Row) ->
    case mem_has_exists(Where2) of
        true  -> mem_corr(St, Id, NextLeaf, Where2, Row);
        false -> mem_npred(Where2, Row)
    end.

%% Evaluate a predicate node against a row.
mem_pred({'QAnd', L, R}, Row) -> mem_pred(L, Row) andalso mem_pred(R, Row);
mem_pred({'QOr', L, R}, Row)  -> mem_pred(L, Row) orelse mem_pred(R, Row);
mem_pred({'QNot', X}, Row)    -> not mem_pred(X, Row);
mem_pred({'QNotTrue', X}, Row) -> not mem_pred(X, Row);
mem_pred({'QEq', L, R}, Row)  -> mem_relate(eq, L, R, Row);
mem_pred({'QNe', L, R}, Row)  -> not mem_relate(eq, L, R, Row);
mem_pred({'QLt', L, R}, Row)  -> mem_relate(lt, L, R, Row);
mem_pred({'QGt', L, R}, Row)  -> mem_relate(lt, R, L, Row);
mem_pred({'QLe', L, R}, Row)  -> not mem_relate(lt, R, L, Row);
mem_pred({'QGe', L, R}, Row)  -> not mem_relate(lt, L, R, Row);
mem_pred({'QLike', V, P}, Row)  -> mem_like_pred(mem_scalar(V, Row), mem_scalar(P, Row));
mem_pred({'QIn', V, Items}, Row) -> mem_in_pred(mem_scalar(V, Row), [mem_scalar(I, Row) || I <- Items]);
%% A bare leaf in predicate position is a boolean column or literal.
mem_pred({'QCol', C}, Row)      -> mem_truthy(maps:get(C, Row, 'SqlNull'));
mem_pred({'QLitBool', B}, _Row) -> B;
mem_pred({'QCase', C, T, E}, Row) ->
    case mem_pred(C, Row) of
        true -> mem_pred(T, Row);
        _    -> mem_pred(E, Row)
    end;
%% A correlated `QExists` needs the store snapshot to resolve its inner table, which
%% the store-free path cannot reach. Every store-backed caller routes through
%% `mem_pred/4`; reaching here means an `EXISTS` in a context that has no store
%% (e.g. an in-memory `Seq` filter), which is unsupported — fail loudly rather than
%% silently dropping the predicate.
mem_pred({'QExists', _Table, _Corr}, _Row) ->
    error(exists_requires_store);
mem_pred(_Other, _Row)          -> false.

%% A `QLike` test: both operands resolve to text, then the pattern matcher runs. A
%% missing column or a non-text operand (a SQL NULL included) simply fails to match.
mem_like_pred({'SqlText', S}, {'SqlText', P}) -> text_like(S, P);
mem_like_pred(_, _)                           -> false.

%% A `QIn` test: the value is a member of the list when it equals one of the resolved
%% elements. A missing column (undefined) or a SQL NULL is never a member.
mem_in_pred(undefined, _Items)  -> false;
mem_in_pred('SqlNull', _Items)  -> false;
mem_in_pred(V, Items)           -> lists:any(fun(I) -> I =/= undefined andalso V =:= I end, Items).

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
mem_scalar({'QLitDecimal', D}, _Row) -> {'SqlDecimal', decimal_to_text(D)};
mem_scalar({'QLitUuid', U}, _Row) -> {'SqlUuid', uuid_to_text(U)};
mem_scalar({'QLitInstant', TS}, _Row) -> {'SqlInstant', time_to_micros(TS)};
mem_scalar({'QAdd', A, B}, Row) -> mem_arith_apply('+', mem_scalar(A, Row), mem_scalar(B, Row));
mem_scalar({'QSub', A, B}, Row) -> mem_arith_apply('-', mem_scalar(A, Row), mem_scalar(B, Row));
mem_scalar({'QMul', A, B}, Row) -> mem_arith_apply('*', mem_scalar(A, Row), mem_scalar(B, Row));
mem_scalar({'QDiv', A, B}, Row) -> mem_arith_apply('/', mem_scalar(A, Row), mem_scalar(B, Row));
mem_scalar({'QMod', A, B}, Row) -> mem_arith_apply('%', mem_scalar(A, Row), mem_scalar(B, Row));
mem_scalar({'QCase', C, T, E}, Row) ->
    case mem_pred(C, Row) of
        true -> mem_scalar(T, Row);
        _    -> mem_scalar(E, Row)
    end;
mem_scalar(_Other, _Row)           -> undefined.

%% Apply an arithmetic operator to two evaluated operands. The quotation checker
%% pins one numeric type for both sides (Int or Float; `%` is Int-only), so a
%% well-typed tree only ever reaches the matching int/float clause. A missing
%% operand (undefined / SqlNull) or a zero divisor yields `undefined` — which the
%% enclosing comparison reads as no-match — rather than an exception: the keeper
%% process holds every table's rows, so a runtime division by zero must drop the
%% row, never crash and lose all in-memory state (Postgres aborts the query; the
%% surface rejects a *literal* zero divisor at compile time, covering the common
%% case). Any shape the checker would not have produced also falls through to
%% `undefined`, keeping the evaluator total.
mem_arith_apply(_Op, undefined, _) -> undefined;
mem_arith_apply(_Op, _, undefined) -> undefined;
mem_arith_apply(_Op, 'SqlNull', _) -> undefined;
mem_arith_apply(_Op, _, 'SqlNull') -> undefined;
mem_arith_apply('/', {'SqlInt', _}, {'SqlInt', 0}) -> undefined;
mem_arith_apply('%', {'SqlInt', _}, {'SqlInt', 0}) -> undefined;
mem_arith_apply('%', {'SqlFloat', _}, {'SqlFloat', _}) -> undefined;
mem_arith_apply('/', {'SqlFloat', _}, {'SqlFloat', B}) when B == 0.0 -> undefined;
mem_arith_apply('+', {'SqlInt', A}, {'SqlInt', B}) -> {'SqlInt', A + B};
mem_arith_apply('-', {'SqlInt', A}, {'SqlInt', B}) -> {'SqlInt', A - B};
mem_arith_apply('*', {'SqlInt', A}, {'SqlInt', B}) -> {'SqlInt', A * B};
mem_arith_apply('/', {'SqlInt', A}, {'SqlInt', B}) -> {'SqlInt', A div B};
mem_arith_apply('%', {'SqlInt', A}, {'SqlInt', B}) -> {'SqlInt', A rem B};
mem_arith_apply('+', {'SqlFloat', A}, {'SqlFloat', B}) -> {'SqlFloat', A + B};
mem_arith_apply('-', {'SqlFloat', A}, {'SqlFloat', B}) -> {'SqlFloat', A - B};
mem_arith_apply('*', {'SqlFloat', A}, {'SqlFloat', B}) -> {'SqlFloat', A * B};
mem_arith_apply('/', {'SqlFloat', A}, {'SqlFloat', B}) -> {'SqlFloat', A / B};
mem_arith_apply(_Op, _, _) -> undefined.

%% Equality is exact and type-aware (the tags must match); ordering is defined
%% only for the ordered base types and answers `false` for anything else.
%% A decimal column compares by value, so 1.5 and 1.50 are equal; this clause
%% precedes the structural `eq` below, which would see the two texts as distinct.
mem_sql_cmp(eq, {'SqlDecimal', X}, {'SqlDecimal', Y}) -> decimal_text_cmp(X, Y) =:= 0;
mem_sql_cmp(eq, A, B) -> A =:= B;
mem_sql_cmp(lt, {'SqlInt', X}, {'SqlInt', Y})     -> X < Y;
mem_sql_cmp(lt, {'SqlText', X}, {'SqlText', Y})   -> X < Y;
mem_sql_cmp(lt, {'SqlFloat', X}, {'SqlFloat', Y}) -> X < Y;
mem_sql_cmp(lt, {'SqlInstant', X}, {'SqlInstant', Y}) -> X < Y;
mem_sql_cmp(lt, {'SqlDecimal', X}, {'SqlDecimal', Y}) -> decimal_text_cmp(X, Y) < 0;
%% A uuid column orders by its canonical text, which matches the 128-bit value order;
%% equality rides the generic structural clause above (the canonical form is unique).
mem_sql_cmp(lt, {'SqlUuid', X}, {'SqlUuid', Y}) -> X < Y;
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
mem_le([{Asc, Key} | Rest], A, B) ->
    case mem_order_cmp(mem_scalar(Key, A), mem_scalar(Key, B)) of
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
