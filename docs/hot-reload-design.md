# Hot-reload contract and backend design

`ridge run --reload` applies compatible edits to a persistent development
runtime. `ridge reload --node` applies a prepared bundle to a running target.
Both paths support body changes, compatible state changes and explicit pure
migration hooks, with the startup and recovery boundaries described below.
`ridge reload --check` reports incompatible changes before applying them.
The Erlang backend requires OTP 27 or later, with current security patches.

Successful application confirms completion of the reload operation. It does
not establish a transaction over actor state, queued messages, external effects
or multiple nodes, or confirm that a replacement actor is serving requests.
The atomic boundary is the prepared module batch. Actor recovery proceeds by
phase, and a failed actor needs application-configured supervision to return.
Runtime and stdlib changes require a cold rebuild and restart.

Records carry version identity and are migrated lazily when stale values are
visited. Explicit history and derived migration edges support compatible
evolution. Values with no applicable migration chain are reported and dropped
at receive time. Pure migration hooks can still throw or fail to terminate;
pure typing does not imply a successful or reversible execution.

## Code commit and actor recovery

| Phase or outcome | What can be concluded |
| --- | --- |
| Preflight rejection | No application module commit, actor migration, version publication or cleanup scheduling change was made by this request. |
| Load permission granted, load unconfirmed | Effects may have started; recovery retains a fence instead of trusting the old baseline. |
| Code commit confirmed | The new module batch is current; actor migration and refresh can still be in progress. |
| Applied operation | The migration/recovery stage finished, including reported actor fallbacks; replacement availability is unobserved. |
| Cleanup acknowledged | The cleanup policy was registered, possibly disabled; old-code removal is not confirmed. |
| Client result missing | Query the reserved operation; lack of acknowledgement does not authorize replay or establish rejection. |

Production bundles are checked before actors are suspended: manifest and option
types, unique module and actor names, rename entries, exact correspondence
between modules and blobs, and actual bytecode/module identity. Missing, extra,
duplicate or malformed entries are rejected with the affected field or module.
A preflight rejection leaves current code, version, evidence generations and
existing purge scheduling unchanged.

The BEAM loader prepares the bytecode before suspension and commits the module
batch atomically. Modules with `on_load` callbacks are rejected by this path;
stdlib/runtime reload remains deferred. If the commit fails (for example,
because old code is still in use), no current module is replaced and evidence
stays current. Cleanup is paused while code is replaced and resumes on
rejection, preserving unrelated modules' deadlines.
The dev path reads the complete module set from disk and prepares the same
atomic batch before actor suspension. Missing or corrupt later files cannot
leave earlier modules replaced. Runtime and stdlib modules require a cold
rebuild and restart; sharing batch preparation does not enable their reload.
The watcher retains its running baseline only after success or an explicit
rejection before code replacement. Any other failure, lost acknowledgement or
missing probe verdict invalidates its in-memory and on-disk baseline and pauses
compilation and reload planning. Press `r` + Enter to rebuild and restart before
another diff; editing alone does not replay an uncertain operation.

Both transports share one admission queue on the target runtime. Each request
checks its base version after entering admission, and retains admission through
actor coordination and version publication. After one request commits, a queued
request against its old base is rejected before it suspends actors or loads code.
An admitted reload continues if its client dies or times out; a timeout is not
cancellation and must not trigger an automatic replay. Dead local callers whose
requests have not been admitted are skipped.

The `--seed` option permits an initially unknown target version only inside this
same admission boundary. It never resets an existing marker, and a rejected
first bundle leaves the marker unknown. This requires the updated target loader;
the CLI no longer seeds by writing a marker through a separate RPC.

Preflight rejection releases admission. Actor coordination has a separate,
unlinked recovery owner that monitors the applicator and records suspension
intents before sending system requests. If the applicator dies before permission
to load code, the owner resumes the original actors with their old state. A
suspension or resume that cannot be confirmed within five seconds terminates
that original process instead; a supervisor can start a replacement.

