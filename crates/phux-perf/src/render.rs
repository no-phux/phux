//! Human rendering of a report: the table `phux perf` prints.

#![allow(
    clippy::cast_precision_loss,
    reason = "rendering counts and durations as floats for display; a 52-bit mantissa is plenty"
)]

use std::fmt::Write as _;

use crate::{MetricValue, PerfReport, Unit};

/// Render `report` as an aligned text table. When `interval` is set the
/// report is a [`PerfReport::delta`] and counters gain a per-second rate;
/// otherwise counters are lifetime totals.
#[must_use]
pub fn render_report(report: &PerfReport, interval: Option<std::time::Duration>) -> String {
    let mut out = String::new();
    render_header(&mut out, report, interval);
    let rows: Vec<Row> = report
        .metrics
        .iter()
        .map(|m| Row::from_metric(m, interval))
        .collect();
    let widths = column_widths(&rows);
    let _ = writeln!(
        out,
        "{:<w0$}  {:>w1$}  {:>w2$}  {:>w3$}  {:>w4$}  {:>w5$}  {:>w6$}",
        "metric",
        "count",
        "rate/s",
        "p50",
        "p90",
        "p99",
        "max",
        w0 = widths[0],
        w1 = widths[1],
        w2 = widths[2],
        w3 = widths[3],
        w4 = widths[4],
        w5 = widths[5],
        w6 = widths[6],
    );
    let mut last_group = "";
    for row in &rows {
        let group = row.name.split('.').next().unwrap_or_default();
        if !last_group.is_empty() && group != last_group {
            out.push('\n');
        }
        last_group = group;
        let _ = writeln!(
            out,
            "{:<w0$}  {:>w1$}  {:>w2$}  {:>w3$}  {:>w4$}  {:>w5$}  {:>w6$}",
            row.name,
            row.cols[0],
            row.cols[1],
            row.cols[2],
            row.cols[3],
            row.cols[4],
            row.cols[5],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3],
            w4 = widths[4],
            w5 = widths[5],
            w6 = widths[6],
        );
    }
    out
}

fn render_header(out: &mut String, report: &PerfReport, interval: Option<std::time::Duration>) {
    let span = interval.map_or_else(
        || format!("uptime {}", fmt_duration_ms(report.uptime_ms)),
        |d| format!("last {}", fmt_duration_ms(crate::duration_ms(d))),
    );
    let _ = write!(out, "{} pid {}  {}", report.role, report.pid, span);
    if let Some(p) = &report.process {
        let cpu_pct = if report.uptime_ms == 0 {
            0.0
        } else {
            (p.cpu_total_us() as f64 / 1000.0) / report.uptime_ms as f64 * 100.0
        };
        let _ = write!(
            out,
            "  cpu {cpu_pct:.1}% (user {} sys {})  peak rss {}  ctx switches vol {} invol {}",
            fmt_us(p.cpu_user_us),
            fmt_us(p.cpu_system_us),
            fmt_bytes(p.max_rss_bytes),
            p.voluntary_ctx_switches,
            p.involuntary_ctx_switches,
        );
    }
    out.push('\n');
}

struct Row {
    name: String,
    cols: [String; 6],
}

impl Row {
    fn from_metric(m: &crate::MetricSnapshot, interval: Option<std::time::Duration>) -> Self {
        let rate = |n: u64| -> String {
            match interval {
                Some(d) if !d.is_zero() => {
                    let per_s = n as f64 / d.as_secs_f64();
                    if per_s >= 100.0 {
                        format!("{per_s:.0}")
                    } else {
                        format!("{per_s:.1}")
                    }
                }
                _ => "-".to_owned(),
            }
        };
        let cols = match &m.value {
            MetricValue::Histogram(h) => [
                h.count.to_string(),
                rate(h.count),
                fmt_unit(h.percentile(50), m.unit),
                fmt_unit(h.percentile(90), m.unit),
                fmt_unit(h.percentile(99), m.unit),
                fmt_unit(h.max, m.unit),
            ],
            MetricValue::Counter(n) => [
                fmt_unit(*n, m.unit),
                rate(*n),
                "-".to_owned(),
                "-".to_owned(),
                "-".to_owned(),
                "-".to_owned(),
            ],
            MetricValue::Gauge(n) => [
                fmt_unit(*n, m.unit),
                "gauge".to_owned(),
                "-".to_owned(),
                "-".to_owned(),
                "-".to_owned(),
                "-".to_owned(),
            ],
        };
        Self {
            name: m.name.clone(),
            cols,
        }
    }
}

