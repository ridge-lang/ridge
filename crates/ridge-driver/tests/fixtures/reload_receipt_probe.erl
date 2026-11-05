%% Real code and actor state are the oracle; receipts alone cannot pass.
-module(reload_receipt_probe).
-export([run/1]).

run(Case) ->
    spawn(fun() -> receive after 15000 -> halt(2) end end),
    {module, ridge_rt} = code:ensure_loaded(ridge_rt),
    register(receipt_observer, self()),
    persistent_term:put(ridge_loader_vsn, <<"base">>),
    test(Case),
    io:format("receipt-probe-ok ~p~n", [Case]), halt(0).

test(Case) when Case =:= duplicate; Case =:= lost_ack; Case =:= postcommit; Case =:= worker_death;
                Case =:= registry_death ->
    OldMeta = case Case of
        duplicate -> blocking();
        lost_ack -> blocking();
        worker_death -> blocking();
        registry_death -> blocking();
        _ -> normal()
    end,
    {module, receipt_toy} = code:load_binary(receipt_toy, "old", beam(1, OldMeta)),
    {ok, Actor} = gen_server:start(receipt_toy, [], []),
    NewMeta = case Case of postcommit -> blocking(); _ -> normal() end,
    Payload = payload(beam(2, NewMeta)),
    {Inc, Id, Digest} = reserve(Payload),
    Caller = case Case of
        lost_ack -> spawn(fun() -> _ = submit(Inc, Id, Payload), receive hold -> ok end end);
        _ -> #{status := in_progress} = submit(Inc, Id, Payload), undefined
    end,
    Server = receive {metadata, S} -> S after 3000 -> error(no_barrier) end,
    case Case of lost_ack -> kill(Caller); _ -> ok end,
    Progress = ridge_loader:query_operation(Inc, Id),
    #{status := in_progress, digest := Digest, recovery_fenced := true} = Progress,
    case Case of
        C when C =:= duplicate; C =:= lost_ack ->
            1 = receipt_toy:value(),
            #{code_commit := unconfirmed} = Progress,
            sys:replace_state(whereis(ridge_loader_receipts), fun(State) ->
                Es = maps:get(entries, State), R = maps:get(Id, Es),
                State#{entries => Es#{Id => R#{created => maps:get(created, R)-900001}}}
            end),
            #{status := in_progress} = ridge_loader:query_operation(Inc, Id),
            Parent = self(),
            [spawn(fun() -> Parent ! {duplicate, submit(Inc, Id, Payload)} end)
             || _ <- lists:seq(1, 8)],
            [receive {duplicate, #{status := in_progress}} -> ok
             after 1000 -> error(duplicate_blocked) end || _ <- lists:seq(1, 8)],
            Server ! continue,
            #{status := applied, code_commit := confirmed, actor_migration := complete,
              applied_version := <<"next">>} = wait(Inc, Id),
            #{generation := 2, migrations := 2} = sys:get_state(Actor),
            #{status := applied} = submit(Inc, Id, Payload),
            #{generation := 2, migrations := 2} = sys:get_state(Actor),
            2 = receipt_toy:value(),
            {MB, Bins, Opts} = Payload,
            {error, operation_conflict} = ridge_loader:apply_operation(Inc, Id, MB, Bins,
                                                                       Opts#{seed => true}),
            {error, operation_conflict} = submit(Inc, Id, payload(beam(3, normal())));
        postcommit ->
            2 = receipt_toy:value(),
            wait_commit(Inc, Id, deadline()),
            Server ! fail,
            #{status := recovery_required, code_commit := confirmed,
              actor_migration := incomplete} = wait(Inc, Id),
            #{status := recovery_required} = submit(Inc, Id, Payload),
            2 = receipt_toy:value();
        worker_death ->
            kill(Server),
            #{status := recovery_required} = wait(Inc, Id),
            #{status := recovery_required} = submit(Inc, Id, Payload),
            1 = receipt_toy:value();
        registry_death ->
            kill(whereis(ridge_loader_receipts)),
            #{incarnation := NewInc} = ridge_loader:operation_protocol(),
            true = NewInc =/= Inc,
            {error, incarnation_mismatch} = ridge_loader:query_operation(Inc, Id),
            {error, incarnation_mismatch} = submit(Inc, Id, Payload),
            Server ! continue,
            wait_value(2, deadline())
    end;
