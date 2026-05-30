# Benchmark results

Tracking data for the compile-pipeline and BEAM benchmarks in
[`crates/ridge-bench`](../crates/ridge-bench). One JSON record per benchmark:

```json
{"version":"0.2.10","sha":"","layer":"B","bench":"bench_list_sum_10k","median_ns":102400,"p99_ns":204800,"iters":199}
```

| field | meaning |
|---|---|
| `version` | workspace version the run was taken at |
| `sha` | commit the run was taken at (empty for a local snapshot) |
| `layer` | `A` native pipeline · `B`/`C` BEAM micro-benchmarks |
| `bench` | benchmark name |
| `median_ns` / `p99_ns` | per-iteration timing |
| `iters` | timed iterations behind the statistics |

## Layers

- **Layer A** (`check` / `emit_core` criterion groups) is native and
  deterministic. It uses criterion's own saved baseline under
  `target/criterion`, so it is not tracked here.
- **Layer B** runs Ridge-generated code on the BEAM. VM jitter makes absolute
  numbers environment-specific, so comparisons use a wide threshold (10–15%)
  via `ridge_bench::tracking::regressions`.

## Baselines

`baselines/<version>.json` is the reference a run is compared against.

The committed `0.2.10.json` is a **local snapshot** (Windows, OTP 28) taken
while the layer was built — `sha` is empty and the numbers are not portable
across machines. Regenerate it on the pinned CI runner before turning the
comparison into a hard gate; until then the benchmark workflow is
informational (it records and uploads, it does not fail a PR).
