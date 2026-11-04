%% Each ordering is driven by acknowledgements, not elapsed sleeps. The same
%% census and commit cases also run against actual compiler-generated actors.
-module(reload_startup_probe).
-export([run/1, run_generated/4, initialize/2]).

run(Case) ->
    setup(),
    M = startup_actor,
    {module, M} = code:load_binary(M, "old", actor_beam(M, 1)),
    test(Case, M, [{atom_to_binary(M), actor_beam(M, 2)}]),
    io:format("startup-probe-ok ~p~n", [Case]), halt().

run_generated(OldDir, NewDir, M, Case) ->
    setup(),
    true = code:add_patha(OldDir),
    {module, M} = code:ensure_loaded(M),
    Parent = list_to_atom(lists:sublist(atom_to_list(M), length(atom_to_list(M)) - 8)),
    Bins = [{atom_to_binary(Mod), begin
        {ok, B} = file:read_file(filename:join(NewDir, atom_to_list(Mod) ++ ".beam")), B
    end} || Mod <- [Parent, M]],
    test(Case, M, Bins),
    io:format("generated-startup-ok ~p~n", [Case]), halt().

setup() ->
    spawn(fun() -> receive after 25000 -> halt(2) end end),
    register(startup_observer, self()),
    {module, ridge_rt} = code:ensure_loaded(ridge_rt),
    persistent_term:put(ridge_loader_vsn, <<"base">>).

test(active_initializer, M, Bins) ->
    Caller = launch(fun() -> gen_server:start(M, [block], []) end),
    Child = receive {initializing, P} -> P after 3000 -> error(no_init) end,
    {error, {reload_rejected, startup, initializing}} = upgrade(M, Bins),
    <<"base">> = ridge_loader:current_version(),
    Child ! continue, {ok, Child} = result(Caller),
    false = maps:is_key(step, sys:get_state(Child)),
    {ok, _} = upgrade(M, Bins), 2 = value(Child);