test(rejection) ->
    Bad = {<<"{}">>, [], #{base_vsn => <<"base">>}},
    {Inc, Id, _} = reserve(Bad),
    _ = submit(Inc, Id, Bad),
    #{status := rejected, code_commit := unconfirmed} = wait(Inc, Id),
    #{status := rejected} = submit(Inc, Id, Bad),
    <<"base">> = ridge_loader:current_version();
test(cancel_reserved) ->
    Payload = payload(beam(2, normal())),
    {Inc, Id, Digest} = reserve(Payload),
    {error, operation_conflict} = ridge_loader:cancel_operation(Inc, Id, <<"wrong">>),
    #{status := reserved} = ridge_loader:query_operation(Inc, Id),
    #{status := rejected, rejection := not_submitted} = ridge_loader:cancel_operation(Inc, Id, Digest),
    #{status := rejected} = submit(Inc, Id, Payload),
    <<"base">> = ridge_loader:current_version(),
    false = code:is_loaded(receipt_toy);
test(restart_before) ->
    {Inc, Id, _} = reserve(payload(beam(2, normal()))),
    ok = file:write_file("receipt-before-restart.term", term_to_binary({Inc, Id}));
test(restart_after) ->
    {ok, Bytes} = file:read_file("receipt-before-restart.term"),
    {Inc, Id} = binary_to_term(Bytes, [safe]),
    #{incarnation := Next} = ridge_loader:operation_protocol(),
    true = Inc =/= Next,
    {error, incarnation_mismatch} = ridge_loader:query_operation(Inc, Id),
    {error, incarnation_mismatch} = submit(Inc, Id, payload(beam(2, normal())));
test(expiry_capacity) ->
    Payload = payload(beam(2, normal())),
    {Inc, Id, _} = reserve(Payload),
    [reserve(Payload) || _ <- lists:seq(2, 128)],
    {MB, Bs, Opts} = Payload,
    {error, receipt_capacity} = ridge_loader:prepare_operation(Inc, MB, Bs, Opts),
    %% Advance the stored age through OTP's debugging API, not a wall-clock
    %% sleep or a production test-only setting. Unknown IDs never reapply.
    sys:replace_state(whereis(ridge_loader_receipts), fun(State) ->
        Es = maps:get(entries, State),
        State#{entries => maps:map(fun(_, R) -> R#{created => maps:get(created, R)-900001} end, Es)}
    end),
    {error, unknown_operation} = ridge_loader:query_operation(Inc, Id),
    {error, unknown_operation} = submit(Inc, Id, Payload),
    {Inc, NewId, _} = reserve(Payload), true = NewId =/= Id,
    <<"base">> = ridge_loader:current_version().

reserve({MB, Bs, Opts}) ->
    #{protocol := 1, incarnation := Inc} = ridge_loader:operation_protocol(),
    #{status := reserved, operation := Id, digest := Digest} =
        ridge_loader:prepare_operation(Inc, MB, Bs, Opts),
    {Inc, Id, Digest}.

submit(Inc, Id, {MB, Bs, Opts}) -> ridge_loader:apply_operation(Inc, Id, MB, Bs, Opts).

payload(Bin) ->
    MB = iolist_to_binary(json:encode(#{base_vsn => <<"base">>, new_vsn => <<"next">>,
        modules => [<<"receipt_toy">>], actors => [#{beam => <<"receipt_toy">>, renames => []}]})),
    {MB, [{<<"receipt_toy">>, Bin}], #{base_vsn => <<"base">>, purge_after_ms => 0}}.

normal() -> "'__ridge_state_fields'()->[].".
blocking() -> "'__ridge_state_fields'()->receipt_observer!{metadata,self()},receive continue->[];fail->error(private_migration_failure) end.".

beam(Value, Metadata) ->
    Texts = ["-module(receipt_toy).",
        "-export([value/0,init/1,'__ridge_init'/1,handle_call/3,handle_cast/2,code_change/3,'__ridge_state_fields'/0,'__ridge_state_defaults'/0]).",
        "value()->" ++ integer_to_list(Value) ++ ".", Metadata,
        "init(A)->ridge_rt:actor_init(receipt_toy,A).",
        "'__ridge_init'([])->{ok,#{generation=>1,migrations=>0}}.",
        "'__ridge_state_defaults'()->#{}.",
        "handle_call(_,_,S)->{reply,S,S}.", "handle_cast(_,S)->{noreply,S}.",
        "code_change(_,S,_)->{ok,S#{generation=>" ++ integer_to_list(Value) ++
            ",migrations=>maps:get(migrations,S)+1}}."],
    Forms = [begin {ok, Ts, _} = erl_scan:string(Text),
                   {ok, Form} = erl_parse:parse_form(Ts), Form end || Text <- Texts],
    {ok, receipt_toy, Bin} = compile:forms(Forms, [binary, return_errors]), Bin.

deadline() -> erlang:monotonic_time(millisecond) + 3000.
wait(Inc, Id) -> wait(Inc, Id, deadline()).
wait(Inc, Id, Deadline) ->
    R = ridge_loader:query_operation(Inc, Id),
    case maps:get(status, R) of
        in_progress -> true = erlang:monotonic_time(millisecond) < Deadline,
                       receive after 1 -> wait(Inc, Id, Deadline) end;
        _ -> R
    end.
wait_commit(Inc, Id, Deadline) ->
    case ridge_loader:query_operation(Inc, Id) of
        #{code_commit := confirmed} -> ok;
        _ -> true = erlang:monotonic_time(millisecond) < Deadline,
             receive after 1 -> wait_commit(Inc, Id, Deadline) end
    end.
wait_value(Value, Deadline) ->
    case receipt_toy:value() of
        Value -> ok;
        _ -> true = erlang:monotonic_time(millisecond) < Deadline,
             receive after 1 -> wait_value(Value, Deadline) end
    end.
kill(Pid) ->
    Ref = monitor(process, Pid), exit(Pid, kill),
    receive {'DOWN', Ref, process, Pid, killed} -> ok after 1000 -> error(kill_timeout) end.
