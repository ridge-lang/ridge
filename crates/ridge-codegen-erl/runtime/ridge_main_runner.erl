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
%%   killed by an exit signal         -> halt(1) + the reason to stderr
%%
%% The fallthrough to halt(0) is deliberate: programs whose main returns plain
%% `Unit` (e.g. `fn io main () -> Unit = Io.println "..."`) — or any non-Result
%% type — keep the historical "exit 0 unless erl itself fails" behaviour, so
%% this runner is backwards-compatible with every existing example.  Only the
%% explicit `Err _` branch flips the exit code, which is what users writing
%% `fn main () -> Result Unit Text` actually want.
%%
%% `main` runs in its OWN process, monitored from here, rather than directly in
%% the process `-s` hands us. That process is Erlang's boot process, and it
%% traps exits — so an exit signal that would kill a real deployment left
%% `ridge run` reporting success instead. Running main in an ordinary process
%% makes what you see while developing match what the program does when it
%% ships.
-module(ridge_main_runner).
-export([run/1]).

run([ModAtom, FnAtom]) ->
    ridge_rt:diagnostics_to_stderr(),
    {Pid, Ref} = erlang:spawn_monitor(fun() -> exit({ridge_main_result, invoke(ModAtom, FnAtom)}) end),
    receive
        {'DOWN', Ref, process, Pid, {ridge_main_result, Outcome}} ->
            finish(Outcome);
        {'DOWN', Ref, process, Pid, Reason} ->
            %% main was killed by an exit signal rather than returning: report
            %% the reason the same way a released program would fail.
            io:format(standard_error, "main exited: ~p~n", [Reason]),
            erlang:halt(1)
    end;
run(Other) ->
    io:format(standard_error, "ridge_main_runner: bad runner args ~p~n", [Other]),
    erlang:halt(2).

%% Run the entry point, converting its return value or its exception into a
%% plain term. Keeping the try/catch next to the call preserves the stack.
invoke(ModAtom, FnAtom) ->
    try
        {returned, ModAtom:FnAtom()}
    catch
        Class:Reason:Stack -> {crashed, Class, Reason, Stack}
    end.

finish({returned, {error, Msg}}) when is_binary(Msg) ->
    io:format(standard_error, "~ts~n", [Msg]),
    erlang:halt(1);
finish({returned, {error, Other}}) ->
    io:format(standard_error, "~p~n", [Other]),
    erlang:halt(1);
finish({returned, _}) ->
    erlang:halt(0);
finish({crashed, Class, Reason, Stack}) ->
    io:format(standard_error, "main crashed: ~p:~p~nstack: ~p~n", [Class, Reason, Stack]),
    erlang:halt(1).