test(Case, M, Bins) when Case =:= census_birth; Case =:= dependency;
                              Case =:= queued; Case =:= queued_death;
                              Case =:= queued_args; Case =:= queued_args_failure;
                              Case =:= supervised_queued; Case =:= queued_worker_death;
                              Case =:= queued_owner_death ->
    {Actor, Supervisor} = case Case of
        supervised_queued ->
            {Sup, InitialPid} = supervised_actor(M, true), {InitialPid, Sup};
        _ -> {ok, InitialPid} = gen_server:start(M, [], []), {InitialPid, undefined}
    end,
    [gen_server:cast(Actor, {tick}) || _ <- lists:seq(1, 7)],
    7 = value(Actor), false = maps:is_key(step, sys:get_state(Actor)),
    case Case of
        census_birth ->
            persistent_term:put(startup_metadata_barrier, true),
            Caller = launch(fun() -> upgrade(M, Bins) end),
            Worker = receive {metadata, P} -> P after 3000 -> error(no_census_barrier) end,
            {ridge_handle, New, _} = ridge_rt:spawn_actor(M, [], []),
            0 = value(New),
            %% Cancellation must already have resumed old actors even though
            %% the applicator is still blocked in metadata.
            7 = value(Actor),
            Worker ! continue,
            {error, {reload_rejected, startup, actor_started}} = result(Caller),
            <<"base">> = ridge_loader:current_version(),
            false = maps:is_key(step, sys:get_state(New)),
            persistent_term:put(startup_metadata_barrier, false),
            {ok, _} = upgrade(M, Bins), 9 = value(Actor), 2 = value(New);
        _ ->
            {TestBins, InitArgs} = case Case of
                ArgCase when ArgCase =:= queued_args; ArgCase =:= queued_args_failure ->
                    R = startup_seed,
                    {module, R} = code:load_binary(R, "old-record", record_beam(1, true)),
                    Seed = #{'__ridge_v' => {R, seed, 1}, count => 11},
                    {ok, Seed} = ridge_rt:migrate_value(Seed),
                    {Bins ++ [{atom_to_binary(R), record_beam(2, Case =:= queued_args)}], [Seed]};
                _ -> {Bins, []}
            end,
            Observer = self(), Fake = spawn(fun() -> fake_purge(Observer) end),
            true = register(ridge_loader_purge, Fake),
            Caller = launch(fun() -> upgrade(M, TestBins) end),
            Grant = receive {purge_barrier, Ref} -> Ref after 3000 -> error(no_purge_barrier) end,
            Worker = whereis(ridge_loader_admission),
            case Case of
                dependency ->
                    Start = launch(fun() -> ridge_rt:spawn_actor(M, [{ask, Actor}], []) end),
                    {ridge_handle, Child, _} = result(Start),
                    %% The owner cannot process cancellation at this barrier.
                    %% The child must nevertheless finish its call to Actor.
                    0 = value(Child), 7 = value(Actor),
                    Fake ! {release, Grant},
                    {error, {reload_rejected, startup, actor_started}} = result(Caller),
                    <<"base">> = ridge_loader:current_version(),
                    {ok, _} = upgrade(M, Bins), 9 = value(Actor), 2 = value(Child);
                _ ->
                    true = erlang:suspend_process(Worker),
                    Fake ! {release, Grant}, wait_grant(Worker, deadline()),
                    %% Observe the actual enter request at the coordinator so
                    %% no absence assertion depends on scheduling the child.
                    Gate = whereis(ridge_actor_startup),
                    1 = erlang:trace(Gate, true, ['receive']),
                    Start = launch(fun() ->
                        case Case of
                            supervised_queued -> kill(Actor),
                                {ok, replacement(Supervisor, Actor, deadline())};
                            _ -> gen_server:start(M, InitArgs, [])
                        end
                    end),
                    Child = receive
                        {trace, Gate, 'receive', {startup_call, P, _, enter}} -> P
                    after 3000 -> error(no_queued_initializer) end,
                    receive {done, Start, _} -> error(initialized_before_commit) after 0 -> ok end,
                    receive {init_called, Child} -> error(init_effect_before_commit) after 0 -> ok end,
                    1 = erlang:trace(Gate, false, ['receive']),
                    case Case of queued_death -> kill(Child); _ -> ok end,
                    case Case of
                        queued_worker_death -> kill(Worker);
                        queued_owner_death -> kill(whereis(ridge_loader_recovery));
                        _ -> ok
                    end,
                    case Case of
                        queued_worker_death -> ok;
                        _ -> true = erlang:resume_process(Worker)
                    end,
                    case Case of
                        Fault when Fault =:= queued_worker_death; Fault =:= queued_owner_death ->
                            {error, {upgrade_failed, _, _, _}} = result(Caller),
                            {error, {ridge_actor_startup_recovery_required, _}} = result(Start),
                            false = is_process_alive(Actor),
                            <<"base">> = ridge_loader:current_version(),
                            {error, _} = gen_server:start(M, [], []),
                            receive {init_called, Child} -> error(init_ran_after_unknown_load) after 0 -> ok end,
                            {error, {reload_recovery_required, restart_node}} = upgrade(M, Bins);
                        _ -> finish_queued_case(Case, Caller, Start, Actor, Child, M)
                    end
            end
    end;
test(init_error, M, _Bins) ->
    {error, {expected_init_failure, _}} = gen_server:start(M, [fail], []),
    {ok, Child} = gen_server:start(M, [], []), 0 = value(Child),
    [] = element(2, process_info(Child, links));
test(nested_init, M, Bins) ->
    {ok, Actor} = gen_server:start(M, [{nested, M}], []),
    0 = value(Actor), {ok, _} = upgrade(M, Bins), 2 = value(Actor);
test(spawner_death, M, Bins) ->
    Spawner = launch(fun() -> gen_server:start(M, [block], []) end),
    Child = receive {initializing, P} -> P after 3000 -> error(no_init) end,
    kill(Spawner), true = is_process_alive(Child),
    {error, {reload_rejected, startup, initializing}} = upgrade(M, Bins),
    Child ! continue, 0 = value(Child),
    {ok, _} = upgrade(M, Bins), 2 = value(Child);
