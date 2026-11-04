%% ridge_loader — hot code upgrade orchestrator.
%%
%% Applies an upgrade manifest: validates the base version, suspends the
%% affected actors, loads the recompiled modules, migrates actor state
%% through sys:change_code, resumes, and reports. Two transports funnel
%% into the same sequence:
%%
%%   apply/2        — dev loop (`ridge run --reload`): the manifest is a
%%                    JSON file and the node loads the new code from its own
%%                    code path.
%%   apply_bundle/3,4 — production (`ridge reload --node`): the manifest and
%%                    the new .beam blobs arrive over rpc; no filesystem on
%%                    the target node is involved.
%%
%% The node tracks the version of the code it runs in
%% persistent_term[?VSN_KEY]; the CLI seeds it at boot and this module
%% advances it after every successful upgrade. A manifest whose base_vsn
%% does not match is rejected before anything is touched.
%%
%% Requires OTP 27+ (the `json` module).
-module(ridge_loader).
-behaviour(gen_server).
-export([current_version/0, apply/2, apply_bundle/3, apply_bundle/4, purge_status/0, startup_protocol/0]).
-export([init/1, handle_call/3, handle_cast/2, handle_info/2]).

-define(VSN_KEY, ridge_loader_vsn).
-define(ADMISSION, ridge_loader_admission).
-define(ACTIVE_KEY, ridge_loader_active_reload).
-define(RECOVERY_REG, ridge_loader_recovery).
-define(RECOVERY_TIMEOUT, 5000).

current_version() ->
    persistent_term:get(?VSN_KEY, undefined).

startup_protocol() ->
    _ = code:ensure_loaded(ridge_rt),
    case erlang:function_exported(ridge_rt, actor_init, 2) andalso
         erlang:function_exported(ridge_rt, actor_startup, 1) of
        true -> 1;
        false -> 0
    end.

apply(ManifestPath, ExpectedVsn) ->
    admitted({dev, ManifestPath, ExpectedVsn}).

%% Unlinked from RPC callers: a timeout or caller death must not interrupt
%% an admitted operation. Never replay a call after the coordinator dies.
admitted(Request) ->
    try
        Pid = case gen_server:start({local, ?ADMISSION}, ?MODULE, [], []) of
            {ok, StartedPid} -> StartedPid;
            {error, {already_started, ExistingPid}} -> ExistingPid
        end,
        case Pid =:= self() of
            true -> {error, {reload_rejected, admission, reentrant}};
            false -> gen_server:call(Pid, Request, infinity)
        end
    catch
        %% gen_server:call exits include the original request, including
        %% every bytecode blob. Do not reflect it into RPC replies or logs.
        exit:{Reason, {gen_server, call, _}} ->
            {error, {upgrade_failed, exit,
                     {reload_admission_down, admission_failure_reason(Reason)}, []}};
        Class:Reason:_ ->
            {error, {upgrade_failed, Class,
                     {reload_admission_failed, admission_failure_reason(Reason)}, []}}
    end.

admission_failure_reason(noproc) -> noproc;
admission_failure_reason(normal) -> normal;
admission_failure_reason(killed) -> killed;
admission_failure_reason(shutdown) -> shutdown;
admission_failure_reason(_) -> coordinator_failed.

init([]) -> {ok, undefined}.

handle_call(Request, {Caller, _}, State) ->
    case node(Caller) =:= node() andalso not is_process_alive(Caller) of
        true -> {noreply, State};
        false ->
            Reply = case whereis(?RECOVERY_REG) of
                undefined ->
                    case persistent_term:get(?ACTIVE_KEY, undefined) of
                        undefined -> dispatch(Request);
                        _ -> {error, {reload_recovery_required, restart_node}}
                    end;
                _ -> {error, {reload_rejected, admission, recovery_in_progress}}
            end,
            {reply, Reply, State}
    end.

handle_cast(_, State) -> {noreply, State}.
handle_info(_, State) -> {noreply, State}.

dispatch({dev, ManifestPath, ExpectedVsn}) -> apply_admitted(ManifestPath, ExpectedVsn);
dispatch({bundle, ManifestBin, BeamBins, Opts}) ->
    apply_bundle_admitted(ManifestBin, BeamBins, Opts);
dispatch(_) -> {error, {reload_rejected, admission, invalid_request}}.

base_matches(ExpectedVsn, Opts) ->
    Current = current_version(),
    Current =:= ExpectedVsn orelse
        (Current =:= undefined andalso maps:get(seed, Opts, false)).

