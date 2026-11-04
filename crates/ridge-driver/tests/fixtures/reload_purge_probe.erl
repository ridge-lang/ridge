%% Each case runs in a fresh VM. Holders execute local old-code receives.
-module(reload_purge_probe).
-export([run/1]).

run(Case) ->
    spawn(fun() -> receive after 15000 -> halt(2) end end),
    {module, ridge_rt} = code:ensure_loaded(ridge_rt),
    persistent_term:put(ridge_loader_vsn, <<"base">>),
    test(Case),
    io:format("purge-probe-ok ~p~n", [Case]),
    halt().

test(holder) ->
    M = ridge_purge_held, load(M, 1),
    {P, Ref} = holder(M),
    {ok, #{purge := #{scheduled := true}}} = upgrade([M], 2, 20),
    receive {'DOWN', Ref, process, P, Why} -> error({holder_killed, Why})
    after 100 -> ok end,
    true = is_process_alive(P), 2 = M:value(),
    true = erlang:check_old_code(M),
    wait(fun() -> pending(M) end),
    P ! stop;
test(no_holder) ->
    M = ridge_purge_free, load(M, 1),
    {ok, _} = upgrade([M], 2, 20),
    wait(fun() -> not erlang:check_old_code(M) end),
    #{waiting := [], pending := []} = ridge_loader:purge_status(),
    2 = M:value();
test(disabled) ->
    M = ridge_purge_disabled, load(M, 1),
    {P, Ref} = holder(M),
    {ok, #{purge := #{scheduled := false, after_ms := 0}}} = upgrade([M], 2, 0),
    receive {'DOWN', Ref, process, P, Why} -> error({holder_killed, Why})
    after 100 -> ok end,
    true = erlang:check_old_code(M),
    #{waiting := [], pending := []} = ridge_loader:purge_status(),
    P ! stop;
test(second_upgrade) ->
    M = ridge_purge_second, load(M, 1),
    {P, Ref} = holder(M),
    {ok, _} = upgrade([M], 2, 20),
    wait(fun() -> pending(M) end),
    {error, {reload_rejected, load, [{M, not_purged}]}} = upgrade([M], 3, 20),
    true = is_process_alive(P), 2 = M:value(),
    <<"v2">> = ridge_loader:current_version(),
    P ! stop,
    receive {'DOWN', Ref, process, P, normal} -> ok after 2000 -> error(holder_stuck) end,
    {ok, _} = upgrade([M], 3, 20),
    wait(fun() -> not erlang:check_old_code(M) end),
    3 = M:value(), <<"v3">> = ridge_loader:current_version(),
    #{waiting := [], pending := []} = ridge_loader:purge_status();
test(disjoint_deadlines) ->
    Pause = pause_cleanup(),
    A = ridge_purge_early, B = ridge_purge_late, load(A, 1), load(B, 1),
    {ok, _} = upgrade([A], 2, 40),
    waiting_while_paused([A]),
    {ok, _} = upgrade([B], 3, 60000),
    waiting_while_paused([A, B]),
    resume_cleanup(Pause),
    wait(fun() -> not erlang:check_old_code(A) end),
    true = erlang:check_old_code(B),
    #{waiting := [<<"ridge_purge_late">>], pending := []} = ridge_loader:purge_status();
test(disabled_preserves_other) ->
    Pause = pause_cleanup(),
    A = ridge_purge_enabled, B = ridge_purge_off, load(A, 1), load(B, 1),
    {ok, _} = upgrade([A], 2, 40),
    waiting_while_paused([A]),
    {ok, _} = upgrade([B], 3, 0),
    waiting_while_paused([A]),
    resume_cleanup(Pause),
    wait(fun() -> not erlang:check_old_code(A) end),
    true = erlang:check_old_code(B),
    #{waiting := [], pending := []} = ridge_loader:purge_status();
test(pending_retried) ->
    A = ridge_purge_retry, B = ridge_purge_trigger, load(A, 1), load(B, 1),
    {P, Ref} = holder(A),
    {ok, _} = upgrade([A], 2, 20),
    wait(fun() -> pending(A) end),
    P ! stop,
    receive {'DOWN', Ref, process, P, normal} -> ok after 2000 -> error(holder_stuck) end,
    true = erlang:check_old_code(A),
    {ok, _} = upgrade([B], 3, 20),
    wait(fun() -> not erlang:check_old_code(A) andalso not erlang:check_old_code(B) end),
    #{waiting := [], pending := []} = ridge_loader:purge_status();
test(rejection_preserves_other) ->
    Pause = pause_cleanup(),
    A = ridge_purge_reject_other, B = ridge_purge_reject_held,
    load(A, 1), load(B, 1), {P, _} = holder(B), load(B, 2),
    {ok, _} = upgrade([A], 2, 80),
    waiting_while_paused([A]),
    {error, {reload_rejected, load, [{B, not_purged}]}} = upgrade([B], 3, 20),
    waiting_while_paused([A]),
    resume_cleanup(Pause),
    wait(fun() -> not erlang:check_old_code(A) end),
    true = is_process_alive(P), 2 = B:value(),
    #{paused := false, waiting := [], pending := []} = ridge_loader:purge_status(),
    P ! stop;
test(same_module_deadline) ->
    Pause = pause_cleanup(),
    M = ridge_purge_rescheduled, load(M, 1),
    {ok, _} = upgrade([M], 2, 100),
    waiting_while_paused([M]),
    {ok, _} = upgrade([M], 3, 60000),
    waiting_while_paused([M]),
    resume_cleanup(Pause),
    timer:sleep(200),
    true = erlang:check_old_code(M), 3 = M:value(),
    #{waiting := [<<"ridge_purge_rescheduled">>], pending := []} = ridge_loader:purge_status();
test(long_deadline) ->
    M = ridge_purge_long, load(M, 1),
    {ok, _} = upgrade([M], 2, 4294967296),
    #{waiting := [<<"ridge_purge_long">>], pending := []} = ridge_loader:purge_status(),
    true = erlang:check_old_code(M);
test(pause_owner_death) ->
    {Owner, Ref} = pause_cleanup(),
    M = ridge_purge_paused, load(M, 1),
    {ok, _} = upgrade([M], 2, 200),
    waiting_while_paused([M]),
    exit(Owner, kill),
    receive {'DOWN', Ref, process, Owner, killed} -> ok after 2000 -> error(owner_stuck) end,
    wait(fun() -> not erlang:check_old_code(M) end),
    #{paused := false, waiting := [], pending := []} = ridge_loader:purge_status();
test(dev_second_upgrade) ->
    M = ridge_purge_dev, load(M, 1), {P, Ref} = holder(M),
    {ok, _} = upgrade([M], 2, 20),
    wait(fun() -> pending(M) end),
    ok = file:write_file(atom_to_list(M) ++ ".beam", beam(M, 3)),
    Mf = #{base_vsn => <<"v2">>, new_vsn => <<"v3">>,
           modules => [atom_to_binary(M)], actors => []},
    ok = file:write_file("dev-upgrade.json", json:encode(Mf)),
    {error, {reload_rejected, load, [{M, not_purged}]}} =
        ridge_loader:apply("dev-upgrade.json", <<"v2">>),
    true = is_process_alive(P), 2 = M:value(),
    <<"v2">> = ridge_loader:current_version(),
    #{paused := false} = ridge_loader:purge_status(),
    P ! stop,
    receive {'DOWN', Ref, process, P, normal} -> ok after 2000 -> error(holder_stuck) end.

%% A separate pause owner keeps every cleanup job pending through the
%% applies under test, regardless of compilation or scheduler latency.
pause_cleanup() ->
    _ = ridge_loader:purge_status(),
    Manager = whereis(ridge_loader_purge), Parent = self(),
    {Owner, Monitor} = spawn_monitor(fun() ->
        Ref = make_ref(), Manager ! {purge_call, self(), Ref, pause},
        receive {Ref, ok} -> Parent ! {cleanup_paused, self()} end,
        receive release -> ok end,
        Resume = make_ref(), Manager ! {purge_call, self(), Resume, resume},
        receive {Resume, ok} -> Parent ! {cleanup_resumed, self()} end
    end),
    receive {cleanup_paused, Owner} -> {Owner, Monitor}
    after 2000 -> error(pause_timeout) end.

resume_cleanup({Owner, Monitor}) ->
    Owner ! release,
    receive {cleanup_resumed, Owner} -> ok after 2000 -> error(resume_timeout) end,
    receive {'DOWN', Monitor, process, Owner, normal} -> ok
    after 2000 -> error(pause_owner_stuck) end.

waiting_while_paused(Modules) ->
    Expected = lists:sort([atom_to_binary(M) || M <- Modules]),
    #{waiting := Expected, pending := [], paused := true} = ridge_loader:purge_status(),
    ok.

pending(M) ->
    #{pending := Pending} = ridge_loader:purge_status(),
    lists:member(atom_to_binary(M), Pending).

wait(Predicate) -> wait(Predicate, erlang:monotonic_time(millisecond) + 3000).
wait(Predicate, Deadline) ->
    case Predicate() of
        true -> ok;
        false ->
            case erlang:monotonic_time(millisecond) < Deadline of
                true -> timer:sleep(10), wait(Predicate, Deadline);
                false -> error({condition_timeout, ridge_loader:purge_status()})
            end
    end.

holder(M) ->
    Parent = self(), {P, Ref} = spawn_monitor(fun() -> M:hold(Parent) end),
    receive {holding, P} -> {P, Ref} after 2000 -> error(holder_not_ready) end.

load(M, V) -> {module, M} = code:load_binary(M, "purge-fixture", beam(M, V)).

upgrade(Modules, V, Delay) ->
    Base = ridge_loader:current_version(),
    Mf = #{base_vsn => Base, new_vsn => list_to_binary("v" ++ integer_to_list(V)),
           modules => [atom_to_binary(M) || M <- Modules], actors => []},
    ridge_loader:apply_bundle(iolist_to_binary(json:encode(Mf)),
        [{atom_to_binary(M), beam(M, V)} || M <- Modules],
        #{base_vsn => Base, purge_after_ms => Delay}).

beam(M, V) ->
    Texts = ["-module(" ++ atom_to_list(M) ++ ").", "-export([value/0,hold/1]).",
        "value() -> " ++ integer_to_list(V) ++ ".",
        "hold(Parent) -> Parent ! {holding,self()}, receive stop -> ok end."],
    Forms = [begin {ok, Tokens, _} = erl_scan:string(Text),
        {ok, Form} = erl_parse:parse_form(Tokens), Form end || Text <- Texts],
    {ok, M, Binary} = compile:forms(Forms, [binary, return_errors]), Binary.