test(gate_death, M, Bins) ->
    {ok, Actor} = gen_server:start(M, [], []),
    kill(whereis(ridge_actor_startup)),
    {error, _} = gen_server:start(M, [], []),
    {error, {reload_rejected, startup, recovery_required}} = upgrade(M, Bins),
    0 = value(Actor), <<"base">> = ridge_loader:current_version();
test(resume_timeout, M, Bins) ->
    {ok, Actor} = gen_server:start(M, [], []),
    persistent_term:put(startup_metadata_barrier, true),
    Caller = launch(fun() -> upgrade(M, Bins) end),
    Worker = receive {metadata, P} -> P after 3000 -> error(no_metadata_barrier) end,
    %% Unlike sys suspension, this test-only scheduler suspension prevents
    %% the confirmed actor from acknowledging any system resume request.
    true = erlang:suspend_process(Actor),
    Ref = monitor(process, Actor),
    {ok, Child} = gen_server:start(M, [], []), 0 = value(Child),
    Worker ! continue,
    {error, {reload_recovery_required, restart_node}} = result(Caller),
    receive {'DOWN', Ref, process, Actor, killed} -> ok after 1000 -> error(unresolved_resume) end,
    <<"base">> = ridge_loader:current_version(),
    {error, {reload_recovery_required, restart_node}} = upgrade(M, Bins),
    {error, _} = gen_server:start(M, [], []);
test(legacy_bundle, M, _Bins) ->
    {ok, Actor} = gen_server:start(M, [], []),
    {error, {reload_rejected, preflight, {actor_rebuild_required, M}}} =
        upgrade(M, [{atom_to_binary(M), legacy_beam(M)}]),
    0 = value(Actor), false = maps:is_key(step, sys:get_state(Actor)),
    <<"base">> = ridge_loader:current_version();
