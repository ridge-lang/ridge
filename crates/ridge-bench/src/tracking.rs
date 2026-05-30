//! Benchmark result tracking and regression comparison.
//!
//! A [`BenchRecord`] is one measurement plus its provenance (version, sha,
//! layer). Layer B/C results come from `ridge_bench_runner`'s JSON lines via
//! [`record_from_line`]; baselines live under `bench-results/baselines/`.
//!
//! [`regressions`] compares a current run against a baseline and reports every
//! benchmark whose median grew by more than a threshold. Layer A leans on
//! criterion's own native baseline instead; this module covers the BEAM layers,
//! where VM jitter means the threshold is wide (10–15%) and gating is opt-in.

use serde::{Deserialize, Serialize};

/// One benchmark measurement with provenance, as stored under `bench-results/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchRecord {
    /// Workspace version the measurement was taken at (e.g. `0.2.10`).
    pub version: String,
    /// Commit sha the measurement was taken at (may be empty for a local snapshot).
    pub sha: String,
    /// Measurement layer: `A` (native pipeline), `B`/`C` (BEAM).
    pub layer: String,
    /// Benchmark name (the `bench_*` function, sans prefix conventions).
    pub bench: String,
    /// Median nanoseconds per iteration.
    pub median_ns: u64,
    /// 99th-percentile nanoseconds per iteration.
    pub p99_ns: u64,
    /// Number of timed iterations the median/p99 were computed from.
    pub iters: u32,
}

impl BenchRecord {
    /// The key a baseline and a current record are matched on.
    const fn key(&self) -> (&str, &str) {
        (self.layer.as_str(), self.bench.as_str())
    }
}

/// A benchmark whose current median exceeds its baseline beyond the threshold.
#[derive(Debug, Clone, PartialEq)]
pub struct Regression {
    /// Measurement layer of the regressed benchmark.
    pub layer: String,
    /// Name of the regressed benchmark.
    pub bench: String,
    /// Baseline median (ns).
    pub baseline_ns: u64,
    /// Current median (ns).
    pub current_ns: u64,
    /// `current / baseline` — how much slower this run is.
    pub ratio: f64,
}

/// Parse one `ridge_bench_runner` JSON result line into a [`BenchRecord`],
/// stamping it with the given provenance.
///
/// Returns `None` for a line that is not a successful result — a non-JSON line
/// or the `{"bench":...,"error":true}` marker the runner emits when a benchmark
/// crashes (those carry no `median_ns`).
#[must_use]
pub fn record_from_line(line: &str, version: &str, sha: &str, layer: &str) -> Option<BenchRecord> {
    #[derive(Deserialize)]
    struct Raw {
        bench: String,
        median_ns: Option<u64>,
        p99_ns: Option<u64>,
        iters: Option<u32>,
    }
    let raw: Raw = serde_json::from_str(line.trim()).ok()?;
    Some(BenchRecord {
        version: version.to_owned(),
        sha: sha.to_owned(),
        layer: layer.to_owned(),
        bench: raw.bench,
        median_ns: raw.median_ns?,
        p99_ns: raw.p99_ns?,
        iters: raw.iters?,
    })
}

/// Find every current record that regressed against the baseline by more than
/// `threshold` (a fraction — `0.15` is 15%).
///
/// Records with no matching `(layer, bench)` in the baseline are ignored: a new
/// benchmark has nothing to regress against.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "ns counts are far below f64's 53-bit exact range"
)]
pub fn regressions(
    baseline: &[BenchRecord],
    current: &[BenchRecord],
    threshold: f64,
) -> Vec<Regression> {
    current
        .iter()
        .filter_map(|c| {
            let base = baseline.iter().find(|b| b.key() == c.key())?;
            let limit = (base.median_ns as f64) * (1.0 + threshold);
            if (c.median_ns as f64) > limit {
                Some(Regression {
                    layer: c.layer.clone(),
                    bench: c.bench.clone(),
                    baseline_ns: base.median_ns,
                    current_ns: c.median_ns,
                    ratio: (c.median_ns as f64) / (base.median_ns as f64),
                })
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn rec(layer: &str, bench: &str, median: u64) -> BenchRecord {
        BenchRecord {
            version: "0.2.10".to_owned(),
            sha: String::new(),
            layer: layer.to_owned(),
            bench: bench.to_owned(),
            median_ns: median,
            p99_ns: median * 2,
            iters: 199,
        }
    }

    #[test]
    fn parses_a_result_line_and_rejects_an_error_marker() {
        let good = r#"{"bench":"bench_x","median_ns":1000,"p99_ns":2000,"iters":199}"#;
        let parsed = record_from_line(good, "0.2.10", "abc123", "B").expect("good line parses");
        assert_eq!(parsed.bench, "bench_x");
        assert_eq!(parsed.median_ns, 1000);
        assert_eq!(parsed.sha, "abc123");

        let err = r#"{"bench":"bench_x","error":true}"#;
        assert!(
            record_from_line(err, "0.2.10", "", "B").is_none(),
            "an error marker carries no timing and must not become a record"
        );
        assert!(record_from_line("not json", "0.2.10", "", "B").is_none());
    }

    #[test]
    fn flags_a_regression_beyond_threshold_only() {
        let baseline = vec![rec("B", "fast", 1000), rec("B", "slow", 1000)];
        // `fast` grew 5% (within 15%), `slow` grew 30% (a regression).
        let current = vec![rec("B", "fast", 1050), rec("B", "slow", 1300)];

        let regs = regressions(&baseline, &current, 0.15);
        assert_eq!(regs.len(), 1, "only the 30% grower regresses");
        assert_eq!(regs[0].bench, "slow");
        assert_eq!(regs[0].baseline_ns, 1000);
        assert_eq!(regs[0].current_ns, 1300);
    }

    #[test]
    fn ignores_benches_absent_from_the_baseline() {
        let baseline = vec![rec("B", "known", 1000)];
        let current = vec![rec("B", "brand_new", 9_999_999)];
        assert!(
            regressions(&baseline, &current, 0.10).is_empty(),
            "a benchmark with no baseline has nothing to regress against"
        );
    }
}
