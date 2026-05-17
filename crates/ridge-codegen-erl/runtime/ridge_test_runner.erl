%% ridge_test_runner — Test runner bridge for `ridge test`.
%%
%% Invoked by `erl -s ridge_test_runner run <Mod> <Fn> -s init stop -noshell`.
%% Dispatches `Mod:Fn()` (zero-arity — Ridge `pub fn test_*` with no params
%% compiles to BEAM arity-0), maps the return value to pass / fail
%% (signed off 2026-05-01):
%%
%%   {ok, _}                        -> halt(0)           (Result Unit Text, Ok branch)
%%   {error, Msg} when is_binary(Msg) -> halt(1) + stderr  (Result Unit Text, Err branch)
%%   true                           -> halt(0)           (Bool transitional)
%%   false                          -> halt(1) + stderr  (Bool transitional)
%%   any other shape                -> halt(1) + stderr
%%   exception                      -> halt(1) + stderr + stacktrace
%%
-module(ridge_test_runner).
-export([run/1]).

%% run([ModAtom, FnAtom]) — called by `erl -s ridge_test_runner run <Mod> <Fn>`.
%%
%% The two atoms arrive as the -s argument list.  The module and function are
%% the Ridge BEAM module and the test function name respectively.
%%
%% Ridge `pub fn test_*` with zero parameters compiles to a BEAM function of
%% arity 0 (no argument).  We call `Mod:Fn()` — not `Mod:Fn(ok)`.
run([ModAtom, FnAtom]) ->
    try ModAtom:FnAtom() of
        {ok, _} ->
            erlang:halt(0);
        {error, Msg} when is_binary(Msg) ->
            io:format(standard_error, "FAIL: ~ts~n", [Msg]),
            erlang:halt(1);
        true ->
            erlang:halt(0);
        false ->
            io:format(standard_error, "FAIL~n", []),
            erlang:halt(1);
        Other ->
            io:format(standard_error, "FAIL: unexpected return ~p~n", [Other]),
            erlang:halt(1)
    catch
        Class:Reason:Stack ->
            io:format(standard_error, "FAIL: ~p:~p~nstack:~p~n",
                      [Class, Reason, Stack]),
            erlang:halt(1)
    end;
run(Other) ->
    io:format(standard_error, "FAIL: bad runner args ~p~n", [Other]),
    erlang:halt(2).
