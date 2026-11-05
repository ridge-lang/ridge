# Hot-reload design — deferred to 0.2.0+

> **Status note (2026-07):** the dev loop half of this story has landed,
> and so has the versioning machinery. `ridge run --reload` boots a
> persistent dev node and hot-loads compatible edits into it — body
> changes, additive state fields with defaults, and pure state-field
> renames apply in place with actor state preserved; anything else prints
> the `ridge reload --check` report and offers a cold restart. On top of
> that, records and actors now carry real version identity: every record
> shape is content-hashed to a 64-bit value shared between the generated
> beams and the on-disk snapshot, tagged onto each record value at
> construction (`__ridge_v`), and checked at receive time. A type may
> declare explicit history — `type User @version(2) = …` plus a
> `do … end` section with `migrate (old: User@1) -> User = …` hooks — and
> an actor may declare a `migrate` member for its state; when no hook
> exists, the compiler derives structural edges (renames, dropped fields)
> so stale-tagged messages in an actor's mailbox are migrated lazily at
> receive instead of crashing the handler, and a message with no
> applicable edge is reported and dropped, never delivered.
> `ridge reload --check` upgrades to a `migrate-hook` verdict when the
> source already covers the change, and its scaffolds roundtrip (what it
> prints parses and compiles). The runtime loader requires OTP 27+.
> Steady-state cost, measured: ~75 ns per message for the receive-path
> wrapper and ~9 words per tagged record. The production half has landed
> too (2026-07-31): `ridge reload --node` ships a bundle (manifest + beam
> blobs) to a running node over distribution and applies it with
> blue/green migration-failure semantics (retry once, then the supervisor
> restarts that actor on init state while every sibling migrates
> normally), a quiescence purge of the old code, a structured JSON drop
> report for non-migratable messages, and a full JSON upgrade report for
> CI/CD and audit. Still deferred: stdlib reload.

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

Operators can query `ridge_loader:purge_status/0` on the target node. It returns
`waiting` and `pending` module-name lists and a `paused` flag. Pending means the
last safe attempt found references; it is not a live reference census. The
apply report keeps `scheduled` and `after_ms` and adds `policy: soft` and the
currently pending module names. This state is local to the worker, not a durable
upgrade receipt, and does not survive worker or VM restart. Keeping referenced
code may retain memory and prevent another version from loading. Let holders
finish, then retry the upgrade, or deliberately restart the node; cleanup
never treats an expired delay as permission to kill them.

This behavior requires the target node to boot with the updated loader. An
application bundle does not replace the node's loader or runtime. Older loaders
can still use destructive cleanup; when a target report lacks `policy: soft`,
the CLI labels the cleanup policy unconfirmed instead of describing it as safe.
Updating the CLI alone does not change the target's cleanup behavior.

Actor migration and subsequent reporting are separate from the atomic code
commit. A failure after commit can leave changed code with the previous version
label; evidence is invalidated before migration and actors are resumed on the
error path. This is not a rollback transaction for actor state. Individual
migration failures retain the existing retry/restart behavior.

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
counts and at most 64 module names for restarts, with generic failure categories
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

Bundle validation does not sandbox bytecode or replace the trust requirements
of Erlang distribution: connected peers can execute code on the node.

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

The sections below retain the original design questions and their context.

For 0.1.0, the dev-loop substitute is `ridge run --watch` (cheap kill-and-relaunch on file change).  `ridge run --observer` adds the BEAM introspection half (`remsh` / `observer` attach via node name + cookie) at ~1 h cost.  Together these capture roughly 70% of the dev-loop value of true hot-reload at a fraction of the implementation budget, while the harder typed-hot-reload work remains deferred until each of the four questions below has a designed answer.

The shape of hot-reload in Ridge is the question of how `code:load_file/1`-style runtime module replacement composes with the language's static guarantees: capabilities, typed records, and stdlib invariants.  Erlang's hot-reload was designed for a dynamically typed runtime where a `badarg` from an incompatible reload is "just" another dynamic-failure mode; under Ridge's pillars a hot-reload that lets a typed program crash with a runtime type error is the *opposite* of what static typing buys the user.  Each of the four sections below captures one axis of that mismatch.

## State migration semantics for typed records across module versions

Suppose a live Ridge process holds a value of `User { name : Text, email : Text }` and the developer hot-reloads the module to define `User { name : Text, email : Text, role : Role }`.  What happens to the in-flight value?  Erlang's answer is "it stays the old shape; new code that pattern-matches it crashes with `badmatch`".  That answer is intolerable in Ridge — the user typed their program *precisely* to avoid that runtime failure mode.  The 0.2.0+ design must choose between (a) requiring a `migrate : OldUser -> NewUser` function on every breaking record change (high friction, high safety), (b) allowing only additive record changes whose new fields carry a default (low friction, narrow scope), or (c) blocking hot-reload entirely on any breaking record change (zero new mechanism, lowest DX).  None of these is obviously right; the choice depends on what user-feedback says about the typical hot-reload diff.

## Capability re-checking on reload

Capabilities are inferred at compile time from the static call graph (Phase 4 inference).  When module `M` is reloaded with a new body that calls `Io.println` it did not previously call, every dependent module's inferred capability set changes.  Does the runtime re-run capability inference across the whole module graph and *reject* the reload if any dependent module's capability surface widened beyond what its original `forbid` rules allowed?  If yes, the reload becomes O(workspace-size) and may take seconds; if no, a half-reload could leak forbidden capabilities into a process whose `forbid` was meant to shield against exactly that.  The cheap middle path is to re-check only the reloaded module's *direct* capability use and reject reloads that introduce a new capability not in the previous version's set — but this misses transitive widening through unreloaded callers.  This question is the load-bearing one for the security pillar of Ridge: any answer that lets a `forbid net` module silently gain net capability after a reload is wrong.

## Type-compatibility relation between module versions

Even setting aside record-shape changes, two module versions need a precise notion of "compatible enough to reload".  Is `pub fn add (a : Int) (b : Int) -> Int` compatible with `pub fn add (a : Int) (b : Int) -> Result Int Text`?  Depth-subtyping says no (return type changed); named-equivalence says no (the type is a different name); structural equivalence says no (structurally different).  All three answers would block what is, intuitively, a benign refinement.  The 0.2.0+ design must define a formal compatibility relation — likely a variance-aware structural relation that allows return-type narrowing and parameter-type widening but rejects everything else — and ship a `ridge reload --check` companion that reports the compatibility verdict for a proposed change before the reload happens.  Without this, the reload semantics drift into "trust the developer" territory, which is the regime Ridge's pillars exist to leave behind.

## Bootstrap question for stdlib hot-reload

Can `core::Io.println` itself be hot-reloaded?  If module `M` is mid-call to `Io.println` when `core::Io` reloads, the in-flight call holds a closure over the old code; on return it may resume into a stack frame the new code has invalidated.  Erlang's answer is "old-code processes finish on the old version, new processes use the new version, and we GC the old code after some quiescence window".  Ridge's stdlib is more entangled: many stdlib functions are inlined at codegen time (Phase 6 erl-codegen), so a stdlib reload would have to invalidate every consumer's compiled `.beam` and re-emit them — turning a hot-reload into a hot-rebuild.  The pragmatic 0.2.0 answer is likely "stdlib is not hot-reloadable in 0.1.x or 0.2.0; only user modules participate", but this needs to be a *designed* restriction, not an emergent one.

These four open questions are the gating items for any 0.2.0+ hot-reload story; until each has a designed answer, hot-reload remains deferred.
