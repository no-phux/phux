//! Metric tables and the report they snapshot into.

use serde::{Deserialize, Serialize};

use crate::{Counter, Gauge, Histogram, HistogramSnapshot, ProcessStats};

/// Bumped when the JSON shape of [`PerfReport`] changes incompatibly.
pub const SCHEMA_VERSION: u32 = 1;

/// What a metric's numbers mean, so a renderer can pick a scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// Microseconds.
    Micros,
    /// Bytes.
    Bytes,
    /// A plain count.
    Count,
}

/// The live source behind a table entry.
#[derive(Debug)]
pub enum MetricSource {
    /// A latency or size distribution.
    Histogram(&'static Histogram),
    /// A monotone total.
    Counter(&'static Counter),
    /// A last-value reading.
    Gauge(&'static Gauge),
}

/// One row of a crate's metric table.
#[derive(Debug)]
pub struct Metric {
    /// Dotted, stable name: `pty.read.size`, `echo.rtt`.
    pub name: &'static str,
    /// Unit of every sample.
    pub unit: Unit,
    /// Where the numbers live.
    pub source: MetricSource,
}

impl Metric {
    /// Table-entry constructor for a histogram.
    #[must_use]
    pub const fn histogram(name: &'static str, unit: Unit, h: &'static Histogram) -> Self {
        Self {
            name,
            unit,
            source: MetricSource::Histogram(h),
        }
    }

    /// Table-entry constructor for a counter.
    #[must_use]
    pub const fn counter(name: &'static str, unit: Unit, c: &'static Counter) -> Self {
        Self {
            name,
            unit,
            source: MetricSource::Counter(c),
        }
    }

    /// Table-entry constructor for a gauge.
    #[must_use]
    pub const fn gauge(name: &'static str, unit: Unit, g: &'static Gauge) -> Self {
        Self {
            name,
            unit,
            source: MetricSource::Gauge(g),
        }
    }

    /// Copy the current value out.
    #[must_use]
    pub fn snapshot(&self) -> MetricSnapshot {
        let value = match self.source {
            MetricSource::Histogram(h) => MetricValue::Histogram(h.snapshot()),
            MetricSource::Counter(c) => MetricValue::Counter(c.get()),
            MetricSource::Gauge(g) => MetricValue::Gauge(g.get()),
        };
        MetricSnapshot {
            name: self.name.to_owned(),
            unit: self.unit,
            value,
        }
    }

    /// Zero the source. Gauges are last-value readings, not accumulations,
    /// so a reset leaves them alone: zeroing `proc.sched_interactive` would
    /// turn a granted promotion into a reported refusal.
    pub fn reset(&self) {
        match self.source {
            MetricSource::Histogram(h) => h.reset(),
            MetricSource::Counter(c) => c.reset(),
            MetricSource::Gauge(_) => {}
        }
    }
}

/// A snapshot value, tagged by kind.
///
/// Adjacently tagged so a bare `u64` counter serialises (an internally
/// tagged newtype of a primitive cannot), and flattened into
/// [`MetricSnapshot`] as `"kind": ..., "value": ...`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum MetricValue {
    /// Distribution.
    Histogram(HistogramSnapshot),
    /// Monotone total.
    Counter(u64),
    /// Last value.
    Gauge(u64),
}

/// One metric at one instant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricSnapshot {
    /// Dotted metric name.
    pub name: String,
    /// Unit of the samples.
    pub unit: Unit,
    /// The value.
    #[serde(flatten)]
    pub value: MetricValue,
}

/// Everything one process knows about its own performance at one instant,
/// or across one interval (see [`PerfReport::delta`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerfReport {
    /// [`SCHEMA_VERSION`] at capture.
    pub schema_version: u32,
    /// `"server"` or `"client"`.
    pub role: String,
    /// Reporting process id.
    pub pid: u32,
    /// Wall-clock capture time, milliseconds since the Unix epoch.
    pub captured_unix_ms: u64,
    /// Process uptime for a live report; interval length for a delta.
    pub uptime_ms: u64,
    /// `getrusage(2)` figures; `None` if the syscall failed.
    pub process: Option<ProcessStats>,
    /// Every metric in the table, in table order.
    pub metrics: Vec<MetricSnapshot>,
}

