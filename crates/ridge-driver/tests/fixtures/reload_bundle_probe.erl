%% Run each case in a fresh VM to keep code versions and timers independent.
-module(reload_bundle_probe).
-export([run/1]).

run(Case) ->
    spawn(fun() -> receive after 10000 -> halt(1) end end),
    {module, ridge_rt} = code:ensure_loaded(ridge_rt),
    A = ridge_bundle_a, B = ridge_bundle_b,
    A1 = beam(A, 1, []), A2 = beam(A, 2, []),
    B1 = beam(B, 1, []), B2 = beam(B, 2, []),
    {module, A} = code:load_binary(A, "old-a", A1),
    {module, B} = code:load_binary(B, "old-b", B1),
    {ok, Actor} = gen_server:start(A, [], []),
    persistent_term:put(ridge_loader_vsn, <<"base">>),
    %% A real unrelated cleanup obligation must survive every rejection
    %% and a successful bundle with cleanup disabled for A and B.
    C = ridge_bundle_cleanup, CN = atom_to_binary(C),
    {module, C} = code:load_binary(C, "cleanup-old", beam(C, 1, [])),
    CleanupMf = #{base_vsn => <<"base">>, new_vsn => <<"base">>,
                  modules => [CN], actors => []},
    {ok, _} = ridge_loader:apply_bundle(iolist_to_binary(json:encode(CleanupMf)),
        [{CN, beam(C, 2, [])}], #{base_vsn => <<"base">>, purge_after_ms => 60000}),
    Timer = whereis(ridge_loader_purge),
    CleanupBefore = ridge_loader:purge_status(),
    Generation = persistent_term:get(ridge_evidence_generation, 0),
    AN = atom_to_binary(A), BN = atom_to_binary(B),
    Mf = #{<<"base_vsn">> => <<"base">>, <<"new_vsn">> => <<"next">>,
           <<"modules">> => [AN, BN], <<"actors">> => []},
    Bins = [{AN, A2}, {BN, B2}],
    Opts = #{base_vsn => <<"base">>, purge_after_ms => 0},
    {Manifest, Blobs, Options, Phase, Reason} = case Case of
        valid -> {Mf, Bins, Opts, ok, ok};
        missing -> {Mf, [{AN, A2}], Opts, preflight, {missing_blob, BN}};
        wrong_name -> {Mf, [{AN, A2}, {BN, beam(ridge_bundle_other, 2, [])}], Opts,
                       preflight, {invalid_modules, [{B, badfile}]}};
        invalid_payload -> {Mf, [{AN, A2}, {BN, <<"FOR1", 0:32, "BEAM", 0>>}], Opts,
                            preflight, {invalid_modules, [{B, badfile}]}};
        garbage -> {Mf, [{AN, A2}, {BN, <<"garbage">>}], Opts,
                    preflight, {invalid_modules, [{B, badfile}]}};
        duplicate_blob -> {Mf, [{AN, A2} | Bins], Opts,
                           preflight, {duplicate_name, blobs, AN}};
        duplicate_module -> {Mf#{<<"modules">> => [AN, AN, BN]}, Bins, Opts,
                             preflight, {duplicate_name, modules, AN}};
        malformed_blob -> {Mf, [{AN, A2}, malformed, {BN, B2}], Opts,
                           preflight, {invalid_field, blob_entry}};
        extra_blob -> {Mf#{<<"modules">> => [AN]}, Bins, Opts,
                       preflight, {unexpected_blob, BN}};
        invalid_name -> {Mf#{<<"modules">> => [AN, 7]}, Bins, Opts,
                         preflight, {invalid_name, modules}};
        invalid_blob_name -> {Mf, [{AN, A2}, {<<255>>, B2}], Opts,
                             preflight, {invalid_name, blob}};
        long_name -> {Mf#{<<"modules">> => [binary:copy(<<"x">>, 256)]}, Bins, Opts,
                      preflight, {invalid_name, modules}};
        wrong_base -> {Mf, Bins, Opts#{base_vsn => <<"wrong">>}, base, ok};
        manifest_base -> {Mf#{<<"base_vsn">> => <<"wrong">>}, Bins, Opts,
                          preflight, {manifest_base_version_mismatch, <<"base">>, <<"wrong">>}};
        invalid_version -> {Mf#{<<"new_vsn">> => 7}, Bins, Opts,
                            preflight, {invalid_field, <<"new_vsn">>}};
        missing_options_base -> {Mf, Bins, maps:remove(base_vsn, Opts),
                                 preflight, {invalid_field, base_vsn}};
        invalid_seed -> {Mf, Bins, Opts#{seed => 1}, preflight, {invalid_field, seed}};
        negative_purge -> {Mf, Bins, Opts#{purge_after_ms => -1},
                           preflight, {invalid_field, purge_after_ms}};
        invalid_format -> {Mf#{<<"format">> => 99}, Bins, Opts,
                           preflight, {invalid_field, <<"format">>}};
        invalid_json -> {<<"{">>, Bins, Opts, preflight, {invalid_field, manifest_json}};
        invalid_manifest -> {[], Bins, Opts, preflight, {invalid_field, manifest}};
        malformed_actor -> {Mf#{<<"actors">> => [7]}, Bins, Opts,
                            preflight, {invalid_field, actor}};
        unknown_actor -> {Mf#{<<"actors">> => [#{<<"beam">> => <<"absent">>}]}, Bins, Opts,
                          preflight, {actor_not_in_modules, <<"absent">>}};
        malformed_rename -> {Mf#{<<"actors">> => [#{<<"beam">> => AN,
            <<"renames">> => [malformed]}]}, Bins, Opts, preflight, {invalid_field, renames, AN}};
        duplicate_actor ->
            Entry = #{<<"beam">> => AN, <<"renames">> => []},
            {Mf#{<<"actors">> => [Entry, Entry]}, Bins, Opts,
             preflight, {duplicate_name, actors, AN}};
        on_load -> {Mf, [{AN, A2}, {BN, beam(B, 2, ["-on_load(loaded/0).", "loaded() -> ok."])}],
                    Opts, preflight, {invalid_modules, [{B, on_load_not_allowed}]}};
        not_purged ->
            %% Keep v1 executing a local receive after loading v2 as current.
            Parent = self(),
            Holder = spawn(fun() -> B:hold(Parent) end),
            receive holding -> ok after 2000 -> error(holder_not_ready) end,
            {module, B} = code:load_binary(B, "second-b", B1),
            put(holder, Holder),
            {Mf, Bins, Opts, load, [{B, not_purged}]}
    end,
    %% A preflight rejection must not even suspend an otherwise affected actor.
    1 = erlang:trace(Actor, true, ['receive']),
    Json = case is_binary(Manifest) of
        true -> Manifest;
        false -> iolist_to_binary(json:encode(Manifest))
    end,
    Result = ridge_loader:apply_bundle(Json, Blobs, Options),
    Ref = erlang:trace_delivered(Actor),
    receive {trace_delivered, Actor, Ref} -> ok after 2000 -> error(trace_timeout) end,
    case Phase of
        ok ->
            {ok, #{modules_loaded := 2}} = Result,
            2 = A:value(), 2 = B:value(), <<"next">> = ridge_loader:current_version(),
            false = (Generation =:= persistent_term:get(ridge_evidence_generation, 0));
        _ ->
            case Phase of
                base -> {error, {base_version_mismatch, <<"wrong">>, <<"base">>}} = Result;
                _ -> {error, {reload_rejected, Phase, Reason}} = Result
            end,
            1 = A:value(), 1 = B:value(), <<"base">> = ridge_loader:current_version(),
            Generation = persistent_term:get(ridge_evidence_generation, 0),
            case Phase of
                load -> true = is_process_alive(get(holder));
                _ ->
                    true = is_process_alive(Timer),
                    receive
                        {trace, Actor, 'receive', {system, _, suspend}} -> error(actor_suspended)
                    after 0 -> ok end
            end
    end,
    true = is_process_alive(Timer), Timer = whereis(ridge_loader_purge),
    CleanupBefore = ridge_loader:purge_status(),
    1 = erlang:trace(Actor, false, ['receive']),
    kept = sys:get_state(Actor),
    ok = gen_server:stop(Actor),
    io:format("bundle-probe-ok ~p~n", [Case]),
    halt().

beam(Module, Value, Extra) ->
    Texts = ["-module(" ++ atom_to_list(Module) ++ ").",
        "-export([value/0,hold/1,init/1,'__ridge_init'/1,handle_call/3,handle_cast/2,code_change/3,'__ridge_state_fields'/0])."] ++ Extra ++ [
        "value() -> " ++ integer_to_list(Value) ++ ".",
        "hold(Parent) -> Parent ! holding, receive stop -> ok end.",
        "init(A) -> ridge_rt:actor_init(" ++ atom_to_list(Module) ++ ", A).", "'__ridge_init'([]) -> {ok,kept}.", "handle_call(_,_,S) -> {reply,S,S}.",
        "handle_cast(_,S) -> {noreply,S}.", "code_change(_,S,_) -> {ok,S}.",
        "'__ridge_state_fields'() -> []."],
    Forms = [begin {ok, Tokens, _} = erl_scan:string(Text),
        {ok, Form} = erl_parse:parse_form(Tokens), Form end || Text <- Texts],
    {ok, Module, Binary} = compile:forms(Forms, [binary, return_errors]),
    Binary.