The owner acknowledges permission before code loading begins. If the applicator
dies after that permission but before confirming the complete load and evidence
invalidation, the outcome is uncertain. The owner terminates the suspended
cohort and retains a node-local recovery fence: subsequent reloads return
`reload_recovery_required` and require a node restart. It never resumes old
state under potentially different handlers or publishes an unconfirmed version.

After loading is confirmed, recovery proceeds forward. An actor is marked ready
only after state migration and collection refresh both succeed. Recovery resumes
ready actors, terminates incomplete original processes, publishes the loaded
version and schedules safe cleanup. Supervisor replacements use the new code's
initial state; they do not inherit lost state. Recovery targets original PIDs,
so it does not terminate their replacements. While the owner exists, another
reload is rejected with `recovery_in_progress`, without removing its fence.
Recovery emits a `reload_recovered` logger report with phase, cause and actor
counts, omitting application state and arbitrary process exit details.

If the recovery owner itself dies while the applicator survives, the applicator
terminates the original cohort and retains the fence. Simultaneous death of both
processes, a failed VM, and uninterruptible native code require operator recovery;
this is not a durable journal or a universal rollback mechanism.
Admission transport errors report a short cause and omit the submitted bundle
and arbitrary coordinator exit details from the response.

Actor initialization shares a startup boundary with both reload transports.
The child holds its initialization permission for the entire user initializer,
including ordinary unlinked spawns, supervised starts and supervisor restarts.
A reload acquires its startup session before taking the actor census. If an
initializer is already running, the reload rejects with
`{reload_rejected, startup, initializing}` before suspension or loading.

A new initializer arriving before load permission cancels that reload, which
returns `{reload_rejected, startup, actor_started}` with the old version intact.
Confirmed suspensions are resumed independently of the recovery owner so an
initializer can call an existing actor even while the owner is waiting to
suspend its parent. The initializer runs once; the loader never retries it.
Cancellation waits for all resume requests to finish before releasing the
startup session. A resume timeout terminates the original process and retains
a restart-required fence, preventing a late resume from crossing another load.

After load permission, new children wait until the recovery owner resolves the
original cohort and publishes the version. They then call the current module's
initializer, rather than an old captured function. Queued initialization
arguments undergo record migration and collection refresh before user code
runs; migration failure fails startup without running that initializer.
An uncertain load or loss of the startup coordinator refuses new initializers
and further reloads until the node is restarted. The coordinator is not silently
recreated after losing its journal.

This requires actors and the runtime built with the startup protocol. The
production CLI checks target support before compiling, including when target
and CLI share an output directory; unsupported targets must be rebuilt and
restarted. The apply RPC also carries the required protocol through a versioned
entry point absent from older loaders, so a target replaced after the initial
check cannot fall back to the old application path. Existing legacy actors and
incoming legacy actor bundles are rejected
before effects. This is a runtime compatibility boundary, not a source-language
change.

Deferred cleanup uses only `code:soft_purge/1`; a referenced old version is
retained rather than forcibly purged. A node-local worker tracks one deadline
per module, preserving unrelated waiting jobs across bundles. Failed safe
attempts become pending, with no periodic retry loop. A later successful bundle
with cleanup enabled rearms pending jobs after its window; its own module
deadlines are replaced. Disabling cleanup removes jobs for the newly upgraded
modules while preserving unrelated obligations. The worker monitors callers
that pause cleanup and releases their pauses if they die. Actor recovery and
upgrade admission are managed separately.

Reload timer arguments are checked before workspace access, locking, target
probes, pending-operation recovery or compilation, including direct CLI-library
callers. `--timeout` accepts 0 through 4294967295 milliseconds; zero requests a
nonblocking RPC, not cancellation of remote work. `--purge-after` accepts 0
through 18446744073709551 whole seconds; zero disables cleanup. The seconds-to-
milliseconds conversion is checked. The loader accepts `purge_after_ms` as an
integer from 0 through 18446744073709551615 and rejects other values before code
commit. Long cleanup windows use bounded receive intervals, so the RPC timeout
limit does not cap a cleanup window.

