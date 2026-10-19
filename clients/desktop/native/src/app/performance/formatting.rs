//! Display formatting for process memory and interval metrics.

use super::*;

pub(super) fn unavailable() -> String {
    "—".to_owned()
}

pub(super) fn memory(bytes: Option<u64>) -> String {
    bytes.map_or_else(unavailable, |value| {
        format!("{:.1} MiB", value as f64 / 1_048_576.)
    })
}

pub(super) fn signed_memory(bytes: i64) -> String {
    let magnitude = memory(Some(bytes.unsigned_abs()));
    if bytes < 0 {
        format!("-{magnitude}")
    } else {
        magnitude
    }
}

pub(super) fn quantity(value: u64, unit: Unit) -> String {
    match unit {
        Unit::Micros if value >= 1_000 => format!("{:.2} ms", value as f64 / 1_000.),
        Unit::Micros => format!("{value} µs"),
        Unit::Bytes if value >= 1_048_576 => format!("{:.1} MiB", value as f64 / 1_048_576.),
        Unit::Bytes if value >= 1_024 => format!("{:.1} KiB", value as f64 / 1_024.),
        Unit::Bytes => format!("{value} B"),
        Unit::Count => value.to_string(),
    }
}

fn rate(value: u64, unit: Unit, seconds: f64) -> String {
    let per_second = value as f64 / seconds;
    match unit {
        Unit::Bytes if per_second >= 1_048_576. => format!("{:.1} MiB/s", per_second / 1_048_576.),
        Unit::Bytes if per_second >= 1_024. => format!("{:.1} KiB/s", per_second / 1_024.),
        Unit::Bytes => format!("{per_second:.1} B/s"),
        Unit::Micros => format!("{per_second:.1} µs/s"),
        Unit::Count => format!("{per_second:.1}/s"),
    }
}

pub(super) fn metric_values(
    current: &MetricSnapshot,
    interval: Option<&MetricSnapshot>,
    seconds: Option<f64>,
) -> [String; 3] {
    if let MetricValue::Gauge(value) = &current.value {
        return [quantity(*value, current.unit), unavailable(), unavailable()];
    }
    let Some((interval, seconds)) = interval.zip(seconds.filter(|seconds| *seconds > 0.)) else {
        return [unavailable(), unavailable(), unavailable()];
    };
    match &interval.value {
        MetricValue::Histogram(histogram) => {
            let stat = |value: u64| {
                if histogram.is_empty() {
                    unavailable()
                } else {
                    quantity(value, current.unit)
                }
            };
            [
                rate(histogram.count, Unit::Count, seconds),
                stat(histogram.mean()),
                stat(histogram.percentile(95)),
            ]
        }
        MetricValue::Counter(value) => [
            rate(*value, current.unit, seconds),
            unavailable(),
            unavailable(),
        ],
        MetricValue::Gauge(value) => [quantity(*value, current.unit), unavailable(), unavailable()],
    }
}
