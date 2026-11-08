%% Reports must describe observable outcomes, independently of supervision.
-module(reload_report_probe).
-export([run/1, init/1, log/2,
         '__ridge_record_versions'/0, '__ridge_record_migrations'/0]).

run(Case) ->
    spawn(fun() -> receive after 15000 -> halt(2) end end),
    {module, ridge_rt} = code:ensure_loaded(ridge_rt),
    register(fallback_observer, self()),
    persistent_term:put(ridge_loader_vsn, <<"base">>),
    test(Case),
    io:format("report-probe-ok ~p~n", [Case]), halt(0).

test(migration_counter) ->
    0 = ridge_rt:migration_count(),
    Old = #{'__ridge_v' => {?MODULE, item, 1}},
    {ok, Current} = ridge_rt:migrate_value(Old),
    #{'__ridge_v' := {?MODULE, item, 3}} = Current,
    2 = ridge_rt:migration_count(),
    {ok, Current} = ridge_rt:migrate_value(Current),
    2 = ridge_rt:migration_count(),
    {'EXIT', {migration_failed, _}} = catch ridge_rt:migrate_value(
        #{'__ridge_v' => {?MODULE, item, 4}}),
    3 = ridge_rt:migration_count(),
    load(1, normal),
    {MB, Bs, Opts} = payload(normal),
    {ok, #{messages_migrated := 3}} = ridge_loader:apply_bundle(MB, Bs, Opts);
test(worker_death) ->
    ok = logger:add_handler(report_recovery_probe, ?MODULE,
                           #{level => warning, config => #{observer => self()}}),
    load(1, normal),
    {ok, Actor} = gen_server:start(fallback_toy, [7], []),
    {MB, Bs, Opts} = payload(block), Parent = self(),
    spawn(fun() -> Parent ! {result, ridge_loader:apply_bundle(MB, Bs, Opts)} end),
    Worker = receive {metadata, P} -> P after 3000 -> error(no_metadata_barrier) end,
    2 = fallback_toy:value(), true = is_process_alive(Actor),
    kill(Worker),
    receive {result, {error, {upgrade_failed, _, _, _}}} -> ok
    after 3000 -> error(no_worker_failure_result) end,
    receive {recovered, Report} ->
        #{actors_restarted := 1, actor_fallback := Fallback} = Report,
        assert_fallback(Fallback, 1, 1, 1),
        false = has_pid(Report)
    after 3000 -> error(no_recovery_report) end,
    false = is_process_alive(Actor), <<"next">> = ridge_loader:current_version();
test(Case) ->
    load(1, normal),
    {Actor, Supervisor} = case Case of
        supervised ->
            {ok, Sup} = supervisor:start_link(?MODULE, []),
            [{actor, P, worker, _}] = supervisor:which_children(Sup),
            ok = gen_server:call(P, {set, 7}), {P, Sup};
        _ -> {ok, P} = gen_server:start(fallback_toy, [7], []), {P, undefined}
    end,
    Other = case Case of multiple -> {ok, Bad} = gen_server:start(fallback_toy, [7], []), Bad;
                         _ -> undefined end,
    {ok, Sibling} = gen_server:start(fallback_toy, [0], []),
    Mode = case Case of normal -> normal; requested -> refuse; receipt -> refuse; _ -> crash end,
    {MB, Bs, Opts} = payload(Mode),
    Report = case Case of
        receipt ->
            #{incarnation := Inc} = ridge_loader:operation_protocol(),
            #{status := reserved, operation := Id} = ridge_loader:prepare_operation(Inc, MB, Bs, Opts),
            #{status := in_progress} = ridge_loader:apply_operation(Inc, Id, MB, Bs, Opts),
            #{status := applied, report := R} = wait_receipt(Inc, Id, deadline()), R;
        _ -> {ok, R} = ridge_loader:apply_bundle(MB, Bs, Opts), R
    end,
    #{actor_fallback := Fallback, actors_restarted := Legacy, restarts := Details} = Report,
    Expected = case Case of normal -> 0; multiple -> 2; _ -> 1 end,
    Requested = case Mode of refuse -> 1; _ -> 0 end,
    assert_fallback(Fallback, Expected, Requested, Expected),
    Expected = Legacy, Expected = length(Details),
    true = lists:all(fun(E) -> lists:sort(maps:keys(E)) =:= [module, reason] end, Details),
    false = has_pid(Fallback),
    2 = fallback_toy:value(), <<"next">> = ridge_loader:current_version(),
    #{count := 0, generation := 2} = gen_server:call(Sibling, get),
    case Case of
        normal -> true = is_process_alive(Actor),
                  #{count := 7, generation := 2} = gen_server:call(Actor, get);
        supervised ->
            false = is_process_alive(Actor),
            Fresh = replacement(Supervisor, Actor, deadline()),
            #{count := 0, generation := 2} = gen_server:call(Fresh, get);
        receipt ->
            false = is_process_alive(Actor),
            false = has_pid(Report),
            nomatch = binary:match(term_to_binary(Report), <<"private_callback_data">>);
        multiple -> false = is_process_alive(Actor), false = is_process_alive(Other);
        _ -> false = is_process_alive(Actor)
    end.

assert_fallback(Fallback, Affected, Requested, Terminated) ->
    Replacement = case Affected of 0 -> <<"not_required">>; _ -> <<"not_observed">> end,
    #{affected := Affected, termination_requested := Requested,
      terminated := Terminated, replacement_status := Replacement} = Fallback,
    ok.

has_pid(P) when is_pid(P) -> true;
has_pid(M) when is_map(M) -> has_pid(maps:to_list(M));
has_pid(T) when is_tuple(T) -> has_pid(tuple_to_list(T));
has_pid(L) when is_list(L) -> lists:any(fun has_pid/1, L);
has_pid(_) -> false.

log(#{msg := {report, #{event := reload_recovered} = Report}},
    #{config := #{observer := Parent}}) -> Parent ! {recovered, Report}, ok;
log(_, _) -> ok.

init([]) ->
    {ok, {#{strategy => one_for_one, intensity => 5, period => 5},
          [#{id => actor, start => {fallback_toy, start_link, []}, restart => permanent,
             shutdown => 1000, type => worker, modules => [fallback_toy]}]}}.

payload(Mode) ->
    MB = iolist_to_binary(json:encode(#{base_vsn => <<"base">>, new_vsn => <<"next">>,
        modules => [<<"fallback_toy">>], actors => [#{beam => <<"fallback_toy">>, renames => []}]})),
    {MB, [{<<"fallback_toy">>, beam(2, Mode)}], #{base_vsn => <<"base">>, purge_after_ms => 0}}.

load(V, Mode) -> {module, fallback_toy} = code:load_binary(fallback_toy, "report-fixture", beam(V, Mode)).

beam(V, Mode) ->
    Change = case Mode of
        crash -> "exit(self(),kill)";
        refuse -> "{error,{migration_failed,<<\"private_callback_data\">>}}";
        _ -> "{ok,S#{generation=>2}}"
    end,
    Barrier = case {V, Mode} of
        {2, block} -> "fallback_observer!{metadata,self()},receive continue->ok end,";
        _ -> ""
    end,
    Texts = ["-module(fallback_toy).",
        "-export([start_link/0,value/0,init/1,'__ridge_init'/1,handle_call/3,handle_cast/2,code_change/3,'__ridge_state_fields'/0,'__ridge_state_defaults'/0]).",
        "start_link()->gen_server:start_link(fallback_toy,[],[]).",
        "value()->" ++ integer_to_list(V) ++ ".",
        "init(A)->ridge_rt:actor_init(fallback_toy,A).",
        "'__ridge_init'([])->'__ridge_init'([0]);'__ridge_init'([C])->{ok,#{count=>C,generation=>" ++ integer_to_list(V) ++ "}}.",
        "handle_call(get,_,S)->{reply,S,S};handle_call({set,C},_,S)->{reply,ok,S#{count=>C}}.",
        "handle_cast(_,S)->{noreply,S}.",
        "code_change(_,S,ridge_refresh_collections)->{ok,S};code_change(_,#{count:=7}=S,_)->_ = S," ++ Change ++ ";code_change(_,S,_)->{ok,S#{generation=>2}}.",
        "'__ridge_state_fields'()->" ++ Barrier ++ "[count,generation].",
        "'__ridge_state_defaults'()->#{}."],
    Forms = [begin {ok, Ts, _} = erl_scan:string(Text),
                   {ok, Form} = erl_parse:parse_form(Ts), Form end || Text <- Texts],
    {ok, fallback_toy, Bin} = compile:forms(Forms, [binary, return_errors]), Bin.

'__ridge_record_versions'() -> #{item => 3}.
'__ridge_record_migrations'() ->
    [{1, fun(V) -> V#{'__ridge_v' => {?MODULE, item, 2}} end},
     {2, fun(V) -> V#{'__ridge_v' => {?MODULE, item, 3}} end},
     {4, fun(_) -> error(migration_failed) end}].

deadline() -> erlang:monotonic_time(millisecond) + 3000.
wait_receipt(Inc, Id, Deadline) ->
    case ridge_loader:query_operation(Inc, Id) of
        #{status := in_progress} -> retry(fun() -> wait_receipt(Inc, Id, Deadline) end, Deadline);
        R -> R
    end.
replacement(Sup, Old, Deadline) ->
    case supervisor:which_children(Sup) of
        [{actor, Fresh, worker, _}] when is_pid(Fresh), Fresh =/= Old -> Fresh;
        _ -> retry(fun() -> replacement(Sup, Old, Deadline) end, Deadline)
    end.
retry(F, Deadline) ->
    true = erlang:monotonic_time(millisecond) < Deadline,
    receive after 1 -> F() end.
kill(Pid) ->
    Ref = monitor(process, Pid), exit(Pid, kill),
    receive {'DOWN', Ref, process, Pid, killed} -> ok after 3000 -> error(kill_timeout) end.