Operators can query `ridge_loader:purge_status/0` on the target node. It returns
`waiting` and `pending` module-name lists, a `paused` flag and `tracking_lost`.
Pending means the last safe attempt found references; it is not a live
reference census. The
apply report keeps `scheduled` and `after_ms` and adds `policy: soft` and the
currently pending module names. `scheduled` acknowledges the current cleanup
policy; it does not guarantee future worker availability or completed purging.

If the tracking worker dies, an independent observer emits the best-effort
`reload_cleanup_tracker_down` warning without reflecting arbitrary exit payloads.
A replacement worker starts with empty task lists and `tracking_lost: true`;
those lists describe only its known tasks, not the lost history. This flag also
appears in subsequent apply reports and remains set until the VM restarts,
including when a new bundle disables cleanup. The CLI surfaces it in the human
summary. It does not invalidate an otherwise confirmed code/migration result.
Lost jobs are not reconstructed or forcibly purged. Monitoring and the history
marker are node-local, not durable across VM or whole-runtime failure.

Task state is local to the worker, not a durable upgrade receipt. Keeping
referenced code may retain memory and prevent another version from loading. Let holders
finish, then retry the upgrade, or deliberately restart the node; cleanup
never treats an expired delay as permission to kill them.

This behavior requires the target node to boot with the updated loader. An
application bundle does not replace the node's loader or runtime. Older loaders
can still use destructive cleanup; when a target report lacks `policy: soft`,
the CLI labels the cleanup policy unconfirmed instead of describing it as safe.
Updating the CLI alone does not change the target's cleanup behavior.

Actor migration and reporting follow the atomic code commit. Before a confirmed
load, recovery must not claim the new version or resume potentially incompatible
state. After confirmation, recovery proceeds forward: ready actors resume,
incomplete originals are terminated, and the loaded version is published when
recovery can finish. If that cannot be established, the recovery fence remains.
An error response alone does not establish which phase completed.

## Operation identity and local baseline recovery

The production CLI reserves an operation before applying a bundle. The target
issues its ID and binds it to a SHA-256 digest of the exact manifest, beam blobs
and options. The registry incarnation is checked at reservation, submission and
query. Repeating the same submission returns its receipt without executing the
migration again; different content for that ID is rejected. Unknown IDs cannot
be submitted, including after their receipts expire.

Queries are independent of admission, so a blocked migration does not block
status queries. Receipts distinguish `reserved`, `in_progress`, `rejected`,
`applied` and `recovery_required`. Code commit confirmation, actor migration
completion and cleanup acknowledgement are separate fields. Acknowledgement
means the cleanup policy was registered, including when disabled; it does not
mean every old-code reference has disappeared. A failed worker is never replayed.

The registry retains at most 128 receipts. Reserved and terminal receipts expire
after 15 minutes; terminal retention starts when the result is recorded. Active
receipts marked `in_progress` are not evicted. Capacity exhaustion rejects
reservations. Receipts
are node-local and not durable: a VM restart or registry loss changes its
incarnation. Missing or expired receipts remain unknown. Reports retain aggregate
counts and at most 64 module names for actor fallbacks, with generic failure categories
and an explicit truncation flag, rather than arbitrary callback errors or actor
state. Detailed callback error terms are not retained in receipts.

Before dispatch, the CLI synchronizes a `reload-pending.json` journal beside the
selected profile's snapshot, retaining the previous and candidate snapshots and
operation identity, without cookies or bytecode. An OS file lock excludes another
production reload in that workspace/profile and is released on process death.
Payload files are frozen for reservation and submission. The journal is local
recovery metadata, not a durable transaction with the target.

A confirmed rejected receipt restores the previous snapshot. A confirmed applied
receipt restores the candidate only if identity, digest, incarnation and current
target version agree and no recovery fence is active. Otherwise the CLI
invalidates the planning snapshot and retains the journal. Running the same
`ridge reload --node ...` command queries the pending operation before compiling,
even if source has since changed. Successful recovery finishes the previous
operation; invoke reload again to apply further edits. It never retries a
migration to obtain a missing result.
If the original CLI stopped before submission, recovery atomically cancels its
still-reserved operation before restoring the previous snapshot. A late submit
then returns that rejection. Cancellation cannot stop an operation already
dispatched to admission.

