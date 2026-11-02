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
%%   apply_bundle/3 — production (`ridge reload --node`): the manifest and
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
-export([current_version/0, apply/2, apply_bundle/3]).

-define(VSN_KEY, ridge_loader_vsn).

current_version() ->
    persistent_term:get(?VSN_KEY, undefined).

apply(ManifestPath, ExpectedVsn) ->
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
    Started = erlang:monotonic_time(millisecond),
    case prepare_bundle(ManifestBin, BeamBins, Opts) of
        {error, _} = Rejected -> Rejected;
        {ok, Mf, Prepared, PurgeAfter} ->
            try
                do_apply(Mf, Started, fun load_prepared/2, Prepared,
                         #{purge_after_ms => PurgeAfter, atomic_load => true})
            catch
                Class:Reason:Stack -> {error, {upgrade_failed, Class, Reason, Stack}}
            end
    end;
apply_bundle(_, _, _) ->
    {error, {reload_rejected, preflight, invalid_bundle_arguments}}.

%% Keep preflight exception handling outside actor callbacks: a callback's
%% thrown term must never be mistaken for a rejection before code changed.
prepare_bundle(ManifestBin, BeamBins, Opts) ->
    try
        ExpectedVsn = required_version(base_vsn, Opts),
        case current_version() =:= ExpectedVsn of
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
        {ok, Mf, Prepared, PurgeAfter}
    catch
        throw:{bundle_rejected, Reason} -> {error, {reload_rejected, preflight, Reason}};
        throw:{base_version_mismatch, _, _} = Reason -> {error, Reason};
        Class:Reason:Stack -> {error, {upgrade_failed, Class, Reason, Stack}}
    end.

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
    case current_version() =:= BaseVsn of
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
    Pids = [P || P <- processes(), upgrade_actor(P, Modules)],
    try
        [ok = sys:suspend(P) || P <- Pids],
        %% Capture OLD field lists before loading the new code.
        OldFieldsByMod = maps:from_list([{M, M:'__ridge_state_fields'()} || M <- MigMods]),
        %% A pending quiescence purge must not fire mid-swap: cancel it
        %% before loading (it is rescheduled below if purging is enabled).
        ok = cancel_pending_purge(),
        case load_and_invalidate(Modules, LoadFun, LoadCtx, Opts) of
            {error, Errors} ->
                [ok = sys:resume(P) || P <- Pids, is_process_alive(P)],
                {error, {reload_rejected, load, Errors}};
            ok ->
                {Migrated, SchemaRestarts} = migrate_actors(Pids, ActorMigs, OldFieldsByMod),
                {Refreshed, IndexRestarts} = refresh_actor_collections(Pids),
                Restarts = SchemaRestarts ++ IndexRestarts,
                %% A restarted actor is dead by now (killed below, or crashed inside
                %% its own code_change) — resume only the survivors.
                [ok = sys:resume(P) || P <- Pids, is_process_alive(P)],
                persistent_term:put(?VSN_KEY, NewVsn),
                Duration = erlang:monotonic_time(millisecond) - Started,
                PurgeAfter = maps:get(purge_after_ms, Opts, 0),
                Purge = case PurgeAfter of
                    0 ->
                        #{scheduled => false, after_ms => 0};
                    _ ->
                        ok = schedule_purge(Modules, PurgeAfter),
                        #{scheduled => true, after_ms => PurgeAfter}
                end,
                {ok, #{
                    modules_loaded => length(Modules),
                    actors_suspended => length(Pids),
                    actors_migrated => length(Migrated),
                    actors_refreshed => length(Refreshed),
                    actors_restarted => length(Restarts),
                    restarts => lists:reverse(Restarts),
                    purge => Purge,
                    %% Cumulative and lazy: migrations happen as messages arrive AFTER
                    %% the upgrade, so this mostly reflects traffic since the previous
                    %% reload — the next reload's report includes this window's.
                    messages_migrated => ridge_rt:migration_count(),
                    duration_ms => Duration
                }}
        end
    catch
        Class:Reason:Stack ->
            %% Resume survivors and report. A production batch is all-or-none,
            %% but later phases can fail after code changed. Dev loads may be
            %% partial. Metadata is invalidated for every committed code swap.
            [catch sys:resume(P) || P <- Pids],
            erlang:raise(Class, Reason, Stack)
    end.

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
%% message), so the purge is deferred by a quiescence window. The timer
%% runs as a registered process: a new upgrade cancels any pending purge
%% before loading (and reschedules its own on success).

-define(PURGE_REG, ridge_loader_purge).

schedule_purge(Modules, DelayMs) ->
    ok = cancel_pending_purge(),
    Pid = spawn(fun() -> purge_timer(Modules, DelayMs) end),
    true = register(?PURGE_REG, Pid),
    ok.

cancel_pending_purge() ->
    case whereis(?PURGE_REG) of
        undefined ->
            ok;
        P ->
            %% Wait for the old timer to die before returning: the caller
            %% may re-register the name immediately after.
            Ref = monitor(process, P),
            P ! ridge_purge_cancel,
            receive
                {'DOWN', Ref, process, P, _} -> ok
            after 5000 ->
                ok
            end
    end.

purge_timer(Modules, DelayMs) ->
    receive
        ridge_purge_cancel -> ok
    after DelayMs ->
        %% Best-effort: a process still running old code keeps the version
        %% referenced (purge returns false); the next upgrade's purge gets
        %% another shot at it.
        [_ = code:soft_purge(M) || M <- Modules],
        [_ = code:purge(M) || M <- Modules],
        ok
    end.

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

ridge_actor(M) -> erlang:function_exported(M, '__ridge_state_fields', 0).

refresh_actor_collections(Pids) ->
    lists:foldl(fun(P, {Refreshed, Restarts}) ->
        M = initial_call_module(P),
        case is_process_alive(P) andalso ridge_actor(M) of
            false -> {Refreshed, Restarts};
            true ->
                case migrate_actor(P, M, ridge_refresh_collections) of
                    ok -> {[P | Refreshed], Restarts};
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