apply_admitted(ManifestPath, ExpectedVsn) ->
    case current_version() =:= ExpectedVsn of
        false ->
            {error, {base_version_mismatch, ExpectedVsn, current_version()}};
        true ->
            Started = erlang:monotonic_time(millisecond),
            try
                ensure_json(),
                {ok, Bin} = file:read_file(ManifestPath),
                do_apply(json:decode(Bin), Started, fun load_from_path/2, undefined,
                         %% Dev loop: no quiescence purge — the node holds two
                         %% code versions until restart, as before.
                         #{purge_after_ms => 0})
            catch
                Class:Reason:Stack ->
                    {error, {upgrade_failed, Class, Reason, Stack}}
            end
    end.

%% Production transport: ManifestBin is the manifest JSON as a binary and
%% BeamBins is [{ModuleNameBin, BeamBinary}] — every .beam blob of the
%% bundle. Opts carries base_vsn (the version the node must be running)
%% and, optionally, purge_after_ms (quiescence purge, see schedule_purge).
%% Every blob is validated before ANY module is loaded, so a corrupt
%% bundle leaves the node untouched.
apply_bundle(ManifestBin, BeamBins, Opts)
  when is_binary(ManifestBin), is_list(BeamBins), is_map(Opts) ->
    admitted({bundle, ManifestBin, BeamBins, Opts});
apply_bundle(_, _, _) ->
    {error, {reload_rejected, preflight, invalid_bundle_arguments}}.

%% The protocol travels with the operation. A target replaced after the CLI's
%% pre-compile handshake must not fall back to an older, uncoordinated loader.
%% Older loaders do not export this arity; future versions are rejected here.
apply_bundle(Protocol, ManifestBin, BeamBins, Opts) ->
    case Protocol =:= 1 andalso startup_protocol() =:= 1 of
        true -> apply_bundle(ManifestBin, BeamBins, Opts);
        false -> {error, {reload_rejected, preflight, unsupported_startup_protocol}}
    end.

apply_bundle_admitted(ManifestBin, BeamBins, Opts) ->
    Started = erlang:monotonic_time(millisecond),
    case prepare_bundle(ManifestBin, BeamBins, Opts) of
        {error, _} = Rejected -> Rejected;
        {ok, Mf, Prepared, PurgeAfter} ->
            try
                do_apply(Mf, Started, fun load_prepared/2, Prepared,
                         #{purge_after_ms => PurgeAfter, atomic_load => true,
                           seed => maps:get(seed, Opts, false)})
            catch
                Class:Reason:Stack -> {error, {upgrade_failed, Class, Reason, Stack}}
            end
    end.

%% Keep preflight exception handling outside actor callbacks: a callback's
%% thrown term must never be mistaken for a rejection before code changed.
prepare_bundle(ManifestBin, BeamBins, Opts) ->
    try
        ExpectedVsn = required_version(base_vsn, Opts),
        require(is_boolean(maps:get(seed, Opts, false)), {invalid_field, seed}),
        case base_matches(ExpectedVsn, Opts) of
            false -> throw({base_version_mismatch, ExpectedVsn, current_version()});
            true -> ok
        end,
        PurgeAfter = maps:get(purge_after_ms, Opts, 60000),
        require(is_integer(PurgeAfter) andalso PurgeAfter >= 0, {invalid_field, purge_after_ms}),
        ensure_json(),
        Mf = try json:decode(ManifestBin)
             catch _:_ -> reject({invalid_field, manifest_json}) end,
        Modules = validate_manifest(Mf, ExpectedVsn),
        Bins = validate_blobs(BeamBins, Modules),
        Prepared = case code:prepare_loading(Bins) of
            {ok, P} -> P;
            {error, Errors} -> reject({invalid_modules, Errors})
        end,
        [validate_actor_startup_blob(Bin) || {_, _, Bin} <- Bins],
        {ok, Mf, Prepared, PurgeAfter}
    catch
        throw:{bundle_rejected, Reason} -> {error, {reload_rejected, preflight, Reason}};
        throw:{base_version_mismatch, _, _} = Reason -> {error, Reason};
        Class:Reason:Stack -> {error, {upgrade_failed, Class, Reason, Stack}}
    end.

validate_actor_startup_blob(Bin) ->
    {ok, {Mod, [{exports, Exports}]}} = beam_lib:chunks(Bin, [exports]),
    IsActor = lists:member({'__ridge_state_fields', 0}, Exports) andalso
              lists:member({init, 1}, Exports),
    require(not IsActor orelse lists:member({'__ridge_init', 1}, Exports),
            {actor_rebuild_required, Mod}).

ensure_json() ->
    case code:ensure_loaded(json) of
        {module, json} -> ok;
        _ -> erlang:error(ridge_loader_requires_otp_27)
    end.

reject(Reason) -> throw({bundle_rejected, Reason}).

require(true, _) -> ok;
require(false, Reason) -> reject(Reason).

required_version(Key, Map) ->
    V = maps:get(Key, Map, undefined),
    require(is_binary(V) andalso byte_size(V) > 0, {invalid_field, Key}),
    V.

%% Validate names before converting them to atoms. Bytecode itself is still
%% trusted executable input, accepted from trusted distribution peers.
valid_name(Name) when is_binary(Name), byte_size(Name) > 0 ->
    case unicode:characters_to_list(Name) of
        Chars when is_list(Chars), length(Chars) =< 255 -> true;
        _ -> false
    end;
valid_name(_) -> false.

