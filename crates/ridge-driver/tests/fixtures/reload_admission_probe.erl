%% Expected semantics: a queued same-base request never changes code or
%% publishes its version; both transports share the target's admission.
-module(reload_admission_probe).
-export([run/1]).

run(Case) ->
    spawn(fun() -> receive after 15000 -> halt(2) end end),
    {module, ridge_rt} = code:ensure_loaded(ridge_rt),
    register(admission_observer, self()),
    persistent_term:put(ridge_loader_vsn, <<"base">>),
    test(Case),
    io:format("admission-probe-ok ~p~n", [Case]),
    halt(0).

test(Case) when Case =:= bundle_bundle; Case =:= bundle_dev;
                Case =:= dev_bundle; Case =:= dev_dev; Case =:= seed ->
    M = admission_toy,
    Blocking = "'__ridge_state_fields'()->admission_observer!{metadata,self()},receive continue->[] end.",
    {module, M} = code:load_binary(M, "old", actor_beam(M, 1, Blocking)),
    {ok, Actor} = gen_server:start(M, [], []),
    #{count := 7, generation := 1} = sys:get_state(Actor),
    B2 = actor_beam(M, 2, normal()), B3 = actor_beam(M, 3, normal()),
    Seed = Case =:= seed,
    case Seed of true -> persistent_term:erase(ridge_loader_vsn); false -> ok end,
    {First, Second} = case Case of
        bundle_dev -> {bundle, dev}; dev_bundle -> {dev, bundle};
        dev_dev -> {dev, dev}; _ -> {bundle, bundle}
    end,
    ok = file:write_file("admission_toy.beam", B2),
    ok = file:write_file("first.json", manifest(<<"base">>, <<"two">>)),
    ok = file:write_file("second.json", manifest(<<"base">>, <<"three">>)),
    P1 = launch(fun() -> apply_payload(First, "first.json", <<"base">>, <<"two">>, B2, Seed) end),
    Server = receive {metadata, S} -> S after 3000 -> error(no_first_admission) end,
    Server = whereis(ridge_loader_admission),
    P2 = launch(fun() -> apply_payload(Second, "second.json", <<"base">>, <<"three">>, B3, Seed) end),
    queued(Server, P2, erlang:monotonic_time(millisecond) + 3000),
    receive {metadata, _} -> error(overlapping_admission);
            {done, P2, _} -> error(premature_second_result)
    after 0 -> ok end,
    Server ! continue,
    {ok, _} = result(P1),
    {error, {base_version_mismatch, <<"base">>, <<"two">>}} = result(P2),
    2 = M:value(), <<"two">> = ridge_loader:current_version(),
    #{count := 7, generation := 2} = sys:get_state(Actor),
    {ok, _} = apply_payload(bundle, unused, <<"two">>, <<"three">>, B3, false),
    3 = M:value(), <<"three">> = ridge_loader:current_version(),
    #{count := 7, generation := 3} = sys:get_state(Actor);
