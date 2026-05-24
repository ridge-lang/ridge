%% ridge_main_runner — entry-point bridge for `ridge run`.
%%
%% Invoked by `erl -s ridge_main_runner run <Mod> <Fn> -s init stop -noshell`.
%% Calls `Mod:Fn()` (zero-arity), pattern-matches the return value, and exits
%% with a code that respects the standard `Result Unit Text` convention:
%%
%%   {error, Msg} when is_binary(Msg) -> halt(1) + write Msg to stderr
%%   {error, Other}                   -> halt(1) + write `~p` of Other to stderr
%%   anything else                    -> halt(0) silently
%%   exception                        -> halt(1) + crash report to stderr
%%
%% The fallthrough to halt(0) is deliberate: programs whose main returns plain
%% `Unit` (e.g. `fn io main () -> Unit = Io.println "..."`) — or any non-Result
%% type — keep the historical "exit 0 unless erl itself fails" behaviour, so
%% this runner is backwards-compatible with every existing example.  Only the
%% explicit `Err _` branch flips the exit code, which is what users writing
%% `fn main () -> Result Unit Text` actually want.
-module(ridge_main_runner).
-export([run/1]).

run([ModAtom, FnAtom]) ->
    try ModAtom:FnAtom() of
        {error, Msg} when is_binary(Msg) ->
            io:format(standard_error, "~ts~n", [Msg]),
            erlang:halt(1);
        {error, Other} ->
            io:format(standard_error, "~p~n", [Other]),
            erlang:halt(1);
        _ ->
            erlang:halt(0)
    catch
        Class:Reason:Stack ->
            io:format(standard_error, "main crashed: ~p:~p~nstack: ~p~n",
                      [Class, Reason, Stack]),
            erlang:halt(1)
    end;
run(Other) ->
    io:format(standard_error, "ridge_main_runner: bad runner args ~p~n", [Other]),
    erlang:halt(2).