unique_names(Names, Field) ->
    require(is_list(Names), {invalid_field, Field}),
    lists:foldl(fun(Name, Seen) ->
        require(valid_name(Name), {invalid_name, Field}),
        require(not maps:is_key(Name, Seen), {duplicate_name, Field, Name}),
        Seen#{Name => true}
    end, #{}, Names).

validate_manifest(Mf, ExpectedVsn) when is_map(Mf) ->
    Base = required_version(<<"base_vsn">>, Mf),
    require(Base =:= ExpectedVsn, {manifest_base_version_mismatch, ExpectedVsn, Base}),
    _ = required_version(<<"new_vsn">>, Mf),
    %% Format was optional in the original transport. Keep formats 1 and 2
    %% and the original omitted-field form, but reject unknown revisions.
    Format = maps:get(<<"format">>, Mf, 1),
    require(Format =:= 1 orelse Format =:= 2, {invalid_field, <<"format">>}),
    Modules = maps:get(<<"modules">>, Mf, undefined),
    Names = unique_names(Modules, modules),
    Actors = maps:get(<<"actors">>, Mf, undefined),
    require(is_list(Actors), {invalid_field, actors}),
    _ = lists:foldl(fun(A, Seen) -> validate_actor(A, Names, Seen) end, #{}, Actors),
    Modules;
validate_manifest(_, _) -> reject({invalid_field, manifest}).

validate_actor(A, Modules, Seen) when is_map(A) ->
    Beam = maps:get(<<"beam">>, A, undefined),
    require(valid_name(Beam), {invalid_name, actor}),
    require(maps:is_key(Beam, Modules), {actor_not_in_modules, Beam}),
    require(not maps:is_key(Beam, Seen), {duplicate_name, actors, Beam}),
    Renames = maps:get(<<"renames">>, A, undefined),
    require(is_list(Renames), {invalid_field, renames, Beam}),
    _ = lists:foldl(fun
        ([From, To], {Sources, Targets}) ->
            require(valid_name(From) andalso valid_name(To), {invalid_field, renames, Beam}),
            require(not maps:is_key(From, Sources) andalso not maps:is_key(To, Targets),
                    {duplicate_rename, Beam}),
            {Sources#{From => true}, Targets#{To => true}};
        (_, _) -> reject({invalid_field, renames, Beam})
    end, {#{}, #{}}, Renames),
    require(is_boolean(maps:get(<<"migrate_hook">>, A, false)),
            {invalid_field, migrate_hook, Beam}),
    [require(is_integer(maps:get(K, A, 0)), {invalid_field, K, Beam})
     || K <- [<<"old_state_hash">>, <<"new_state_hash">>]],
    Seen#{Beam => true};
validate_actor(_, _, _) -> reject({invalid_field, actor}).

validate_blobs(BeamBins, Modules) ->
    BinMap = lists:foldl(fun
        ({Name, Bin}, Seen) when is_binary(Bin) ->
            require(valid_name(Name), {invalid_name, blob}),
            require(not maps:is_key(Name, Seen), {duplicate_name, blobs, Name}),
            Seen#{Name => Bin};
        (_, _) -> reject({invalid_field, blob_entry})
    end, #{}, BeamBins),
    [require(maps:is_key(M, BinMap), {missing_blob, M}) || M <- Modules],
    ModuleSet = maps:from_list([{M, true} || M <- Modules]),
    [require(maps:is_key(M, ModuleSet), {unexpected_blob, M}) || M <- maps:keys(BinMap)],
    [{binary_to_atom(M), beam_label(binary_to_atom(M)), maps:get(M, BinMap)} || M <- Modules].

do_apply(Mf, Started, LoadFun, LoadCtx, Opts) ->
    #{
        <<"base_vsn">> := BaseVsn,
        <<"new_vsn">> := NewVsn,
        <<"modules">> := Modules0,
        <<"actors">> := ActorMigs0
    } = Mf,
    case base_matches(BaseVsn, Opts) of
        false -> erlang:error({base_version_mismatch, BaseVsn, current_version()});
        true -> ok
    end,
    Modules = [binary_to_atom(M) || M <- Modules0],
    ActorMigs = [
        #{
            beam => binary_to_atom(maps:get(<<"beam">>, A)),
            renames => [
                {binary_to_atom(F), binary_to_atom(T)}
                || [F, T] <- maps:get(<<"renames">>, A)
            ],
            migrate_hook => maps:get(<<"migrate_hook">>, A, false),
            old_state_hash => maps:get(<<"old_state_hash">>, A, 0)
        }
        || A <- ActorMigs0
    ],
    MigMods = [M || #{beam := M} <- ActorMigs],
    %% Comparison methods can depend on any reloaded module, even when an
    %% actor's own callback did not change. Refresh its retained indexes once
    %% at the upgrade boundary rather than repeatedly in later queries.
    case start_recovery_owner(Modules, NewVsn, maps:get(purge_after_ms, Opts, 0)) of
        {error, _} = Rejected -> Rejected;
        {ok, Owner} ->
            do_apply_owned(MigMods, ActorMigs, Modules, Started,
                           LoadFun, LoadCtx, Opts, Owner)
    end.

do_apply_owned(MigMods, ActorMigs, Modules, Started, LoadFun, LoadCtx, Opts, Owner) ->
    Outcome = try
        case recovery_call(Owner, suspend) of
            {error, actor_started} -> startup_rejected;
            {ok, Suspended} ->
                %% Capture OLD field lists before loading the new code.
                OldFieldsByMod = maps:from_list([{M, M:'__ridge_state_fields'()} || M <- MigMods]),
                case recovery_call(Owner, commit) of
                    {error, actor_started} -> startup_rejected;
                    ok ->
                        case load_and_invalidate(Modules, LoadFun, LoadCtx, Opts) of
                            {error, Errors} -> {rejected, Errors};
                            ok ->
                                %% Confirm both the code swap and evidence
                                %% invalidation before publishing this version.
                                ok = recovery_call(Owner, loaded),
                                {Migrated, SchemaRestarts} = migrate_actors(Suspended, ActorMigs, OldFieldsByMod),
                                {Refreshed, IndexRestarts} = refresh_actor_collections(Suspended, Owner),
                                {ready, Suspended, Migrated, Refreshed, SchemaRestarts ++ IndexRestarts}
                        end
                end
        end
    catch
        Class:Reason:Stack -> {failed, Class, Reason, Stack}
    end,
    case Outcome of
        startup_rejected ->
            case recovery_finish(Owner, abort) of
                ok -> {error, {reload_rejected, startup, actor_started}};
                {error, {reload_recovery_required, restart_node}} = Required -> Required
            end;
        {ready, SuspendedPids, MigratedPids, RefreshedPids, PriorRestarts} ->
            {ok, Purge, RecoveryRestarts} = recovery_finish(Owner, complete),
            Restarts = PriorRestarts ++ RecoveryRestarts,
            {ok, #{
                    modules_loaded => length(Modules),
                    actors_suspended => length(SuspendedPids),
                    actors_migrated => length(MigratedPids),
                    actors_refreshed => length(RefreshedPids),
                    actors_restarted => length(Restarts),
                    restarts => lists:reverse(Restarts),
                    purge => Purge,
                    %% Cumulative and lazy: migrations happen as messages arrive AFTER
                    %% the upgrade, so this mostly reflects traffic since the previous
                    %% reload — the next reload's report includes this window's.
                    messages_migrated => ridge_rt:migration_count(),
                    duration_ms => erlang:monotonic_time(millisecond) - Started
                }};
        {rejected, LoadErrors} ->
            ok = recovery_finish(Owner, rejected),
            {error, {reload_rejected, load, LoadErrors}};
        {failed, FailureClass, FailureReason, FailureStack} ->
            %% The owner, not this potentially dying process, resolves every
            %% suspension. Its phase decides whether old state is still safe.
            _ = catch recovery_finish(Owner, abort),
            erlang:raise(FailureClass, FailureReason, FailureStack)
    end.

%% A separate, unlinked owner survives the applicator. It never receives
%% bytecode or application state, and never runs a user migration callback.
%% Its registered lifetime also excludes a replacement admission server
%% until recovery has finished, including the interval before the fence.
start_recovery_owner(Modules, Version, Delay) ->
    Worker = self(), Tag = make_ref(),
    {Pid, Monitor} = spawn_monitor(fun() ->
        case catch register(?RECOVERY_REG, self()) of
            true ->
                WorkerMonitor = monitor(process, Worker),
                case catch ridge_rt:actor_startup(begin_reload) of
                    ok ->
                        %% The startup writer must precede even the census.
                        %% Initializers already running reject admission;
                        %% births during the census cancel the load grant.
                        case startup_actor_candidates(Modules) of
                            {ok, Pids} ->
                                Worker ! {Tag, {ready, Pids}},
                                recovery_loop(#{worker => Worker, monitor => WorkerMonitor,
                                    candidates => Pids, owned => #{}, suspended => [], restarts => [],
                                    phase => precommit, modules => Modules, version => Version, delay => Delay});
                            {error, Why} ->
                                {ok, 0} = ridge_rt:actor_startup(abort),
                                ok = ridge_rt:actor_startup(finish),
                                Worker ! {Tag, {rejected, Why}}
                        end;
                    {error, initializing} -> Worker ! {Tag, {rejected, initializing}};
                    _ -> Worker ! {Tag, {rejected, recovery_required}}
                end;
            _ -> Worker ! {Tag, busy}
        end
    end),
    receive
        {Tag, {ready, Pids}} -> {ok, #{pid => Pid, monitor => Monitor, candidates => Pids}};
        {Tag, {rejected, Why}} ->
            receive {'DOWN', Monitor, process, Pid, _} -> ok end,
            {error, {reload_rejected, startup, Why}};
        {Tag, busy} ->
            receive {'DOWN', Monitor, process, Pid, _} -> ok end,
            {error, {reload_rejected, admission, recovery_in_progress}};
        {'DOWN', Monitor, process, Pid, _} ->
            erlang:error(reload_recovery_owner_unavailable)
    end.

recovery_call(#{pid := Pid, monitor := Monitor} = Owner, Request) ->
    case is_process_alive(Pid) of
        false -> recovery_owner_lost(Owner);
        true ->
            Ref = make_ref(), Pid ! {recovery_call, self(), Ref, Request},
            receive
                {Ref, Reply} -> Reply;
                {'DOWN', Monitor, process, Pid, _} -> recovery_owner_lost(Owner)
            end
    end.

recovery_finish(#{pid := Pid, monitor := Monitor} = Owner, Request) ->
    Reply = recovery_call(Owner, Request),
    receive {'DOWN', Monitor, process, Pid, normal} -> Reply;
            {'DOWN', Monitor, process, Pid, _} -> recovery_owner_lost(Owner)
    end.

recovery_owner_lost(#{candidates := Pids}) ->
    %% An absent fence means either no suspension started, or the owner
    %% completed all effects. Otherwise its lost journal is unsafe to resume.
    case persistent_term:get(?ACTIVE_KEY, undefined) of
        undefined -> ok;
        _ -> _ = stop_recovery_actors(Pids, reload_recovery_owner_down)
    end,
    erlang:error(reload_recovery_owner_down).

recovery_loop(#{worker := Worker, monitor := Monitor} = State) ->
    receive
        {recovery_call, Worker, Ref, suspend} ->
            persistent_term:put(?ACTIVE_KEY, mutating),
            case suspend_recovery_actors(maps:get(candidates, State), State) of
                {ok, Next} ->
                    Worker ! {Ref, {ok, lists:reverse(maps:get(suspended, Next))}},
                    recovery_loop(Next);
                {failed, Next} ->
                    Worker ! {Ref, {error, reload_actor_unavailable}}, recovery_loop(Next);
                {cancelled, Next} ->
                    Worker ! {Ref, {error, actor_started}}, recovery_loop(Next);
                {abandoned, Next} -> recover_and_stop(Next, applicator_down)
            end;
        {recovery_call, Worker, Ref, commit} ->
            %% The grant is acknowledged before the applicator can load code.
            %% A death after this grant but before `loaded` is ambiguous.
            case catch pause_pending_purge() of
                ok ->
                    case {ridge_rt:actor_startup(check), is_process_alive(Worker),
                          lists:all(fun is_process_alive/1, maps:keys(maps:get(owned, State)))} of
                        {{error, actor_started}, true, _} ->
                            Worker ! {Ref, {error, actor_started}}, recovery_loop(State);
                        {ok, true, true} ->
                            case ridge_rt:actor_startup(commit) of
                                ok -> Worker ! {Ref, ok}, recovery_loop(State#{phase => committing});
                                {error, actor_started} ->
                                    Worker ! {Ref, {error, actor_started}}, recovery_loop(State)
                            end;
                        {ok, true, false} ->
                            Worker ! {Ref, {error, reload_actor_unavailable}}, recovery_loop(State);
                        {_, false, _} -> recover_and_stop(State, applicator_down)
                    end;
                _ -> Worker ! {Ref, {error, reload_cleanup_unavailable}}, recovery_loop(State)
            end;
        {recovery_call, Worker, Ref, loaded} ->
            committing = maps:get(phase, State),
            Worker ! {Ref, ok}, recovery_loop(State#{phase => loaded});
        {recovery_call, Worker, Ref, {ready, Pid}} ->
            loaded = maps:get(phase, State), Owned = maps:get(owned, State),
            true = maps:is_key(Pid, Owned),
            Worker ! {Ref, ok}, recovery_loop(State#{owned => Owned#{Pid => true}});
        {recovery_call, Worker, Ref, complete} ->
            loaded = maps:get(phase, State),
            true = lists:all(fun(P) -> not is_process_alive(P) orelse maps:get(P, maps:get(owned, State)) =:= true end,
                             maps:keys(maps:get(owned, State))),
            {Result, Stats} = complete_recovery(State),
            Worker ! {Ref, Result},
            case is_process_alive(Worker) of
                false -> recovery_notice(State, applicator_down, Stats);
                true -> ok
            end;
        {recovery_call, Worker, Ref, rejected} ->
            %% finish_loading explicitly rejected its whole atomic batch.
            {Result, _} = abort_recovery(State#{phase => precommit}),
            Worker ! {Ref, Result};
        {recovery_call, Worker, Ref, abort} ->
            {Result, Stats} = abort_recovery(State),
            recovery_notice(State, applicator_error, Stats),
            Worker ! {Ref, Result};
        {ridge_startup_cancelled, _Gate} -> recovery_loop(State);
        {'DOWN', Monitor, process, Worker, _} -> recover_and_stop(State, applicator_down)
    end.

suspend_recovery_actors([], State) -> {ok, State};
suspend_recovery_actors([P | Rest], #{worker := Worker, owned := Owned} = State) ->
    case is_process_alive(Worker) of
        false -> {abandoned, State};
        true ->
            case is_process_alive(P) of
                false -> {failed, State};
                true ->
                    %% Own the intent before sending the system request. A
                    %% timed-out suspension is killed, never followed by a
                    %% resume which could race a late suspend request.
                    Next = State#{owned => Owned#{P => restart}},
                    case catch sys:suspend(P, ?RECOVERY_TIMEOUT) of
                        ok ->
                            Confirmed = Next#{owned => Owned#{P => false},
                                              suspended => [P | maps:get(suspended, Next)]},
                            case ridge_rt:actor_startup({suspended, P}) of
                                ok -> suspend_recovery_actors(Rest, Confirmed);
                                {error, actor_started} -> {cancelled, Confirmed}
                            end;
                        _ ->
                            {Restarts, Safe} = stop_recovery_actors([P], reload_suspend_failed),
                            _ = Safe,
                            {failed, Next#{restarts => Restarts ++ maps:get(restarts, Next)}}
                    end
            end
    end.

recover_and_stop(State, Cause) ->
    {_, Stats} = abort_recovery(State),
    recovery_notice(State, Cause, Stats).

abort_recovery(#{phase := loaded} = State) ->
    {Result, Stats} = complete_recovery(State),
    {case Result of {ok, _, _} -> ok; _ -> Result end, Stats};
abort_recovery(#{phase := Phase} = State) ->
    {Stats, Safe} = case Phase of
        precommit -> settle_precommit(State);
        committing -> settle_recovery_actors(State, restart)
    end,
    _ = catch resume_pending_purge(),
    case Phase =:= precommit andalso Safe of
        true ->
            persistent_term:erase(?ACTIVE_KEY),
            ok = ridge_rt:actor_startup(finish), {ok, Stats};
        false ->
            _ = ridge_rt:actor_startup(finish),
            {{error, {reload_recovery_required, restart_node}}, Stats}
    end.

settle_precommit(#{owned := Owned}) ->
    %% The startup gate owns confirmed suspensions during precommit abort.
    %% It may already have resumed them to break a parent's spawn dependency;
    %% sending a second resume could now block in an ordinary user handler.
    Pending = [P || P <- maps:keys(Owned), maps:get(P, Owned) =:= restart],
    {Killed, Safe} = stop_recovery_actors(Pending, reload_incomplete_actor_state),
    case ridge_rt:actor_startup(abort) of
        {ok, Count} -> {#{resumed => Count, restarts => Killed}, Safe};
        {error, recovery_required, Count} -> {#{resumed => Count, restarts => Killed}, false}
    end.

complete_recovery(State) ->
    {Stats, Safe} = settle_recovery_actors(State, ready),
    Result = try
        true = Safe,
        persistent_term:put(?VSN_KEY, maps:get(version, State)),
        Purge = schedule_purge(maps:get(modules, State), maps:get(delay, State)),
        persistent_term:erase(?ACTIVE_KEY),
        ok = ridge_rt:actor_startup(finish),
        {ok, Purge, maps:get(restarts, State) ++ maps:get(restarts, Stats)}
    catch _:_ -> {error, {reload_recovery_required, restart_node}}
    after _ = catch resume_pending_purge()
    end,
    {Result, Stats}.

settle_recovery_actors(#{owned := Owned}, Mode) ->
    {Resume, Restart} = lists:partition(fun(P) ->
        (Mode =:= resume andalso maps:get(P, Owned) =/= restart) orelse
            (Mode =:= ready andalso maps:get(P, Owned) =:= true)
    end, maps:keys(Owned)),
    {Killed, Safe} = stop_recovery_actors(Restart, reload_incomplete_actor_state),
    lists:foldl(fun(P, {Stats, AllSafe}) ->
        case is_process_alive(P) of
            false -> {Stats, AllSafe};
            true ->
                case catch sys:resume(P, ?RECOVERY_TIMEOUT) of
                    ok -> {Stats#{resumed => maps:get(resumed, Stats) + 1}, AllSafe};
                    _ ->
                        {More, Stopped} = stop_recovery_actors([P], reload_resume_failed),
                        {Stats#{restarts => More ++ maps:get(restarts, Stats)}, AllSafe andalso Stopped}
                end
        end
    end, {#{resumed => 0, restarts => Killed}, Safe}, Resume).

stop_recovery_actors(Pids, Reason) ->
    Entries = [{monitor(process, P), P, initial_call_module(P)} || P <- Pids, is_process_alive(P)],
    [exit(P, kill) || {_, P, _} <- Entries],
    Pending = maps:from_list([{Ref, P} || {Ref, P, _} <- Entries]),
    Safe = await_recovery_stops(Pending, erlang:monotonic_time(millisecond) + ?RECOVERY_TIMEOUT),
    {[#{module => M, reason => Reason} || {_, _, M} <- Entries], Safe}.

await_recovery_stops(Pending, _) when map_size(Pending) =:= 0 -> true;
await_recovery_stops(Pending, Deadline) ->
    Left = max(0, Deadline - erlang:monotonic_time(millisecond)),
    receive
        {'DOWN', Ref, process, _, _} when is_map_key(Ref, Pending) ->
            await_recovery_stops(maps:remove(Ref, Pending), Deadline)
    after Left ->
        [demonitor(Ref, [flush]) || Ref <- maps:keys(Pending)], false
    end.

recovery_notice(#{phase := Phase, restarts := PriorRestarts}, Cause, Stats) ->
    %% Logging must not hold the recovery lifetime or echo an arbitrary
    %% process exit reason. This report contains only phases and counts.
    _ = spawn(fun() -> logger:warning(#{event => reload_recovered, phase => Phase,
        cause => Cause, actors_resumed => maps:get(resumed, Stats),
        actors_restarted => length(PriorRestarts) + length(maps:get(restarts, Stats))}) end),
    ok.

%% Dev transport: the new code is on the node's own code path. A module
%% holds at most two loaded versions, and neither load_file nor
%% load_binary purges by itself: the second upgrade on a node fails with
%% not_purged unless the oldest version is dropped first. The actors are
%% already suspended, so nothing executes the old code and the soft purge
%% succeeds.
load_from_path(Modules, _) ->
    [_ = code:soft_purge(M) || M <- Modules],
    [{module, M} = code:load_file(M) || M <- Modules],
    ok.

%% Production transport: replace the loaded code straight from the shipped
%% blobs — no temp files or code-path changes. Preparation checked all
%% bytecode before actor suspension; finishing commits the whole batch or
%% leaves every current module unchanged. Modules with on_load are rejected.
load_prepared(Modules, Prepared) ->
    [_ = code:soft_purge(M) || M <- Modules],
    code:finish_loading(Prepared).

load_and_invalidate(Modules, LoadFun, LoadCtx, #{atomic_load := true}) ->
    case LoadFun(Modules, LoadCtx) of
        ok -> ridge_rt:invalidate_record_versions(Modules);
        {error, _} = Rejected -> Rejected
    end;
load_and_invalidate(Modules, LoadFun, LoadCtx, _) ->
    try LoadFun(Modules, LoadCtx)
    after
        %% Dev loads are sequential: invalidate even after a partial swap.
        ok = ridge_rt:invalidate_record_versions(Modules)
    end.

beam_label(M) ->
    atom_to_list(M) ++ ".beam (ridge reload bundle)".

%% ── Quiescence purge ────────────────────────────────────────────────────────
%%
%% A loaded module keeps TWO code versions until the old one is purged.
%% After a successful upgrade the old code may still be referenced (a
%% suspended-then-resumed actor finishing a call, a fun captured in a
%% message). A window schedules a SAFE cleanup attempt, not permission to
%% kill holders. One worker keeps deadlines per module across bundles.
%% Failed soft purges remain pending until a later enabled bundle retries
%% them; there is no unbounded periodic process scan.

-define(PURGE_REG, ridge_loader_purge).

schedule_purge(Modules, DelayMs) ->
    Status = purge_call({schedule, Modules, DelayMs}),
    #{scheduled => DelayMs > 0, after_ms => DelayMs, policy => soft,
      pending => maps:get(pending, Status)}.

%% Node-local diagnostic state, not a durable operation receipt. Pending
%% means the last safe attempt found references; waiting has a deadline.
purge_status() -> purge_call(status).

pause_pending_purge() -> purge_call(pause).
resume_pending_purge() -> purge_call(resume).

purge_call(Request) ->
    Pid = purge_worker(), Ref = monitor(process, Pid),
    Pid ! {purge_call, self(), Ref, Request},
    receive
        {Ref, Reply} -> demonitor(Ref, [flush]), Reply;
        {'DOWN', Ref, process, Pid, Reason} -> error({purge_worker_down, Reason})
    after 5000 ->
        demonitor(Ref, [flush]), error(purge_worker_timeout)
    end.

purge_worker() ->
    case whereis(?PURGE_REG) of
        undefined ->
            Parent = self(), Tag = make_ref(),
            {Pid, Ref} = spawn_monitor(fun() ->
                case catch register(?PURGE_REG, self()) of
                    true -> Parent ! {Tag, self()}, purge_loop(#{}, #{});
                    _ -> ok
                end
            end),
            receive
                {Tag, Pid} -> demonitor(Ref, [flush]), Pid;
                {'DOWN', Ref, process, Pid, _} -> purge_worker()
            after 5000 ->
                demonitor(Ref, [flush]), error(purge_worker_start_timeout)
            end;
        Pid -> Pid
    end.

purge_loop(Jobs, Pauses) ->
    Wait = purge_wait(Jobs, Pauses),
    receive
        {purge_call, From, Ref, status} ->
            From ! {Ref, cleanup_status(Jobs, Pauses)}, purge_loop(Jobs, Pauses);
        {purge_call, From, Ref, pause} ->
            Next = case maps:is_key(From, Pauses) of
                true -> Pauses;
                false -> Pauses#{From => monitor(process, From)}
            end,
            From ! {Ref, ok}, purge_loop(Jobs, Next);
        {purge_call, From, Ref, resume} ->
            Next = release_purge_pause(From, Pauses),
            From ! {Ref, ok}, purge_loop(Jobs, Next);
        {purge_call, From, Ref, {schedule, Modules, DelayMs}} ->
            Due = erlang:monotonic_time(millisecond) + DelayMs,
            %% Retry retained modules only when an enabled bundle asks for
            %% cleanup. Unrelated waiting jobs keep their original deadlines.
            Retried = maps:map(fun(_, pending) when DelayMs > 0 -> Due;
                                 (_, Value) -> Value end, Jobs),
            NextJobs = lists:foldl(fun(M, Acc) ->
                case DelayMs of 0 -> maps:remove(M, Acc); _ -> Acc#{M => Due} end
            end, Retried, Modules),
            NextPauses = release_purge_pause(From, Pauses),
            From ! {Ref, cleanup_status(NextJobs, NextPauses)},
            purge_loop(NextJobs, NextPauses);
        {'DOWN', Ref, process, From, _} ->
            Next = case maps:find(From, Pauses) of
                {ok, Ref} -> maps:remove(From, Pauses);
                _ -> Pauses
            end,
            purge_loop(Jobs, Next);
        _ -> purge_loop(Jobs, Pauses)
    after Wait ->
        Now = erlang:monotonic_time(millisecond),
        NextJobs = maps:fold(fun(M, Due, Acc) when is_integer(Due), Due =< Now ->
            case code:soft_purge(M) of
                true -> maps:remove(M, Acc);
                false -> Acc#{M => pending}
            end;
            (_, _, Acc) -> Acc
        end, Jobs, Jobs),
        purge_loop(NextJobs, Pauses)
    end.

release_purge_pause(From, Pauses) ->
    case maps:take(From, Pauses) of
        {Ref, Next} -> demonitor(Ref, [flush]), Next;
        error -> Pauses
    end.

purge_wait(_, Pauses) when map_size(Pauses) > 0 -> infinity;
purge_wait(Jobs, _) ->
    case [Due || Due <- maps:values(Jobs), is_integer(Due)] of
        [] -> infinity;
        Deadlines ->
            %% Long deadlines use bounded receives instead of crashing the
            %% tracking worker on the runtime's receive-timeout limit.
            min(16#FFFFFFFF, max(0, lists:min(Deadlines) - erlang:monotonic_time(millisecond)))
    end.

cleanup_status(Jobs, Pauses) ->
    #{waiting => lists:sort([atom_to_binary(M) || {M, Due} <- maps:to_list(Jobs), is_integer(Due)]),
      pending => lists:sort([atom_to_binary(M) || {M, pending} <- maps:to_list(Jobs)]),
      paused => map_size(Pauses) > 0}.

%% The callback module of a gen_server, via the '$initial_call' dictionary
%% entry gen_server plants at boot. Returns undefined for non-gen_servers.
initial_call_module(P) ->
    case process_info(P, dictionary) of
        {dictionary, Dict} ->
            case lists:keyfind('$initial_call', 1, Dict) of
                {'$initial_call', {M, init, 1}} -> M;
                _ -> undefined
            end;
        _ -> undefined
    end.

upgrade_actor(P, Modules) ->
    M = initial_call_module(P),
    lists:member(M, Modules) orelse ridge_actor(M).

startup_actor_candidates(Modules) ->
    Candidates = [P || P <- processes(), upgrade_actor(P, Modules)],
    case lists:any(fun(P) ->
        M = initial_call_module(P),
        ridge_actor(M) andalso not erlang:function_exported(M, '__ridge_init', 1)
    end, Candidates) of
        true -> {error, actor_rebuild_required};
        false -> {ok, [P || P <- Candidates, startup_actor_ready(P)]}
    end.

startup_actor_ready(P) ->
    case ridge_actor(initial_call_module(P)) of
        false -> true;
        true ->
            case process_info(P, dictionary) of
                {dictionary, Dict} -> lists:keyfind('__ridge_actor_ready', 1, Dict) =:=
                                      {'__ridge_actor_ready', true};
                _ -> false
            end
    end.

ridge_actor(M) -> erlang:function_exported(M, '__ridge_state_fields', 0).

refresh_actor_collections(Pids, Owner) ->
    lists:foldl(fun(P, {Refreshed, Restarts}) ->
        M = initial_call_module(P),
        case is_process_alive(P) andalso ridge_actor(M) of
            false ->
                ok = recovery_call(Owner, {ready, P}),
                {Refreshed, Restarts};
            true ->
                case migrate_actor(P, M, ridge_refresh_collections) of
                    ok ->
                        ok = recovery_call(Owner, {ready, P}),
                        {[P | Refreshed], Restarts};
                    {restarted, Reason} ->
                        {Refreshed, [#{module => M, reason => Reason} | Restarts]}
                end
        end
    end, {[], []}, Pids).

migrate_actors(Pids, ActorMigs, OldFieldsByMod) ->
    lists:foldl(fun(P, {Mig, Rst}) ->
        M = initial_call_module(P),
        case [A || A = #{beam := B} <- ActorMigs, B =:= M] of
            [] ->
                {Mig, Rst};
            [Entry] ->
                Extra = case maps:get(migrate_hook, Entry, false) of
                    true ->
                        %% The actor's new code carries user migrate hooks:
                        %% code_change dispatches on the OLD state-shape hash
                        %% and the hook takes precedence over automatic
                        %% rename/default instructions.
                        {ridge_migrate_hook, maps:get(old_state_hash, Entry, 0)};
                    false ->
                        NewFields = M:'__ridge_state_fields'(),
                        Defaults = M:'__ridge_state_defaults'(),
                        OldFields = maps:get(M, OldFieldsByMod, []),
                        Added = [F || F <- NewFields, not lists:member(F, OldFields)],
                        {ridge_migrate, #{
                            renames => maps:get(renames, Entry),
                            added => Added,
                            fields => NewFields,
                            defaults => Defaults
                        }}
                end,
                case migrate_actor(P, M, Extra) of
                    ok -> {[P | Mig], Rst};
                    {restarted, Reason} -> {Mig, [#{module => M, reason => Reason} | Rst]}
                end
        end
    end, {[], []}, Pids).

%% Blue/green isolation: one actor's failed migration never aborts the
%% upgrade. The migration is retried once (migrations are idempotent), and
%% if it still fails the actor is killed so its supervisor restarts it on
%% init state — it is never resumed with corrupt state. A migrate hook that
%% throws crashes the gen_server inside its own code_change: the actor is
%% already dead by the time we look, and the supervisor restarts it the
%% same way. The failure is reported, not hidden.
migrate_actor(P, M, Extra) ->
    case try_change_code(P, M, Extra) of
        ok ->
            ok;
        {failed, Reason1} ->
            case is_process_alive(P) of
                false ->
                    {restarted, Reason1};
                true ->
                    case try_change_code(P, M, Extra) of
                        ok ->
                            ok;
                        {failed, Reason2} ->
                            exit(P, kill),
                            {restarted, Reason2}
                    end
            end
    end.

try_change_code(P, M, Extra) ->
    try sys:change_code(P, M, old, Extra) of
        ok -> ok;
        {error, Reason} -> {failed, Reason}
    catch
        Class:Reason -> {failed, {Class, Reason}}
    end.
