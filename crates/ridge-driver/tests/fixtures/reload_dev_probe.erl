%% Each case uses an isolated VM, real disk beams and the real loader.
-module(reload_dev_probe).
-export([run/1, setup/1, state/0]).

run(Case) ->
    setup(Case),
    Result = case Case of
        prepared_bytes ->
            Parent = self(),
            spawn(fun() -> Parent ! {result, ridge_loader:apply("dev.json", <<"base">>)} end),
            Worker = receive {metadata, W} -> W after 2000 -> error(no_metadata) end,
            ok = file:write_file("ridge_dev_a.beam", beam(ridge_dev_a, 3, [])),
            Worker ! continue,
            receive {result, R} -> R after 5000 -> error(no_result) end;
        _ -> ridge_loader:apply("dev.json", <<"base">>)
    end,
    Facts = state(),
    io:format("dev-probe-facts ~0tp ~0tp~n", [Result, Facts]),
    case Case of
        C when C =:= valid; C =:= prepared_bytes; C =:= unicode_name ->
            {ok, #{modules_loaded := 2}} = Result,
            #{a := 2, b := 2, version := <<"next">>, changed := true} = Facts;
        postcommit ->
            {error, {upgrade_failed, _, _, _}} = Result,
            #{a := 2, b := 2, changed := true} = Facts;
        runtime ->
            {error, {reload_rejected, preflight, {module_restart_required, <<"ridge_rt">>}}} = Result,
            #{a := 1, b := 1, version := <<"base">>, changed := false} = Facts;
        stdlib ->
            {error, {reload_rejected, preflight, {module_restart_required, <<"std.list">>}}} = Result,
            #{a := 1, b := 1, version := <<"base">>, changed := false} = Facts;
        _ ->
            {error, {reload_rejected, _, _}} = Result,
            #{a := 1, b := 1, version := <<"base">>, changed := false} = Facts
    end,
    io:format("dev-probe-ok ~p~n", [Case]), halt(0).

setup(Case) ->
    spawn(fun() -> receive after 30000 -> halt(2) end end),
    {module, ridge_rt} = code:ensure_loaded(ridge_rt),
    A = ridge_dev_a,
    B = case Case of unicode_name -> 'ridge_dev_å'; _ -> ridge_dev_b end,
    BFile = atom_to_list(B) ++ ".beam",
    persistent_term:put(dev_probe_b, B),
    register(dev_probe_observer, self()),
    OldExtra = case Case of
        prepared_bytes -> ["'__ridge_state_fields'() -> dev_probe_observer ! {metadata,self()}, receive continue -> [] end."];
        _ -> []
    end,
    {module, A} = code:load_binary(A, "old-a", beam(A, 1, OldExtra)),
    {module, B} = code:load_binary(B, "old-b", beam(B, 1, [])),
    {ok, Actor} = gen_server:start(A, [], []),
    persistent_term:put(dev_probe_actor, Actor),
    persistent_term:put(ridge_loader_vsn, <<"base">>),
    persistent_term:put(dev_probe_generation, persistent_term:get(ridge_evidence_generation, 0)),
    A2 = beam(A, 2, []), B2 = beam(B, 2, []),
    ok = file:write_file("ridge_dev_a.beam", A2),
    case Case of
        missing -> ok;
        corrupt -> ok = file:write_file(BFile, <<"garbage">>);
        wrong_name -> ok = file:write_file(BFile, beam(ridge_dev_wrong, 2, []));
        on_load -> ok = file:write_file(BFile,
            beam(B, 2, ["-on_load(loaded/0).", "loaded() -> error(on_load_ran)."]));
        not_purged ->
            Parent = self(), P = spawn(fun() -> B:hold(Parent) end),
            receive holding -> ok after 2000 -> error(holder_timeout) end,
            persistent_term:put(dev_probe_holder, P),
            {module, B} = code:load_binary(B, "second-b", beam(B, 1, [])),
            ok = file:write_file(BFile, B2);
        postcommit -> ok = file:write_file(BFile, B2);
        _ -> ok = file:write_file(BFile, B2)
    end,
    Modules = case Case of
        runtime -> [<<"ridge_dev_a">>, <<"ridge_rt">>];
        stdlib ->
            ok = file:write_file("std.list.beam", beam('std.list', 2, [])),
            [<<"ridge_dev_a">>, <<"std.list">>];
        _ -> [<<"ridge_dev_a">>, atom_to_binary(B)]
    end,
    %% The new actor metadata fails after the code commit. This is a real
    %% failure in do_apply's migration phase, not a fabricated RPC result.
    Actors = case Case of
        C when C =:= postcommit; C =:= prepared_bytes -> [#{beam => <<"ridge_dev_a">>, renames => []}];
        _ -> []
    end,
    case Case of
        postcommit -> ok = file:write_file("ridge_dev_a.beam", beam(A, 2,
            ["'__ridge_state_fields'() -> error(committed_metadata_failure)."]));
        _ -> ok
    end,
    Manifest = #{base_vsn => <<"base">>, new_vsn => <<"next">>, modules => Modules, actors => Actors},
    ok = file:write_file("dev.json", json:encode(Manifest)),
    io:format("dev-probe-ready~n"), ok.

state() ->
    B = persistent_term:get(dev_probe_b),
    #{a => ridge_dev_a:value(), b => B:value(),
      version => ridge_loader:current_version(),
      changed => persistent_term:get(dev_probe_generation) =/=
                 persistent_term:get(ridge_evidence_generation, 0)}.

beam(Module, Value, Extra) ->
    ModuleText = lists:flatten(io_lib:format("~p", [Module])),
    Fields = case lists:any(fun(S) -> lists:prefix("'__ridge_state_fields'", S) end, Extra) of
        true -> []; false -> ["'__ridge_state_fields'() -> []."]
    end,
    Texts = ["-module(" ++ ModuleText ++ ").",
        "-export([value/0,hold/1,init/1,'__ridge_init'/1,handle_call/3,handle_cast/2,code_change/3,'__ridge_state_fields'/0,'__ridge_state_defaults'/0])."] ++ Extra ++ Fields ++ [
        "value() -> " ++ integer_to_list(Value) ++ ".",
        "hold(Parent) -> Parent ! holding, receive stop -> ok end.",
        "init(A) -> ridge_rt:actor_init(" ++ ModuleText ++ ", A).",
        "'__ridge_init'([]) -> {ok,kept}.",
        "'__ridge_state_defaults'() -> #{}.",
        "handle_call(_,_,S) -> {reply,S,S}.", "handle_cast(_,S) -> {noreply,S}.",
        "code_change(_,S,_) -> {ok,S}."],
    Forms = [begin {ok, Tokens, _} = erl_scan:string(Text),
        {ok, Form} = erl_parse:parse_form(Tokens), Form end || Text <- Texts],
    {ok, Module, Binary} = compile:forms(Forms, [binary, return_errors]), Binary.
