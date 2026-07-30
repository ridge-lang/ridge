%% ridge_loader — hot code upgrade orchestrator for the Ridge dev loop.
%%
%% Applies an upgrade manifest written by `ridge run --reload`: validates the
%% base version, suspends the affected actors, loads the recompiled modules,
%% migrates actor state through sys:change_code, resumes, and reports.
%%
%% The node tracks the version of the code it runs in
%% persistent_term[?VSN_KEY]; `ridge run --reload` seeds it at boot and this
%% module advances it after every successful upgrade. A manifest whose
%% base_vsn does not match is rejected before anything is touched.
%%
%% Requires OTP 27+ (the `json` module).
-module(ridge_loader).
-export([current_version/0, apply/2]).

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
                apply_manifest(ManifestPath, Started)
            catch
                Class:Reason:Stack ->
                    {error, {upgrade_failed, Class, Reason, Stack}}
            end
    end.

apply_manifest(ManifestPath, Started) ->
    case code:ensure_loaded(json) of
        {module, json} -> ok;
        _ -> erlang:error(ridge_loader_requires_otp_27)
    end,
    {ok, Bin} = file:read_file(ManifestPath),
    Mf = json:decode(Bin),
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
    %% Suspend every actor whose callback module is about to be reloaded, so
    %% no message is processed with mixed code versions mid-upgrade.
    Pids = [P || P <- processes(), lists:member(initial_call_module(P), Modules)],
    [ok = sys:suspend(P) || P <- Pids],
    try
        %% Capture OLD field lists before loading the new code.
        OldFieldsByMod = maps:from_list([{M, M:'__ridge_state_fields'()} || M <- MigMods]),
        [{module, M} = code:load_file(M) || M <- Modules],
        Migrated = migrate_actors(Pids, ActorMigs, OldFieldsByMod),
        [ok = sys:resume(P) || P <- Pids],
        persistent_term:put(?VSN_KEY, NewVsn),
        Duration = erlang:monotonic_time(millisecond) - Started,
        {ok, #{
            modules_loaded => length(Modules),
            actors_suspended => length(Pids),
            actors_migrated => length(Migrated),
            %% Cumulative and lazy: migrations happen as messages arrive AFTER
            %% the upgrade, so this mostly reflects traffic since the previous
            %% reload — the next reload's report includes this window's.
            messages_migrated => ridge_rt:migration_count(),
            duration_ms => Duration
        }}
    catch
        Class:Reason:Stack ->
            %% Suspension is reversible: resume everything and report. The
            %% system keeps running the old code (a failed code:load_file
            %% changes nothing; a failed migration leaves that actor
            %% suspended-then-resumed on its old state).
            [catch sys:resume(P) || P <- Pids],
            erlang:raise(Class, Reason, Stack)
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

migrate_actors(Pids, ActorMigs, OldFieldsByMod) ->
    lists:filtermap(fun(P) ->
        M = initial_call_module(P),
        case [A || A = #{beam := B} <- ActorMigs, B =:= M] of
            [] ->
                false;
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
                case sys:change_code(P, M, old, Extra) of
                    ok -> {true, P};
                    {error, Reason} -> erlang:error({migration_failed, M, Reason})
                end
        end
    end, Pids).