If the node or registry restarted, a receipt expired, or recovery is required,
querying cannot prove the outcome. Rebuild and restart deliberately, then remove
the old `target/ridge/<profile>/reload-pending.json` before establishing a fresh
reload baseline. Do not delete a pending journal merely to bypass an in-progress
operation. Development watcher uncertainty retains its manual restart contract.

## Actor outcomes and report compatibility

A failed actor migration is retried once if the original process is still
alive. If it fails again, the loader requests termination; it never intentionally
resumes that actor with an unsuccessful migration. The recovery owner also
terminates originals whose suspension, refresh or resume cannot be completed
safely. A dead original is not a confirmed replacement. A bare spawn stays dead;
a configured supervisor may replace it according to its policy and restart
intensity. Even a permanent child can fail to initialize or exhaust its restart
budget. A replacement starts from its new initializer; previous state and queued
messages are not transferred. Handles bound directly to the original process
do not automatically identify a replacement; supervision handles follow their
own resolution contract.
This is failure isolation, not a blue/green deployment or rollback.

Apply reports and `reload_recovered` events include `actor_fallback`:

| Field | Observation |
| --- | --- |
| `affected` | Unique original actors for which the loader recorded a fallback. |
| `termination_requested` | Those originals for which the loader sent a termination request. |
| `terminated` | Those originals observed dead when this report was constructed. |
| `replacement_status` | `not_required` when no fallback was recorded; otherwise `not_observed`. |

The loader does not track replacement identities or readiness, even when a
supervisor has already created a replacement by the time a caller reads the
report. Availability must be established through the application's supervision
and readiness checks. The human summaries in both CLI modes state this limit.

For example, this excerpt records an original that died without a termination
request from the loader. It does not establish whether a supervisor replaced it:

```json
{
  "actor_fallback": {
    "affected": 1,
    "termination_requested": 0,
    "terminated": 1,
    "replacement_status": "not_observed"
  },
  "actors_restarted": 1
}
```

`actors_restarted` and `restarts` remain deprecated legacy fields with their
existing values and entry shape. They count fallback events, not confirmed
restarts or necessarily unique actors. Direct loader calls retain their legacy
diagnostic terms; operation receipts retain only bounded module names and generic
reason text. No private PID bookkeeping is added to either report. Consumers
should use `actor_fallback` for observations and accept additive report fields.
Consumers that enforce an exact JSON object shape must update their schemas.
Older reports without `actor_fallback` are labeled unconfirmed by the CLI; it
does not infer termination counts from `actors_restarted`.

The legacy `messages_migrated` field is a snapshot of `migration_count/0`, a
node-local diagnostic counter of attempted record-migration edges. It increments
before invoking a migration hook, including an attempt that throws. Nested data
and multi-hop chains can contribute several increments for one message; lazy
migration may happen after the upgrade, and other stale retained values can
contribute too. The counter is not reset at reload boundaries and concurrent
updates are not atomic accounting. It is neither a count of delivered messages
nor an exact measure of work or failures attributable to this operation. The CLI
labels it `record-migration attempts (node counter)`.

## Trust and secure transport

Bundle validation does not sandbox bytecode or replace the trust requirements
of Erlang distribution: connected peers can execute code on the node.
Only trusted build artifacts and authorized peers may participate. A version
marker or compatibility hash identifies a baseline; it is not an artifact
signature. The operation digest binds submitted bytes to an operation and is
not a publisher identity or authorization mechanism.

The default cookie used by the development loop is not a production credential.
Use a dedicated cookie, protect access to it (including local process arguments),
restrict network access to distribution and discovery ports, and run a supported,
patched OTP release. Cookie authentication does not encrypt traffic or validate
certificates. Ridge does not provision TLS certificates or select a secure
distribution configuration for the operator.

