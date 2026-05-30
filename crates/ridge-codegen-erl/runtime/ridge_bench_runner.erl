%% ridge_bench_runner — Layer B micro-benchmark harness.
%%
%% Invoked by `erl -noshell -s ridge_bench_runner run <Mod> [<Mod> ...]
%% -s init stop`.  For each module it finds every exported `bench_*/0`
%% function, runs an untimed warm-up pass, then times N iterations with
%% `erlang:monotonic_time/1`, discards the first timed sample (extra JIT
%% warm), and prints one machine-readable result line per benchmark:
%%
%%   {"bench":"<name>","median_ns":<int>,"p99_ns":<int>,"iters":<int>}
%%
%% Running every benchmark inside a single `erl -noshell` amortises the BEAM
%% boot, which would otherwise dominate micro-benchmark wall time.  The result
%% lines are consumed by the tracking layer, which stamps version / sha / layer
%% around them.
%%
%% Timing is per single call.  A `bench_*/0` body must therefore do enough work
%% to clear the platform clock resolution (~100 ns on Windows); a trivial body
%% reports 0.  The Layer B micro-benchmarks (10k-element match, string-build
%% loop, record churn) are sized accordingly — a body cheaper than that should
%% repeat its own work internally before this harness can measure it.
%%
-module(ridge_bench_runner).
-export([run/1, bench/3]).

%% Untimed warm-up iterations before timing begins (JIT + caches).
-define(WARMUP, 50).
%% Timed iterations per benchmark (the first sample is discarded).
-define(ITERS, 200).

%% run([ModAtom | _]) — entry point for `erl -s ridge_bench_runner run ...`.
run(Mods) when is_list(Mods) ->
    Benches = lists:flatmap(fun discover/1, Mods),
    case Benches of
        [] ->
            io:format(standard_error, "no bench_*/0 functions found in ~p~n", [Mods]),
            erlang:halt(1);
        _ ->
            lists:foreach(fun({Mod, Fn}) -> report(Mod, Fn) end, Benches),
            erlang:halt(0)
    end;
run(Other) ->
    io:format(standard_error, "bad runner args ~p~n", [Other]),
    erlang:halt(2).

%% discover(ModAtom) -> [{Mod, Fn}] — every exported zero-arity bench_*.
discover(Mod) ->
    _ = code:ensure_loaded(Mod),
    Exports = try Mod:module_info(exports) catch _:_ -> [] end,
    [{Mod, Fn} || {Fn, 0} <- Exports, is_bench_name(Fn)].

is_bench_name(Fn) ->
    case atom_to_list(Fn) of
        "bench_" ++ _ -> true;
        _ -> false
    end.

report(Mod, Fn) ->
    {Median, P99, Iters} = bench(Mod, Fn, ?ITERS),
    io:format(
        "{\"bench\":\"~s\",\"median_ns\":~B,\"p99_ns\":~B,\"iters\":~B}~n",
        [Fn, Median, P99, Iters]
    ).

%% bench(Mod, Fn, Iters) -> {MedianNs, P99Ns, MeasuredIters}.
bench(Mod, Fn, Iters) ->
    warmup(Mod, Fn, ?WARMUP),
    Samples = [time_one(Mod, Fn) || _ <- lists:seq(1, Iters)],
    %% Drop the first timed sample (still partly cold).
    Measured =
        case Samples of
            [_ | Rest] -> Rest;
            [] -> []
        end,
    Sorted = lists:sort(Measured),
    {percentile(Sorted, 50), percentile(Sorted, 99), length(Sorted)}.

warmup(_Mod, _Fn, 0) ->
    ok;
warmup(Mod, Fn, N) ->
    _ = Mod:Fn(),
    warmup(Mod, Fn, N - 1).

time_one(Mod, Fn) ->
    T0 = erlang:monotonic_time(nanosecond),
    _ = Mod:Fn(),
    T1 = erlang:monotonic_time(nanosecond),
    T1 - T0.

%% percentile(SortedList, P) -> integer ns. Nearest-rank, P in 0..100.
percentile([], _P) ->
    0;
percentile(Sorted, P) ->
    Len = length(Sorted),
    %% Nearest-rank: rank = ceil(P/100 * N), clamped to [1, N].
    Rank0 = (P * Len + 99) div 100,
    Rank = max(1, min(Len, Rank0)),
    lists:nth(Rank, Sorted).