test(rejection) ->
    {error, {reload_rejected, preflight, _}} = ridge_loader:apply_bundle(
        <<"{}">>, [], #{base_vsn => <<"base">>}),
    valid_after_failure();
test(exception) ->
    {error, {upgrade_failed, _, _, _}} = ridge_loader:apply("absent.json", <<"base">>),
    valid_after_failure();
test(seed_rejection) ->
    persistent_term:erase(ridge_loader_vsn),
    {error, {reload_rejected, preflight, _}} = ridge_loader:apply_bundle(
        <<"{}">>, [], #{base_vsn => <<"base">>, seed => true}),
    undefined = ridge_loader:current_version(),
    M = admission_toy,
    {module, M} = code:load_binary(M, "old", beam(M, 1, normal())),
    {ok, _} = apply_payload(bundle, unused, <<"base">>, <<"two">>, beam(M, 2, normal()), true),
    2 = M:value(), <<"two">> = ridge_loader:current_version();
test(Case) when Case =:= dead_queued_caller; Case =:= death_before_dispatch;
                Case =:= diagnostic_payload; Case =:= diagnostic_reason ->
    M = admission_toy,
    {module, M} = code:load_binary(M, "old", beam(M, 1, normal())),
    B2 = beam(M, 2, normal()), B3 = beam(M, 3, normal()),
    {error, {base_version_mismatch, _, _}} = ridge_loader:apply("unused", <<"wrong">>),
    Server = whereis(ridge_loader_admission),
    ok = sys:suspend(Server),
    P1 = launch(fun() -> apply_payload(bundle, unused, <<"base">>, <<"two">>, B2, false) end),
    queued(Server, P1, deadline()),
    case Case of
        dead_queued_caller ->
            kill_and_wait(P1),
            P2 = launch(fun() -> apply_payload(bundle, unused, <<"base">>, <<"three">>, B3, false) end),
            queued(Server, P2, deadline()),
            ok = sys:resume(Server),
            {ok, _} = result(P2);
        C when C =:= death_before_dispatch; C =:= diagnostic_payload;
               C =:= diagnostic_reason ->
            case Case of
                diagnostic_reason -> exit_and_wait(Server, {private_failure, B2});
                _ -> kill_and_wait(Server)
            end,
            Failure = result(P1),
            {error, {upgrade_failed, _, _, _}} = Failure,
            case Case of
                D when D =:= diagnostic_payload; D =:= diagnostic_reason ->
                    Encoded = term_to_binary(Failure),
                    nomatch = binary:match(Encoded, B2),
                    true = byte_size(Encoded) < 512,
                    Expected = case D of diagnostic_payload -> killed;
                                         diagnostic_reason -> coordinator_failed end,
                    {error, {upgrade_failed, exit, {reload_admission_down, Expected}, []}} = Failure;
                _ -> ok
            end,
            1 = M:value(), <<"base">> = ridge_loader:current_version(),
            {ok, _} = apply_payload(bundle, unused, <<"base">>, <<"three">>, B3, false)
    end,
    3 = M:value(), <<"three">> = ridge_loader:current_version();
test(Case) when Case =:= admitted_caller_death; Case =:= rpc_timeout;
                Case =:= active_worker_death; Case =:= active_exception ->
    M = admission_toy,
    Blocking = "'__ridge_state_fields'()->admission_observer!{metadata,self()},receive continue->[];fail->error(metadata_failed) end.",
    {module, M} = code:load_binary(M, "old", beam(M, 1, Blocking)),
    B2 = beam(M, 2, normal()), B3 = beam(M, 3, normal()),
    %% Bootstrap admission before timing the RPC: compiler/startup latency
    %% cannot make a timeout pass without reaching the metadata barrier.
    case Case of
        rpc_timeout ->
            {error, {base_version_mismatch, _, _}} = ridge_loader:apply("unused", <<"wrong">>);
        _ -> ok
    end,
    P1 = launch(fun() ->
        case Case of
            rpc_timeout -> rpc:call(node(), ridge_loader, apply_bundle,
                [manifest(<<"base">>, <<"two">>), [{<<"admission_toy">>, B2}],
                 #{base_vsn => <<"base">>, purge_after_ms => 0}], 2000);
            _ -> apply_payload(bundle, unused, <<"base">>, <<"two">>, B2, false)
        end
    end),
    Server = receive {metadata, S} -> S after 3000 -> error(no_admission) end,
    case Case of
        admitted_caller_death -> kill_and_wait(P1), Server ! continue;
        rpc_timeout -> {badrpc, timeout} = result(P1), Server ! continue;
        active_worker_death ->
            kill_and_wait(Server),
            {error, {upgrade_failed, _, _, _}} = result(P1);
        active_exception ->
            Server ! fail,
            {error, {upgrade_failed, error, metadata_failed, _}} = result(P1)
    end,
    case Case of
        C when C =:= active_worker_death; C =:= active_exception ->
            wait_no_recovery(deadline()),
            1 = M:value(), <<"base">> = ridge_loader:current_version(),
            {ok, _} = followup(<<"base">>, B3, metadata),
            3 = M:value(), <<"three">> = ridge_loader:current_version();
        _ ->
            wait_version(<<"two">>, deadline()),
            2 = M:value(),
            {ok, _} = apply_payload(bundle, unused, <<"two">>, <<"three">>, B3, false),
            3 = M:value()
    end;
test(reentrant) ->
    M = admission_toy,
    Metadata = "'__ridge_state_fields'()->{error,{reload_rejected,admission,reentrant}}=ridge_loader:apply_bundle(<<\"{}\">>,[],#{}),[].",
    {module, M} = code:load_binary(M, "old", beam(M, 1, Metadata)),
    {ok, _} = apply_payload(bundle, unused, <<"base">>, <<"two">>, beam(M, 2, normal()), false),
    2 = M:value(), <<"two">> = ridge_loader:current_version();
test(Case) when Case =:= postload_death; Case =:= postload_exception ->
    M = admission_toy,
    {module, M} = code:load_binary(M, "old", actor_beam(M, 1, normal())),
    {ok, Actor} = gen_server:start(M, [], []),
    ActorRef = monitor(process, Actor),
    Metadata = "'__ridge_state_fields'()->admission_observer!{postload,self()},receive continue->[];fail->error(postload_failed) end.",
    B2 = actor_beam(M, 2, Metadata), B3 = actor_beam(M, 3, normal()),
    P1 = launch(fun() -> apply_payload(bundle, unused, <<"base">>, <<"two">>, B2, false) end),
    Server = receive {postload, S} -> S after 3000 -> error(no_postload_barrier) end,
    %% The mismatch is observable before the failure, so this cannot pass
    %% by killing a worker before it actually loaded code.
    2 = M:value(), <<"base">> = ridge_loader:current_version(),
    P2 = launch(fun() -> apply_payload(bundle, unused, <<"base">>, <<"three">>, B3, false) end),
    queued(Server, P2, deadline()),
    case Case of
        postload_death -> kill_and_wait(Server);
        postload_exception -> Server ! fail
    end,
    {error, {upgrade_failed, _, _, _}} = result(P1),
    case Case of
        postload_death -> {error, {upgrade_failed, _, _, _}} = result(P2);
        postload_exception ->
            {error, {base_version_mismatch, <<"base">>, <<"two">>}} = result(P2)
    end,
    wait_no_recovery(deadline()),
    receive {'DOWN', ActorRef, process, Actor, killed} -> ok
    after 3000 -> error(unsafe_actor_survived) end,
    {error, {base_version_mismatch, <<"base">>, <<"two">>}} =
        apply_payload(bundle, unused, <<"base">>, <<"three">>, B3, false),
    2 = M:value(), <<"two">> = ridge_loader:current_version(),
    {ok, _} = followup(<<"two">>, B3, postload),
    3 = M:value(), <<"three">> = ridge_loader:current_version();
test(worker_death) ->
    %% An idle worker can die without poisoning subsequent admission.
    {error, {base_version_mismatch, _, _}} = ridge_loader:apply("unused", <<"wrong">>),
    Server = whereis(ridge_loader_admission),
    Ref = monitor(process, Server), exit(Server, kill),
    receive {'DOWN', Ref, process, Server, killed} -> ok end,
    valid_after_failure().

valid_after_failure() ->
    M = admission_toy,
    {module, M} = code:load_binary(M, "old", beam(M, 1, normal())),
    {ok, _} = apply_payload(bundle, unused, <<"base">>, <<"two">>, beam(M, 2, normal()), false),
    2 = M:value(), <<"two">> = ridge_loader:current_version().

apply_payload(dev, Path, Base, _, _, _) -> ridge_loader:apply(Path, Base);
apply_payload(bundle, _, Base, Next, Bin, Seed) ->
    ridge_loader:apply_bundle(manifest(Base, Next), [{<<"admission_toy">>, Bin}],
        #{base_vsn => Base, seed => Seed, purge_after_ms => 0}).

manifest(Base, Next) ->
    iolist_to_binary(json:encode(#{base_vsn => Base, new_vsn => Next,
        modules => [<<"admission_toy">>],
        actors => [#{beam => <<"admission_toy">>, renames => []}]})).

launch(Fun) ->
    Parent = self(), spawn(fun() -> Parent ! {done, self(), Fun()} end).

result(Pid) -> receive {done, Pid, R} -> R after 4000 -> error({missing_result, Pid}) end.

followup(Base, Bin, Tag) ->
    Pid = launch(fun() -> apply_payload(bundle, unused, Base, <<"three">>, Bin, false) end),
    receive {Tag, Worker} -> Worker ! continue after 3000 -> error(no_followup_barrier) end,
    result(Pid).

wait_no_recovery(Deadline) ->
    case whereis(ridge_loader_recovery) of
        undefined -> ok;
        _ ->
            case erlang:monotonic_time(millisecond) < Deadline of
                true -> receive after 1 -> wait_no_recovery(Deadline) end;
                false -> error(recovery_not_released)
            end
    end.

deadline() -> erlang:monotonic_time(millisecond) + 3000.

kill_and_wait(Pid) ->
    exit_and_wait(Pid, killed).

exit_and_wait(Pid, Reason) ->
    Ref = monitor(process, Pid),
    Signal = case Reason of killed -> kill; _ -> Reason end,
    exit(Pid, Signal),
    receive {'DOWN', Ref, process, Pid, Reason} -> ok
    after 3000 -> error({process_stuck, Pid}) end.

wait_version(Version, Deadline) ->
    case ridge_loader:current_version() of
        Version -> ok;
        Other ->
            case erlang:monotonic_time(millisecond) < Deadline of
                true -> receive after 1 -> wait_version(Version, Deadline) end;
                false -> error({version_stuck, Other})
            end
    end.

queued(Server, Caller, Deadline) ->
    {messages, Messages} = process_info(Server, messages),
    case lists:any(fun
        ({'$gen_call', {P, _}, _}) when P =:= Caller -> true;
        (_) -> false
    end, Messages) of
        true -> ok;
        false ->
            case erlang:monotonic_time(millisecond) < Deadline of
                true -> receive after 1 -> queued(Server, Caller, Deadline) end;
                false -> error(second_caller_never_queued)
            end
    end.

normal() -> "'__ridge_state_fields'()->[].".
beam(M, V, Metadata) ->
    Source = lists:flatten(io_lib:format(
        "-module(~p). -export([value/0,'__ridge_state_fields'/0]). value()->~p. ~s",
        [M, V, Metadata])),
    compile_source(M, Source).

actor_beam(M, V, Metadata) ->
    Source = lists:flatten(io_lib:format(
        "-module(~p). -export([value/0,init/1,handle_call/3,handle_cast/2,code_change/3,'__ridge_state_fields'/0,'__ridge_state_defaults'/0]). value()->~p. init([])->{ok,#{count=>7,generation=>1}}. handle_call(get,_,S)->{reply,S,S}. handle_cast(_,S)->{noreply,S}. code_change(_,S,ridge_refresh_collections)->{ok,S};code_change(_,S,_)->{ok,S#{generation=>~p}}. '__ridge_state_defaults'()->#{}. ~s",
        [M, V, V, Metadata])),
    compile_source(M, Source).

compile_source(M, Source) ->
    {ok, Tokens, _} = erl_scan:string(Source),
    {ok, M, Bin} = compile:forms(forms(Tokens, [], []), [binary]), Bin.

forms([], [], Acc) -> lists:reverse(Acc);
forms([{dot, _} = T | Rest], Current, Acc) ->
    {ok, F} = erl_parse:parse_form(lists:reverse([T | Current])),
    forms(Rest, [], [F | Acc]);
forms([T | Rest], Current, Acc) -> forms(Rest, [T | Current], Acc).
