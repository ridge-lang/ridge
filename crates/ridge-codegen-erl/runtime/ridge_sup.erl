%% ridge_sup — OTP supervisor callback for Ridge `std.actor.supervise`.
%%
%% OTP cannot start a supervisor from a bare flags/children pair — a callback
%% module is required — so this module is the trivial passthrough: every
%% decision (strategy, restart intensity, child specs) is computed up front in
%% `ridge_rt:start_supervisor/4` and handed through unchanged.
%%
%% Bundled with ridge-codegen-erl and installed into <out_root>/runtime/,
%% exactly like ridge_main_runner.erl.
-module(ridge_sup).
-behaviour(supervisor).

-export([init/1]).

init({Flags, Children}) ->
    {ok, {Flags, Children}}.