fn column_widths(rows: &[Row]) -> [usize; 7] {
    let mut w = [6_usize, 5, 6, 3, 3, 3, 3];
    for r in rows {
        w[0] = w[0].max(r.name.len());
        for (i, c) in r.cols.iter().enumerate() {
            w[i + 1] = w[i + 1].max(c.len());
        }
    }
    w
}

fn fmt_unit(v: u64, unit: Unit) -> String {
    match unit {
        Unit::Micros => fmt_us(v),
        Unit::Bytes => fmt_bytes(v),
        Unit::Count => v.to_string(),
    }
}

/// `842us`, `1.7ms`, `2.30s`.
fn fmt_us(us: u64) -> String {
    if us < 1_000 {
        format!("{us}us")
    } else if us < 1_000_000 {
        format!("{:.1}ms", us as f64 / 1_000.0)
    } else {
        format!("{:.2}s", us as f64 / 1_000_000.0)
    }
}

fn fmt_duration_ms(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else if s < 86_400 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d{:02}h", s / 86_400, (s % 86_400) / 3600)
    }
}

/// `512B`, `1.5KiB`, `28.7MiB`.
fn fmt_bytes(b: u64) -> String {
    const KIB: f64 = 1024.0;
    let f = b as f64;
    if b < 1024 {
        format!("{b}B")
    } else if f < KIB * KIB {
        format!("{:.1}KiB", f / KIB)
    } else if f < KIB * KIB * KIB {
        format!("{:.1}MiB", f / (KIB * KIB))
    } else {
        format!("{:.2}GiB", f / (KIB * KIB * KIB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Counter, Gauge, Histogram, Metric, Unit};

    static H: Histogram = Histogram::new();
    static C: Counter = Counter::new();
    static G: Gauge = Gauge::new();
    static TABLE: &[Metric] = &[
        Metric::histogram("echo.rtt", Unit::Micros, &H),
        Metric::counter("pty.read.bytes", Unit::Bytes, &C),
        Metric::gauge("clients", Unit::Count, &G),
    ];

    #[test]
    fn units_format_readably() {
        assert_eq!(fmt_us(842), "842us");
        assert_eq!(fmt_us(1_700), "1.7ms");
        assert_eq!(fmt_us(2_300_000), "2.30s");
        assert_eq!(fmt_bytes(512), "512B");
        assert_eq!(fmt_bytes(30_000_000), "28.6MiB");
        assert_eq!(fmt_duration_ms(90_000), "1m30s");
        assert_eq!(fmt_duration_ms(6 * 86_400_000 + 3_600_000), "6d01h");
    }

    #[test]
    fn table_has_a_header_and_every_metric() {
        H.record(700);
        C.add(4096);
        G.set(2);
        let r = crate::snapshot("server", TABLE, std::time::Duration::from_secs(10));
        let text = render_report(&r, None);
        assert!(text.starts_with("server pid "), "{text}");
        assert!(text.contains("uptime 10.0s"), "{text}");
        assert!(text.contains("echo.rtt"), "{text}");
        assert!(text.contains("pty.read.bytes"), "{text}");
        assert!(text.contains("4.0KiB"), "{text}");
        assert!(text.contains("gauge"), "{text}");
        let with_rate = render_report(&r, Some(std::time::Duration::from_secs(2)));
        assert!(with_rate.contains("last 2.0s"), "{with_rate}");
    }
}
