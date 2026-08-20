%% ridge_repl_runner — result printer for `ridge repl`.
%%
%% Invoked by `erl -s ridge_repl_runner run <Mod> <Fn> -s init stop -noshell`
%% once the REPL has compiled an entered expression into a temporary
%% workspace. Calls `Mod:Fn()` and renders whatever comes back:
%%
%%   ok                             -> no output          (Unit)
%%   {ok, _}                        -> no output          (Result, Ok branch)
%%   {error, Msg} when is_binary(Msg) -> stderr + halt(1) (Result, Err branch)
%%   true / false                   -> "true" / "false"
%%   integer / float / binary       -> the value
%%   any other shape                -> its Erlang rendering
%%   exception                      -> stderr + halt(1)
%%
%% This module lives beside the other runners rather than inside the CLI
%% crate: it is a BEAM module, so a second backend has to be able to replace
%% it instead of inheriting it.
-module(ridge_repl_runner).
-export([run/1]).

run([ModAtom, FnAtom]) ->
    try ModAtom:FnAtom() of
        ok ->
            erlang:halt(0);
        {ok, _} ->
            erlang:halt(0);
        {error, Msg} when is_binary(Msg) ->
            io:format(standard_error, "error: ~ts~n", [Msg]),
            erlang:halt(1);
        true ->
            io:put_chars(standard_io, <<"true\n">>),
            erlang:halt(0);
        false ->
            io:put_chars(standard_io, <<"false\n">>),
            erlang:halt(0);
        V when is_integer(V) ->
            io:format("~B~n", [V]),
            erlang:halt(0);
        V when is_float(V) ->
            io:format("~g~n", [V]),
            erlang:halt(0);
        V when is_binary(V) ->
            io:format("~ts~n", [V]),
            erlang:halt(0);
        V ->
            io:format("~p~n", [V]),
            erlang:halt(0)
    catch
        %% The same report `ridge run` gives, under the same opening word: an
        %% expression that crashes in the REPL crashed for the same reasons,
        %% and a person switching between the two should not have to learn
        %% two vocabularies for one failure.
        Class:Reason:Stack ->
            ridge_rt:print_failure(<<"error: ">>, Class, Reason, Stack),
            erlang:halt(1)
    end;
run(Other) ->
    io:format(standard_error, "error: bad runner args ~p~n", [Other]),
    erlang:halt(2).