For this backend, configure SSL boot support and `-proto_dist inet_tls` on both
the target and CLI probe, with `-ssl_dist_optfile` supplying client and server
options. Require peer verification against the intended CA and identity, and
require client certificates on the server. The CLI inherits `ERL_FLAGS`; it must
receive the same transport setup, including any required boot configuration.
Account for its short-lived probe node names when establishing certificate
identities and node restrictions. Follow the [OTP TLS distribution guide](https://www.erlang.org/doc/apps/ssl/ssl_distribution.html)
for boot scripts, certificate verification and options; the flags alone do not
establish a trusted channel. Keep track of [vendor security advisories](https://github.com/erlang/otp/security/advisories)
for the deployed OTP and native dependencies.

## Retained values and compatibility boundaries

During a successful reload, collection indexes held in actor state are refreshed
after state-schema migration and before the actors resume. This includes maps
and sets nested in lists or records, even when the actor's own code did not
change. Comparison implementations can depend on other reloaded modules, so
the refresh includes all live Ridge actors. This work increases the reload
pause; it does not add a state traversal to every message.

The upgrade report keeps `actors_migrated` for successful state-schema
migrations and adds `actors_refreshed` for actors whose collection refresh
completed. The latter counts actors, not changed collections. Refresh failures
follow the existing retry and actor-restart policy. Empty collections retain no
comparison-provider dependency.

Retained comparison snapshots identify the runtime incarnation as well as its
local code epoch. A foreign collection is rebound to the receiving runtime's
current providers before its index is used; matching local epoch numbers alone
do not make evidence current.

Refreshing actor state does not replace other aliases of an immutable
collection. An old collection retained outside that state is refreshed when
accessed. The runtime can reuse a migrated data index for repeated reads of
the same immutable snapshot in one process and code epoch. This private cache
holds at most four snapshots under a 1 MiB weighted retention budget; it stores
neither comparison dictionaries nor data containing functions, process handles
or references. Binary slices are charged for their backing storage. New writes
receive distinct snapshot identities, so one alias cannot change another.
Eviction, larger values, excluded values and legacy snapshots use the ordinary
migration path and can repeat the refresh work. The budget is a conservative
term charge, not an exact process-heap measurement or a language-level limit.

## Supported evolution and remaining verification

### State migration

Record and actor migration are implemented through explicit history and checked
structural changes. `ridge reload --check` classifies changes using the stored
baseline and newly compiled program. It can offer migration scaffolds; an
incompatible verdict requires a cold restart. Adding history does not imply that
arbitrary old closures or values held outside the migrated state were rewritten.

### Capability re-checking and Type-compatibility

Compilation checks the candidate program's capabilities, while reload planning
compares its exported contract with the baseline. Changing a public return type
or capability contract cannot be treated as a harmless module-body replacement.
The extended verification of historical type resolution, constrained callable
schemes, opaque types, removed modules, retained values and editor history is
tracked in [the compatibility matrix](https://github.com/ridge-lang/ridge/issues/734).
Each accepted case needs a runtime semantic oracle; additional test counts alone
do not establish compatibility for every historical value.

### Runtime and stdlib hot-reload

Runtime and stdlib replacement remain outside the supported application reload
contract. Their dependencies, generated consumers and initialization requirements
need a cold rebuild and restart. The production restriction on `on_load` preserves
the prepared atomic batch; loading those modules sequentially would weaken it.
Supporting another initialization protocol or scoped state rollback requires a
separate design. A saved state cannot undo messages or external effects that
other processes or services have already observed.

Reload scans the runtime and suspends/refreshes live Ridge actors, including
unchanged callbacks whose retained collections depend on changed providers.
No general pause, memory or availability budget is established by a successful
functional test. Reproducible [load and recovery measurements](https://github.com/ridge-lang/ridge/issues/733)
must separate planning, transport, suspension, code commit, migration, collection
refresh and resumption, with semantic oracles under startup and termination.
Historical prototype timings are not a guarantee for the current implementation.

The [original exploratory questions](https://github.com/ridge-lang/ridge/blob/42c61efb1777295b6f11acd27d33af4d780f8f5c/docs/hot-reload-design.md#L273)
remain in version history; they do not describe the current supported surface.
