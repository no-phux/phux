//! On-demand performance samples for the desktop process itself.
//!
//! No timer or background work lives here: a caller that opts in owns the
//! cadence and can stop sampling simply by dropping its sampler.

use std::time::Instant;

use phux_perf::{PerfReport, ProcessStats};

/// One desktop-process sample, with unavailable readings kept distinct from zero.
#[derive(Debug, Clone)]
pub struct Sample {
    /// Process CPU over the actual monotonic interval; one full core is 100%.
    /// Unavailable on the first sample or after a failed/reset CPU reading.
    pub cpu_percent: Option<f64>,
    /// Native current resident memory, not the lifetime high-water mark.
    pub resident_bytes: Option<u64>,
    /// macOS physical footprint (Activity Monitor's memory accounting).
    pub footprint_bytes: Option<u64>,
    /// Lifetime peak resident memory from the report's process section.
    pub peak_resident_bytes: Option<u64>,
    /// Cumulative kernel, runtime, painter and window metrics at capture.
    pub report: PerfReport,
}

/// A CPU baseline only; creating this value does not capture or poll anything.
#[derive(Debug, Default)]
pub struct Sampler {
    previous: Option<(Instant, ProcessStats)>,
}

impl Sampler {
    /// Create an unsampled model. Its first CPU reading will be unavailable.
    #[must_use]
    pub const fn new() -> Self {
        Self { previous: None }
    }

    /// Capture the desktop process and all its existing layered metrics once.
    #[must_use]
    pub fn sample(&mut self) -> Sample {
        let report = crate::diag::report();
        let at = Instant::now();
        let resident_bytes = report.rss_bytes;
        let mut sample = self.sample_at(at, report, resident_bytes);
        sample.footprint_bytes = ProcessStats::physical_footprint_bytes();
        sample
    }

    fn sample_at(
        &mut self,
        at: Instant,
        report: PerfReport,
        resident_bytes: Option<u64>,
    ) -> Sample {
        // A missing process reading also clears the baseline. Recovery needs
        // two successful observations, rather than bridging an unknown gap.
        let previous = std::mem::replace(
            &mut self.previous,
            report.process.map(|process| (at, process)),
        );
        let cpu_percent = report
            .process
            .zip(previous)
            .and_then(|(now, (before_at, before))| {
                let elapsed = at.checked_duration_since(before_at)?.as_secs_f64();
                if elapsed == 0.0 {
                    return None;
                }
                // A reset/regression is not an idle interval. Rebaseline rather
                // than hiding it as a zero-CPU sample with saturating subtraction.
                let user_us = now.cpu_user_us.checked_sub(before.cpu_user_us)?;
                let system_us = now.cpu_system_us.checked_sub(before.cpu_system_us)?;
                let cpu_us = user_us.checked_add(system_us)?;
                Some(cpu_us as f64 / 1_000_000.0 / elapsed * 100.0)
            });
        Sample {
            cpu_percent,
            resident_bytes,
            footprint_bytes: None,
            peak_resident_bytes: report.process.map(|process| process.max_rss_bytes),
            report,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn report(process: Option<ProcessStats>) -> PerfReport {
        PerfReport {
            schema_version: phux_perf::SCHEMA_VERSION,
            role: "kernel".to_owned(),
            pid: 42,
            captured_unix_ms: 100,
            uptime_ms: 10,
            process,
            metrics: Vec::new(),
            stream_diagnostics: None,
            rss_bytes: None,
            alloc: None,
        }
    }

    fn cpu(user_us: u64, system_us: u64) -> Option<ProcessStats> {
        Some(ProcessStats {
            cpu_user_us: user_us,
            cpu_system_us: system_us,
            max_rss_bytes: 4096,
            ..ProcessStats::default()
        })
    }

    fn assert_percent(actual: Option<f64>, expected: f64) {
        let actual = actual.unwrap_or_else(|| panic!("expected a CPU interval"));
        assert!(
            (actual - expected).abs() < 0.000_001,
            "{actual} != {expected}"
        );
    }

    #[test]
    fn first_sample_has_memory_and_report_but_no_cpu_interval() {
        let mut sampler = Sampler::new();
        let report = report(cpu(1_000_000, 500_000));
        let sample = sampler.sample_at(Instant::now(), report.clone(), Some(2048));
        assert_eq!(sample.cpu_percent, None);
        assert_eq!(sample.resident_bytes, Some(2048));
        assert_eq!(sample.peak_resident_bytes, Some(4096));
        assert_eq!(sample.report, report);
    }

    #[test]
    fn cpu_uses_real_elapsed_time_and_can_exceed_one_core() {
        let mut sampler = Sampler::new();
        let start = Instant::now();
        sampler.sample_at(start, report(cpu(1_000_000, 500_000)), None);
        // Identical wall-clock report timestamps deliberately prove that CPU
        // does not use the clock, uptime, or an assumed one-second cadence.
        let sample = sampler.sample_at(
            start + Duration::from_millis(250),
            report(cpu(1_250_000, 625_000)),
            None,
        );
        assert_percent(sample.cpu_percent, 150.0);
        let idle = sampler.sample_at(
            start + Duration::from_secs(2),
            report(cpu(1_250_000, 625_000)),
            None,
        );
        assert_percent(idle.cpu_percent, 0.0);
    }

    #[test]
    fn failed_cpu_capture_clears_baseline_without_losing_current_rss() {
        let mut sampler = Sampler::new();
        let start = Instant::now();
        sampler.sample_at(start, report(cpu(100, 50)), Some(1024));
        let failed = sampler.sample_at(start + Duration::from_secs(1), report(None), Some(2048));
        assert_eq!(failed.cpu_percent, None);
        assert_eq!(failed.resident_bytes, Some(2048));
        assert_eq!(failed.peak_resident_bytes, None);
        let recovered = sampler.sample_at(
            start + Duration::from_secs(2),
            report(cpu(1_000_100, 50)),
            None,
        );
        assert_eq!(recovered.cpu_percent, None);
        assert_eq!(recovered.resident_bytes, None);
        assert_eq!(recovered.peak_resident_bytes, Some(4096));
        let next = sampler.sample_at(
            start + Duration::from_secs(4),
            report(cpu(2_000_100, 50)),
            None,
        );
        assert_percent(next.cpu_percent, 50.0);
    }

    #[test]
    fn zero_interval_and_counter_regression_are_unavailable_then_rebaseline() {
        let mut sampler = Sampler::new();
        let start = Instant::now();
        sampler.sample_at(start, report(cpu(1_000_000, 500_000)), None);
        let zero = sampler.sample_at(start, report(cpu(1_100_000, 500_000)), None);
        assert_eq!(zero.cpu_percent, None);
        // Total CPU still grows, but user CPU regresses: neither field may
        // regress independently without invalidating the interval.
        let reset = sampler.sample_at(
            start + Duration::from_secs(1),
            report(cpu(1_000_000, 1_000_000)),
            None,
        );
        assert_eq!(reset.cpu_percent, None);
        let next = sampler.sample_at(
            start + Duration::from_secs(3),
            report(cpu(1_500_000, 1_500_000)),
            None,
        );
        assert_percent(next.cpu_percent, 50.0);
    }
}
