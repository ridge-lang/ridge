# BEAM gate

## CI path selection controls

Run the workflow selection checks with Node 24 and npm:

```sh
npm ci --ignore-scripts --prefix tools/testing
npm test --prefix tools/testing
```

These controls read the actual CI and Bench workflow filters and evaluate them
with the glob matcher used by `dorny/paths-filter`. CI runs them in a separate,
unfiltered job so a broken filter cannot skip its own regression checks.
Executable example changes activate the workspace and BEAM lanes; dogfood
changes activate workspace tests, including formatter corpus properties.
Example/dogfood README and docs-only changes retain their skips. LSP client
acceptance and Layer A
benchmarks use their own fixtures and keep their existing selection.
Selection checks do not execute runtime suites or certify every corpus file;
the formatter properties sample corpus inputs rather than visiting every file.

## Complete BEAM lane

Python 3.11+, Cargo, cargo-nextest and Erlang/OTP are required. Run from the
repository root. Individual Cargo integration targets and the default workspace
tests keep their behavior; grouping is used only by this gate.

```sh
python tools/testing/beam-gate.py build --work target/beam-check-1
python tools/testing/beam-gate.py run --work target/beam-check-1 --require-postgres
```

Use a new work directory for each build. The build writes its native inventory,
dependency feature comparison, commands and generated workspace there. The run
uses that plan, checks that source and generated configuration have not changed,
then executes the grouped fixtures and the original driver, CLI and codegen
targets retained in the plan, using nextest's process-per-test isolation. The
original Postgres group keeps its concurrency limit of one. Set
`RIDGE_TEST_PG_URL` to a TLS-enabled fixture using
the schema in `.github/workflows/ci.yml`. `--require-postgres` rejects service
guard skips; without it a local run records unavailable database coverage.

This command covers the BEAM lane. Workspace tests, both workspace Clippy
configurations, stdlib self-compilation and LSP acceptance remain separate CI
steps. Grouped harnesses also inherit workspace lints, profiles, locked registry
versions and the effective Clippy configuration, and receive their own lint pass.

The separate stdlib self-compilation test derives its expected identities from
the versioned `.ridge` sources, independently of the runner's discovery and
embedded source table. It reconciles one `ridge test --stdlib` execution with
that inventory, including multiplicity, progress and summary consistency. Exit
status, zero failures and rejections, and the existing suite-size floor remain
required. The `stdlib_e2e` target also runs controls for source discovery,
omitted, unexpected and repeated identities, and incomplete execution output.

A supplementary `runtime-surfaces` phase runs the complete CLI `run` target with
`beam-runtime` and `cli-watch`, plus the `ridge-bench` `layer_b` target with
`beam-runtime`. It lists and rechecks their native inventory and requires the
watch-cycle and Layer B oracles to be active. Its feature context is separate
from the original/grouped driver context and its reusable catalogs. The CLI
`run` target is intentionally exercised in both feature configurations. The slow
watch stress test remains explicitly ignored. Each phase retains its own command
timing and execution evidence; inventory-only builds omit this runtime phase.

Cargo metadata supplies the driver's effective package version, edition,
minimum Rust version and target directory. Driver build scripts, package-specific
profile overrides, driver-specific lint settings and driver-local Clippy files
require a generator review instead of silently changing the compilation context.
Generated Rust paths support Unicode and use Rust's own string escape syntax.

## Native inventory review

`driver-groups.json` partitions every driver integration source exactly once.
Fixtures that depend on their original crate context, shared helpers, global
process state or database service remain in the driver package. New sources or
new context dependencies require a partition review.

Reusable catalogs live in `inventories/<native-target-triple>.json`. They contain
the selected feature, normalized source hashes, complete native test identities
and ignored states for the grouped fixtures only. Other suites are always listed
natively in their original packages. No Windows catalog stands in for Unix.

If a catalog is missing or `--audit` is supplied, the build compiles and lists
the original eligible targets as well. With a compatible catalog, only changed
or newly grouped sources need their original targets rebuilt and listed; cases
from unchanged source hashes retain their reviewed reference. Both directions
of case parity, ignored-state parity and duplicate identity checks must pass.
It writes `inventory-candidate.json`; it does **not** update a reviewed catalog.
The first build on an unaudited platform therefore retains the original linking
cost in addition to the grouped harness build. Only subsequent builds using a
reviewed matching catalog can avoid that original compilation.

To add or update a catalog:

1. Run `build --audit` on the native platform. Use `--inventory-only` when only
   the grouped sources need auditing; that plan cannot execute the complete gate.
2. Review the original and grouped native listings, the candidate case/ignored
   changes and the fixture changes against their intended assertions.
3. Copy the reviewed candidate to `inventories/<target-triple>.json` in the same
   change. CI verifies its sources, target, feature and actual grouped identities.

CI uploads JSON evidence, unapproved candidates and complete command stdout/stderr
for each platform/OTP job. Failed commands also print both streams in full so
the initial assertion remains visible even when later tests produce long output.
Never manufacture a catalog by editing counts or reusing a different platform.
The dependency graph comparison is target-specific and rejects feature drift
between the original joint codegen/driver/CLI selection and the generated package.
The harness includes those roots and their development dependencies; retained
original targets keep the same joint selection. Native audits also select root
libraries and binaries so development dependencies remain active when only some
integration targets need listing. Catalog schema 2 records this joint context;
catalogs from the previous selection of the driver alone require a fresh native
audit.

## Local command coordination

On machines with an external Rust coordinator, `--command-prefix prefix.json`
prepends a JSON argv array to each individual Rust command. Arguments, cwd and
the environment are otherwise inherited. The orchestrator itself must not hold
a Rust lease across all phases. CI runs directly on its isolated runners.

Run the tooling controls without compiling Rust:

```sh
python -m unittest discover -s tools/testing -p test_beam_gate.py -v
```
