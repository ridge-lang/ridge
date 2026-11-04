%% Failure barriers are controlled metadata/callbacks, not timing sleeps.
%% Every case runs in a fresh VM against the actual loader/runtime sources.
-module(reload_recovery_probe).
-export([run/1, run_generated/4, init/1]).

%% The gate actor supplies a deterministic barrier while Counter keeps its
%% actual compiler-generated callbacks, state layout and startup path.
run_generated(OldDir, NewDir, ActorModule, Case) ->
    spawn(fun() -> receive after 20000 -> halt(2) end end),
    register(recovery_observer, self()),
    {module, ridge_loader} = code:ensure_loaded(ridge_loader),
    ProbeDir = filename:dirname(code:which(?MODULE)),
    ProbeDir = filename:dirname(code:which(ridge_loader)),
    true = code:add_patha(OldDir),
    {module, ridge_rt} = code:ensure_loaded(ridge_rt),
    {module, ActorModule} = code:ensure_loaded(ActorModule),
    {ok, Sup} = supervisor:start_link(?MODULE, {supervise, ActorModule}),
    Actor = child(Sup),
    Handle = {ridge_handle, Actor, unbounded},
    [ridge_rt:send_op(Handle, {tick}) || _ <- lists:seq(1, 7)],
    7 = ridge_rt:ask(Handle, {count}, 500),
    false = maps:is_key(step, sys:get_state(Actor)),
    Gate = recovery_gate,
    {module, Gate} = code:load_binary(Gate, "old", beam(Gate, 1)),
    {ok, GatePid} = gen_server:start(Gate, [], []),
    %% Strip the exact generated suffix, rather than trimming its characters.
    ParentName = lists:sublist(atom_to_list(ActorModule), length(atom_to_list(ActorModule)) - 8),
    Modules = [list_to_atom(ParentName), ActorModule, Gate],
    Bins = [{atom_to_binary(M), case M of
                Gate -> beam(Gate, 2);
                _ -> {ok, B} = file:read_file(filename:join(NewDir, atom_to_list(M) ++ ".beam")), B
            end} || M <- Modules],
    Mf = #{base_vsn => <<"base">>, new_vsn => <<"two">>,
           modules => [atom_to_binary(M) || M <- Modules],
           actors => [#{beam => atom_to_binary(M), renames => []} || M <- [ActorModule, Gate]]},
    Json = iolist_to_binary(json:encode(Mf)),
    persistent_term:put(ridge_loader_vsn, <<"base">>),
    Block = case Case of precommit -> before_load; postload -> after_load end,
    persistent_term:put(recovery_pause, Block),
    Caller = launch(fun() -> ridge_loader:apply_bundle(Json, Bins,
                       #{base_vsn => <<"base">>, purge_after_ms => 0}) end),
    Worker = receive {Block, P} -> P after 3000 -> error(no_generated_barrier) end,
    Worker = whereis(ridge_loader_admission),
    {'EXIT', {timeout, _}} = catch gen_server:call(Actor, {count}, 30),
    kill(Worker), {error, {upgrade_failed, _, _, _}} = result(Caller),
    persistent_term:put(recovery_pause, false), wait_recovery(deadline()),
    case Case of
        precommit ->
            Actor = child(Sup), 7 = ridge_rt:ask(Handle, {count}, 500),
            false = maps:is_key(step, sys:get_state(Actor)),
            <<"base">> = ridge_loader:current_version(),
            {ok, _} = ridge_loader:apply_bundle(Json, Bins,
                            #{base_vsn => <<"base">>, purge_after_ms => 0}),
            7 = ridge_rt:ask(Handle, {count}, 500),
            #{count := 7, step := 2} = sys:get_state(Actor);
        postload ->
            Fresh = replacement(Sup, Actor, deadline()),
            0 = ridge_rt:ask({ridge_handle, Fresh, unbounded}, {count}, 500),
            #{count := 0, step := 2} = sys:get_state(Fresh),
            false = is_process_alive(GatePid),
            <<"two">> = ridge_loader:current_version()
    end,
    io:format("generated-recovery-ok ~p~n", [Case]), halt(0).

run(Case) ->
    spawn(fun() -> receive after 20000 -> halt(2) end end),
    {module, ridge_rt} = code:ensure_loaded(ridge_rt),
    register(recovery_observer, self()),
    persistent_term:put(ridge_loader_vsn, <<"base">>),
    Actual = case Case of
        precommit_dev -> persistent_term:put(recovery_transport, dev), precommit;
        postload_dev -> persistent_term:put(recovery_transport, dev), postload;
        _ -> Case
    end,
    test(Actual),
    io:format("recovery-probe-ok ~p~n", [Case]), halt(0).

test(Case) when Case =:= precommit; Case =:= postload;
                Case =:= postload_exception; Case =:= owner_death ->
    M = recovery_actor,
    {module, M} = code:load_binary(M, "old", beam(M, 1)),
    {ok, Actor} = gen_server:start(M, [], []),
    ok = gen_server:call(Actor, {set, 7}),
    7 = gen_server:call(Actor, get),
    Block = case Case of postload -> after_load; postload_exception -> after_load;
                         _ -> before_load end,
    persistent_term:put(recovery_pause, Block),
    B2 = beam(M, 2), B3 = beam(M, 3),
    Caller = launch(fun() -> upgrade(M, B2, <<"base">>, <<"two">>) end),
    Worker = receive {Block, P} -> P after 3000 -> error(no_barrier) end,
    Worker = whereis(ridge_loader_admission),
    case Block of before_load -> 1 = M:value(); after_load -> 2 = M:value() end,
    <<"base">> = ridge_loader:current_version(),
    true = blocked(Actor),
    Ref = monitor(process, Actor),
    case Case of
        owner_death -> kill(whereis(ridge_loader_recovery)), Worker ! continue;
        postload_exception -> Worker ! fail;
        _ -> kill(Worker)
    end,
    {error, {upgrade_failed, _, _, _}} = result(Caller),
    persistent_term:put(recovery_pause, false),
    wait_recovery(deadline()),
    case Case of
        precommit ->
            true = is_process_alive(Actor),
            7 = gen_server:call(Actor, get, 500),
            <<"base">> = ridge_loader:current_version(),
            {ok, _} = upgrade(M, B3, <<"base">>, <<"three">>),
            7 = gen_server:call(Actor, get, 500);
        owner_death ->
            receive {'DOWN', Ref, process, Actor, killed} -> ok after 3000 -> error(actor_stranded) end,
            1 = M:value(), <<"base">> = ridge_loader:current_version(),
            {error, {reload_recovery_required, restart_node}} =
                upgrade(M, B3, <<"base">>, <<"three">>);
        _ ->
            receive {'DOWN', Ref, process, Actor, killed} -> ok after 3000 -> error(actor_stranded) end,
            2 = M:value(), <<"two">> = ridge_loader:current_version(),
            {ok, Fresh} = gen_server:start(M, [], []),
            0 = gen_server:call(Fresh, get, 500),
            {ok, _} = upgrade(M, B3, <<"two">>, <<"three">>),
            0 = gen_server:call(Fresh, get, 500)
    end;
test(partial_ready) ->
    M = recovery_actor,
    {module, M} = code:load_binary(M, "old", beam(M, 1)),
    {ok, A} = gen_server:start(M, [], []), {ok, B} = gen_server:start(M, [], []),
    ok = gen_server:call(A, {set, 11}), ok = gen_server:call(B, {set, 22}),
    Counts = #{A => 11, B => 22}, B2 = beam(M, 2),
    persistent_term:put(recovery_pause, refresh),
    Caller = launch(fun() -> upgrade(M, B2, <<"base">>, <<"two">>) end),
    First = receive {refresh, P} -> P after 3000 -> error(no_first_refresh) end,
    true = maps:is_key(First, Counts), First ! continue,
    Second = receive {refresh, P2} -> P2 after 3000 -> error(no_second_refresh) end,
    true = maps:is_key(Second, Counts), true = First =/= Second,
    true = blocked(First), true = blocked(Second),
    Ref = monitor(process, Second), kill(whereis(ridge_loader_admission)),
    {error, {upgrade_failed, _, _, _}} = result(Caller),
    wait_recovery(deadline()),
    Expected = maps:get(First, Counts), Expected = gen_server:call(First, get, 500),
    receive {'DOWN', Ref, process, Second, killed} -> ok after 3000 -> error(second_stranded) end,
    <<"two">> = ridge_loader:current_version(),
    persistent_term:put(recovery_pause, false),
    {ok, _} = upgrade(M, beam(M, 3), <<"two">>, <<"three">>),
    Expected = gen_server:call(First, get, 500);
test(Case) when Case =:= busy_recovery; Case =:= unknown_commit;
                Case =:= finishing ->
    M = recovery_actor,
    {module, M} = code:load_binary(M, "old", beam(M, 1)),
    {ok, Actor} = gen_server:start(M, [], []),
    ok = gen_server:call(Actor, {set, 7}),
    Mode = case Case of finishing -> schedule; _ -> pause end,
    Observer = self(),
    Fake = spawn(fun() -> fake_purge(Observer, Mode) end),
    true = register(ridge_loader_purge, Fake),
    B2 = beam(M, 2), B3 = beam(M, 3),
    Caller = launch(fun() -> upgrade(M, B2, <<"base">>, <<"two">>) end),
    {Owner, Grant} = receive {purge_barrier, P, R} -> {P, R}
                     after 3000 -> error(no_purge_barrier) end,
    Owner = whereis(ridge_loader_recovery),
    Worker = whereis(ridge_loader_admission),
    ActorRef = monitor(process, Actor),
    case Case of
        finishing -> 7 = gen_server:call(Actor, get, 500),
                     <<"two">> = ridge_loader:current_version();
        unknown_commit ->
            true = erlang:suspend_process(Worker),
            Fake ! {release, Grant},
            wait_grant(Worker, deadline()),
            1 = M:value(), <<"base">> = ridge_loader:current_version();
        busy_recovery -> true = blocked(Actor)
    end,
    kill(Worker),
    {error, {upgrade_failed, _, _, _}} = result(Caller),
    case Case of
        unknown_commit -> ok;
        _ ->
            {error, {reload_rejected, admission, recovery_in_progress}} =
                upgrade(M, B3, <<"base">>, <<"three">>),
            mutating = persistent_term:get(ridge_loader_active_reload),
            Fake ! {release, Grant}
    end,
    wait_recovery(deadline()),
    case Case of
        unknown_commit ->
            receive {'DOWN', ActorRef, process, Actor, killed} -> ok
            after 3000 -> error(unsafe_actor_survived) end,
            1 = M:value(), <<"base">> = ridge_loader:current_version(),
            {error, {reload_recovery_required, restart_node}} =
                upgrade(M, B3, <<"base">>, <<"three">>);
        _ ->
            7 = gen_server:call(Actor, get, 500),
            Base = case Case of finishing -> <<"two">>; _ -> <<"base">> end,
            Base = ridge_loader:current_version(),
            {ok, _} = upgrade(M, B3, Base, <<"three">>),
            7 = gen_server:call(Actor, get, 500)
    end;
test(suspend_intent) ->
    M = recovery_actor,
    {module, M} = code:load_binary(M, "old", beam(M, 1)),
    {ok, Actor} = gen_server:start(M, [], []),
    _ = launch(fun() -> catch gen_server:call(Actor, block, infinity) end),
    receive {blocked_callback, Actor} -> ok after 3000 -> error(no_callback) end,
    B2 = beam(M, 2),
    Caller = launch(fun() -> upgrade(M, B2, <<"base">>, <<"two">>) end),
    wait_suspend_intent(Actor, deadline()),
    true = blocked(Actor), Ref = monitor(process, Actor),
    kill(whereis(ridge_loader_admission)),
    {error, {upgrade_failed, _, _, _}} = result(Caller),
    wait_recovery(deadline()),
    receive {'DOWN', Ref, process, Actor, killed} -> ok after 1000 -> error(late_suspend_survived) end,
    1 = M:value(), <<"base">> = ridge_loader:current_version(),
    {ok, Fresh} = gen_server:start(M, [], []),
    {ok, _} = upgrade(M, beam(M, 3), <<"base">>, <<"three">>),
    0 = gen_server:call(Fresh, get, 500);
test(Case) when Case =:= supervised; Case =:= actor_died ->
    M = recovery_actor,
    {module, M} = code:load_binary(M, "old", beam(M, 1)),
    {ok, Sup} = supervisor:start_link(?MODULE, {supervise, M}),
    Actor = child(Sup), ok = gen_server:call(Actor, {set, 7}),
    Block = case Case of supervised -> after_load; actor_died -> before_load end,
    persistent_term:put(recovery_pause, Block),
    B2 = beam(M, 2),
    Caller = launch(fun() -> upgrade(M, B2, <<"base">>, <<"two">>) end),
    Worker = receive {Block, P} -> P after 3000 -> error(no_barrier) end,
    case Case of
        supervised -> kill(Worker);
        actor_died -> kill(Actor), _ = replacement(Sup, Actor, deadline()), Worker ! continue
    end,
    {error, {upgrade_failed, _, _, _}} = result(Caller),
    persistent_term:put(recovery_pause, false),
    wait_recovery(deadline()),
    Fresh = replacement(Sup, Actor, deadline()),
    0 = gen_server:call(Fresh, get, 500),
    Base = case Case of supervised -> <<"two">>; actor_died -> <<"base">> end,
    V = case Case of supervised -> 2; actor_died -> 1 end,
    V = M:value(), Base = ridge_loader:current_version(),
    {ok, _} = upgrade(M, beam(M, 3), Base, <<"three">>),
    Fresh = child(Sup), 0 = gen_server:call(Fresh, get, 500).

child(Sup) -> [{actor, Pid, worker, _}] = supervisor:which_children(Sup), Pid.
wait_suspend_intent(Actor, Deadline) ->
    Owner = whereis(ridge_loader_recovery),
    {messages, Messages} = process_info(Actor, messages),
    case lists:any(fun({system, {P, _}, suspend}) -> P =:= Owner; (_) -> false end, Messages) of
        true -> ok;
        false -> retry(fun() -> wait_suspend_intent(Actor, Deadline) end, Deadline)
    end.
replacement(Sup, Old, Deadline) ->
    case child(Sup) of
        P when is_pid(P), P =/= Old -> P;
        _ -> retry(fun() -> replacement(Sup, Old, Deadline) end, Deadline)
    end.
wait_grant(Worker, Deadline) ->
    {messages, Messages} = process_info(Worker, messages),
    case lists:any(fun({R, ok}) when is_reference(R) -> true; (_) -> false end, Messages) of
        true -> ok;
        false -> retry(fun() -> wait_grant(Worker, Deadline) end, Deadline)
    end.
retry(Fun, Deadline) ->
    case erlang:monotonic_time(millisecond) < Deadline of
        true -> receive after 1 -> Fun() end;
        false -> error(barrier_timeout)
    end.
fake_purge(Observer, Mode) ->
    receive
        {purge_call, From, Ref, Request} ->
            Kind = case Request of {schedule, _, _} -> schedule; _ -> Request end,
            Next = case Kind =:= Mode of
                true -> Observer ! {purge_barrier, From, Ref},
                        receive {release, Ref} -> ok end, none;
                false -> Mode
            end,
            Reply = case Kind of schedule -> #{pending => []}; status -> #{pending => []}; _ -> ok end,
            From ! {Ref, Reply}, fake_purge(Observer, Next)
    end.

blocked(Pid) ->
    case catch gen_server:call(Pid, get, 30) of
        {'EXIT', {timeout, _}} -> true;
        _ -> false
    end.

upgrade(M, Bin, Base, Next) ->
    Mf = #{base_vsn => Base, new_vsn => Next, modules => [atom_to_binary(M)],
           actors => [#{beam => atom_to_binary(M), renames => []}]},
    Json = iolist_to_binary(json:encode(Mf)),
    case persistent_term:get(recovery_transport, bundle) of
        bundle -> ridge_loader:apply_bundle(Json,
            [{atom_to_binary(M), Bin}], #{base_vsn => Base, purge_after_ms => 0});
        dev ->
            ok = file:write_file(atom_to_list(M) ++ ".beam", Bin),
            ok = file:write_file("recovery-manifest.json", Json),
            ridge_loader:apply("recovery-manifest.json", Base)
    end.

launch(Fun) -> Parent = self(), spawn(fun() -> Parent ! {done, self(), Fun()} end).
result(Pid) -> receive {done, Pid, R} -> R after 10000 -> error(missing_result) end.
deadline() -> erlang:monotonic_time(millisecond) + 8000.
wait_recovery(Deadline) ->
    case whereis(ridge_loader_recovery) of
        undefined -> ok;
        _ ->
            case erlang:monotonic_time(millisecond) < Deadline of
                true -> receive after 1 -> wait_recovery(Deadline) end;
                false -> error(recovery_stranded)
            end
    end.
kill(Pid) ->
    Ref = monitor(process, Pid), exit(Pid, kill),
    receive {'DOWN', Ref, process, Pid, killed} -> ok after 3000 -> error(worker_stranded) end.

%% Used by supervised cases and generated Ridge actor coverage.
init({supervise, Module}) ->
    {ok, {#{strategy => one_for_one, intensity => 10, period => 5},
          [#{id => actor, start => {Module, start_link, []}, restart => permanent,
             shutdown => 5000, type => worker}]}}.

beam(M, V) ->
    Fields = case V of 1 -> [count, generation]; _ -> [count, generation, step] end,
    Phase = case V of 1 -> before_load; _ -> after_load end,
    InitState = case V of 1 -> "#{count=>0,generation=>1}";
                         _ -> lists:flatten(io_lib:format("#{count=>0,generation=>~p,step=>~p}", [V,V])) end,
    Source = lists:flatten(io_lib:format(
        "-module(~p). -export([start_link/0,value/0,init/1,handle_call/3,handle_cast/2,code_change/3,'__ridge_state_fields'/0,'__ridge_state_defaults'/0]). start_link()->gen_server:start_link(~p,[],[]). value()->~p. init([])->{ok,~s}. handle_call(block,_,S)->recovery_observer!{blocked_callback,self()},receive continue->ok end,{reply,ok,S};handle_call(get,_,S)->{reply,maps:get(count,S),S};handle_call({set,C},_,S)->{reply,ok,S#{count=>C}}. handle_cast(_,S)->{noreply,S}. code_change(_,S,ridge_refresh_collections)->case persistent_term:get(recovery_pause,false)of refresh->recovery_observer!{refresh,self()},receive continue->ok end;_->ok end,{ok,S};code_change(_,S,_)->{ok,S#{generation=>~p,step=>~p}}. '__ridge_state_defaults'()->#{step=>~p}. '__ridge_state_fields'()->case persistent_term:get(recovery_pause,false)of ~p->recovery_observer!{~p,self()},receive continue->ok;fail->error(metadata_failed)end;_->ok end,~p.",
        [M,M,V,InitState,V,V,V,Phase,Phase,Fields])),
    {ok, Tokens, _} = erl_scan:string(Source),
    {ok, M, Bin} = compile:forms(forms(Tokens, [], []), [binary]), Bin.

forms([], [], Acc) -> lists:reverse(Acc);
forms([{dot, _} = T | Rest], Current, Acc) ->
    {ok, F} = erl_parse:parse_form(lists:reverse([T | Current])), forms(Rest, [], [F | Acc]);
forms([T | Rest], Current, Acc) -> forms(Rest, [T | Current], Acc).