test(unsupported_protocol, M, Bins) ->
    {ok, Actor} = gen_server:start(M, [], []),
    [gen_server:cast(Actor, {tick}) || _ <- lists:seq(1, 7)], 7 = value(Actor),
    {error, {reload_rejected, preflight, unsupported_startup_protocol}} =
        ridge_loader:apply_bundle(2, <<"invalid manifest">>, [], #{}),
    undefined = whereis(ridge_loader_recovery),
    undefined = persistent_term:get(ridge_loader_active_reload, undefined),
    7 = value(Actor), false = maps:is_key(step, sys:get_state(Actor)),
    <<"base">> = ridge_loader:current_version(),
    {ok, _} = upgrade(M, Bins), 9 = value(Actor);
test(legacy_live_actor, M, Bins) ->
    Legacy = startup_legacy,
    {module, Legacy} = code:load_binary(Legacy, "legacy", legacy_beam(Legacy)),
    {ok, Old} = gen_server:start(Legacy, [], []),
    receive {legacy_init, Old} -> ok after 1000 -> error(no_legacy_init) end,
    {error, {reload_rejected, startup, actor_rebuild_required}} = upgrade(M, Bins),
    {'EXIT', {ridge_actor_rebuild_required, _}} = catch ridge_rt:spawn_actor(Legacy, [], []),
    Spec = #{id => <<"legacy">>, start => {Legacy, start_link, []}},
    {error, _} = ridge_rt:start_supervisor('OneForOne', 3, 5000, [Spec]),
    receive {legacy_init, _} -> error(legacy_init_bypassed_guard) after 0 -> ok end,
    <<"base">> = ridge_loader:current_version(), 0 = value(Old),
    ok = gen_server:stop(Old),
    {ok, _} = upgrade(M, Bins);
test(Case, M, Bins) when Case =:= supervised; Case =:= supervised_static ->
    {Sup, Child} = supervised_actor(M, Case =:= supervised_static),
    [gen_server:cast(Child, {tick}) || _ <- lists:seq(1, 7)], 7 = value(Child),
    {ok, _} = upgrade(M, Bins), 9 = value(Child),
    kill(Child), Fresh = replacement(Sup, Child, deadline()),
    2 = value(Fresh),
    {links, Links} = process_info(Fresh, links), true = lists:member(Sup, Links).

finish_queued_case(Case, Caller, Start, Actor, Child, M) ->
    {ok, _} = result(Caller),
    case Case of
        supervised_queued -> false = is_process_alive(Actor);
        _ -> 9 = value(Actor)
    end,
    case Case of
        supervised_queued -> {ok, Child} = result(Start), 2 = value(Child),
                             #{count := 0, step := 2} = sys:get_state(Child);
        queued -> {ok, Child} = result(Start), 2 = value(Child),
                  #{count := 0, step := 2} = sys:get_state(Child);
        queued_args ->
            {ok, Child} = result(Start), 13 = value(Child),
            receive {init_called, Child} -> ok after 1000 -> error(init_not_called) end,
            receive {init_called, Child} -> error(init_replayed) after 0 -> ok end;
        queued_args_failure ->
            {error, {ridge_actor_init_args_migration_failed, _}} = result(Start),
            receive {init_called, Child} -> error(init_called_on_bad_args) after 0 -> ok end;
        queued_death -> {error, _} = result(Start),
                        {ok, Fresh} = gen_server:start(M, [], []), 2 = value(Fresh)
    end.

supervised_actor(M, Static) ->
    Spec = #{id => <<"child">>, start => {M, start_link, []}, restart => permanent},
    Specs = case Static of true -> [Spec]; false -> [] end,
    {ok, {ridge_sup, Sup} = Handle} = ridge_rt:start_supervisor('OneForOne', 3, 5000, Specs),
    case Static of
        false -> {ok, {ridge_sup_handle, Sup, <<"child">>, bounded}} =
                     ridge_rt:start_supervised_child(Handle, Spec);
        true -> ok
    end,
    [{<<"child">>, Child, worker, [M]}] = supervisor:which_children(Sup),
    {Sup, Child}.

value(Pid) -> gen_server:call(Pid, {count}, 1000).
launch(Fun) -> Parent = self(), spawn(fun() -> Parent ! {done, self(), Fun()} end).
result(Pid) -> receive {done, Pid, R} -> R after 7000 -> error(missing_result) end.
deadline() -> erlang:monotonic_time(millisecond) + 3000.
retry(Fun, Deadline) ->
    true = erlang:monotonic_time(millisecond) < Deadline,
    receive after 1 -> Fun() end.
wait_grant(Worker, Deadline) ->
    {messages, Messages} = process_info(Worker, messages),
    case lists:any(fun({R, ok}) when is_reference(R) -> true; (_) -> false end, Messages) of
        true -> ok;
        false -> retry(fun() -> wait_grant(Worker, Deadline) end, Deadline)
    end.
replacement(Sup, Old, Deadline) ->
    case supervisor:which_children(Sup) of
        [{_, P, worker, _}] when is_pid(P), P =/= Old -> P;
        _ -> retry(fun() -> replacement(Sup, Old, Deadline) end, Deadline)
    end.
kill(Pid) ->
    Ref = monitor(process, Pid), exit(Pid, kill),
    receive {'DOWN', Ref, process, Pid, killed} -> ok after 3000 -> error(not_killed) end.

upgrade(M, Bins) ->
    Gate = startup_metadata,
    case code:is_loaded(Gate) of
        false -> {module, Gate} = code:load_binary(Gate, "metadata", metadata_beam(Gate));
        _ -> ok
    end,
    All = Bins ++ [{atom_to_binary(Gate), metadata_beam(Gate)}],
    Manifest = #{base_vsn => <<"base">>, new_vsn => <<"next">>,
        modules => [Name || {Name, _} <- All],
        actors => [#{beam => atom_to_binary(Mod), renames => []} || Mod <- [M, Gate]]},
    ridge_loader:apply_bundle(1, iolist_to_binary(json:encode(Manifest)), All,
                             #{base_vsn => <<"base">>, purge_after_ms => 0}).

fake_purge(Observer) ->
    receive
        {purge_call, From, Ref, pause} ->
            Observer ! {purge_barrier, Ref},
            receive {release, Ref} -> From ! {Ref, ok} end,
            purge_open();
        _ -> error(unexpected_purge_request)
    end.
purge_open() ->
    receive
        {purge_call, From, Ref, {schedule, _, _}} -> From ! {Ref, #{pending => []}}, purge_open();
        {purge_call, From, Ref, _} -> From ! {Ref, ok}, purge_open()
    end.

metadata_beam(M) ->
    compile_source(M, lists:flatten(io_lib:format(
        "-module(~p). -export(['__ridge_state_fields'/0]). '__ridge_state_fields'()->case persistent_term:get(startup_metadata_barrier,false) of true->startup_observer!{metadata,self()},receive continue->ok end;false->ok end, [].", [M]))).

actor_beam(M, V) ->
    State = case V of 1 -> "#{count=>0}"; 2 -> "#{count=>0,step=>2}" end,
    Value = case V of 1 -> "maps:get(count,S)"; 2 -> "maps:get(count,S)+maps:get(step,S)" end,
    Fields = case V of 1 -> [count]; 2 -> [count,step] end,
    Source = lists:flatten(io_lib:format(
        "-module(~p). -export([start_link/0,init/1,'__ridge_init'/1,handle_call/3,handle_cast/2,code_change/3,'__ridge_state_fields'/0,'__ridge_state_defaults'/0,'__ridge_mailbox_config'/0]). start_link()->gen_server:start_link(~p,[],[]). init(A)->ridge_rt:actor_init(~p,A). '__ridge_init'(A)->reload_startup_probe:initialize(A,~s). handle_call({count},_,S)->{reply,~s,S}. handle_cast({tick},S)->{noreply,S#{count:=maps:get(count,S)+1}}. code_change(_,S,Extra)->ridge_rt:apply_code_change(S,Extra). '__ridge_state_fields'()->~p. '__ridge_state_defaults'()->#{step=>2}. '__ridge_mailbox_config'()->bounded.",
        [M,M,M,State,Value,Fields])),
    compile_source(M, Source).

initialize(Args, State) ->
    startup_observer ! {init_called, self()},
    Count = case Args of
        [block] -> startup_observer ! {initializing, self()}, receive continue -> 0 end;
        [{ask, P}] -> 7 = gen_server:call(P, {count}, 1000), 0;
        [{nested, M}] -> {ok, Child} = gen_server:start(M, [], []), 0 = value(Child), 0;
        [fail] -> error(expected_init_failure);
        [#{'__ridge_v' := {startup_seed, seed, 2}, count := C, step := 2}] -> C;
        [] -> 0
    end,
    {ok, State#{count := Count}}.

record_beam(V, HasEdge) ->
    Edge = case HasEdge of
        true -> "[{1,fun(S)->S#{'__ridge_v':={startup_seed,seed,2},step=>2}end}]";
        false -> "[]"
    end,
    compile_source(startup_seed, lists:flatten(io_lib:format(
        "-module(startup_seed). -export(['__ridge_record_versions'/0,'__ridge_record_migrations'/0]). '__ridge_record_versions'()->#{seed=>~p}. '__ridge_record_migrations'()->~s.",
        [V, Edge]))).

legacy_beam(M) ->
    compile_source(M, lists:flatten(io_lib:format(
        "-module(~p). -export([start_link/0,init/1,handle_call/3,handle_cast/2,'__ridge_state_fields'/0]). start_link()->gen_server:start_link(~p,[],[]). init([])->startup_observer!{legacy_init,self()},{ok,#{count=>0}}. handle_call({count},_,S)->{reply,maps:get(count,S),S}. handle_cast(_,S)->{noreply,S}. '__ridge_state_fields'()->[count].",
        [M,M]))).

compile_source(M, Source) ->
    {ok, Tokens, _} = erl_scan:string(Source),
    {ok, M, Bin} = compile:forms(forms(Tokens, [], []), [binary, return_errors]), Bin.
forms([], [], Acc) -> lists:reverse(Acc);
forms([{dot, _} = T | Rest], Current, Acc) ->
    {ok, F} = erl_parse:parse_form(lists:reverse([T | Current])), forms(Rest, [], [F | Acc]);
forms([T | Rest], Current, Acc) -> forms(Rest, [T | Current], Acc).