impl PerfReport {
    /// Serialize to compact JSON.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_owned())
    }

    /// Parse a report produced by [`Self::to_json`].
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Look a metric up by name.
    #[must_use]
    pub fn metric(&self, name: &str) -> Option<&MetricSnapshot> {
        self.metrics.iter().find(|m| m.name == name)
    }

    /// `self - prev`: what happened between two reports from the same
    /// process. Counters and histograms become interval figures, gauges keep
    /// their current reading, `uptime_ms` becomes the interval length, and
    /// the process section reports CPU consumed in the interval. A metric
    /// absent from `prev` (new binary, or a reset in between) is carried
    /// through unchanged.
    #[must_use]
    pub fn delta(&self, prev: &Self) -> Self {
        let metrics = self
            .metrics
            .iter()
            .map(|cur| {
                let before = prev.metric(&cur.name);
                let value = match (&cur.value, before.map(|b| &b.value)) {
                    (MetricValue::Histogram(h), Some(MetricValue::Histogram(p))) => {
                        MetricValue::Histogram(h.delta(p))
                    }
                    (MetricValue::Counter(c), Some(MetricValue::Counter(p))) => {
                        MetricValue::Counter(if c < p { *c } else { c - p })
                    }
                    (v, _) => v.clone(),
                };
                MetricSnapshot {
                    name: cur.name.clone(),
                    unit: cur.unit,
                    value,
                }
            })
            .collect();
        Self {
            schema_version: self.schema_version,
            role: self.role.clone(),
            pid: self.pid,
            captured_unix_ms: self.captured_unix_ms,
            uptime_ms: self.captured_unix_ms.saturating_sub(prev.captured_unix_ms),
            process: match (&self.process, &prev.process) {
                (Some(cur), Some(before)) => Some(cur.delta(before)),
                (cur, _) => *cur,
            },
            metrics,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static H: Histogram = Histogram::new();
    static C: Counter = Counter::new();
    static G: Gauge = Gauge::new();
    static TABLE: &[Metric] = &[
        Metric::histogram("t.lat", Unit::Micros, &H),
        Metric::counter("t.bytes", Unit::Bytes, &C),
        Metric::gauge("t.clients", Unit::Count, &G),
    ];
    // Its own sources: tests run in parallel, and a reset racing the
    // snapshot test would make that test flaky.
    static RH: Histogram = Histogram::new();
    static RC: Counter = Counter::new();
    static RG: Gauge = Gauge::new();
    static RESET_TABLE: &[Metric] = &[
        Metric::histogram("r.lat", Unit::Micros, &RH),
        Metric::counter("r.bytes", Unit::Bytes, &RC),
        Metric::gauge("r.clients", Unit::Count, &RG),
    ];

    #[test]
    fn snapshot_delta_and_json_roundtrip() {
        H.record(100);
        C.add(10);
        G.set(2);
        let a = crate::snapshot("server", TABLE, std::time::Duration::from_secs(1));
        H.record(200);
        C.add(5);
        G.set(3);
        let b = crate::snapshot("server", TABLE, std::time::Duration::from_secs(2));
        let d = b.delta(&a);
        match &d.metric("t.bytes").map(|m| &m.value) {
            Some(MetricValue::Counter(n)) => assert_eq!(*n, 5),
            other => panic!("unexpected {other:?}"),
        }
        match &d.metric("t.clients").map(|m| &m.value) {
            Some(MetricValue::Gauge(n)) => assert_eq!(*n, 3),
            other => panic!("unexpected {other:?}"),
        }
        match &d.metric("t.lat").map(|m| &m.value) {
            Some(MetricValue::Histogram(h)) => assert_eq!(h.count, 1),
            other => panic!("unexpected {other:?}"),
        }
        let back = PerfReport::from_json(&b.to_json()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, b);
        assert_eq!(back.schema_version, SCHEMA_VERSION);
        assert_eq!(back.role, "server");
    }

    #[test]
    fn reset_zeroes_every_source() {
        RH.record(1);
        RC.add(1);
        RG.set(1);
        crate::reset(RESET_TABLE);
        assert_eq!(RH.count(), 0);
        assert_eq!(RC.get(), 0);
        assert_eq!(
            RG.get(),
            1,
            "a gauge is a reading, not a total; reset leaves it"
        );
    }
}
