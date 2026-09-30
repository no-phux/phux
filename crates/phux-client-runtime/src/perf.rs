//! Owner-thread telemetry: how often the engine is asked to apply and how
//! much projecting and publishing grids costs.
//!
//! The kernel's table (`phux_client_core::perf`) counts frames and times
//! libghostty applies; this one sits one layer out, where a binding's
//! delivery shape shows up. `runtime.apply_batches` against `kernel.frames`
//! is how many output frames each owner-thread round trip carried, and
//! `runtime.publish` counts grid publications (one projection of every dirty
//! row each). Publication is paced by reads (see `publication`): a batch
//! publishes a presentation whose current frame a consumer has acquired and
//! defers one whose frame is unread (`runtime.publish_deferred`); the next
//! `runtime.acquire` of a deferred presentation pulls one projection
//! (`runtime.catch_up`, also counted in `runtime.publish`). Under an output
//! flood `runtime.publish` therefore tracks `runtime.acquire`, not
//! `runtime.apply_batches`. Everything is a `static` from [`phux_perf`],
//! always on, one relaxed atomic add per sample.

use phux_perf::{Counter, Histogram, Metric, MetricSnapshot, Unit};

/// Engine apply requests: one owner-thread round trip each.
pub static APPLY_BATCHES: Counter = Counter::new();
/// Grid publications: one dirty-row projection plus a slot swap each.
pub static PUBLISHED: Counter = Counter::new();
/// Microseconds to project and publish one terminal's grid.
pub static PROJECT: Histogram = Histogram::new();
/// Damaged presentations a batch left unprojected because nobody had read
/// their current frame.
pub static DEFERRED: Counter = Counter::new();
/// Frames consumers acquired from the publication table.
pub static ACQUIRED: Counter = Counter::new();
/// Projections an acquire pulled from the owner for a deferred presentation.
pub static CAUGHT_UP: Counter = Counter::new();

/// The runtime's metric table, in render order.
pub static TABLE: &[Metric] = &[
    Metric::counter("runtime.apply_batches", Unit::Count, &APPLY_BATCHES),
    Metric::counter("runtime.publish", Unit::Count, &PUBLISHED),
    Metric::histogram("runtime.project", Unit::Micros, &PROJECT),
    Metric::counter("runtime.publish_deferred", Unit::Count, &DEFERRED),
    Metric::counter("runtime.acquire", Unit::Count, &ACQUIRED),
    Metric::counter("runtime.catch_up", Unit::Count, &CAUGHT_UP),
];
/// Snapshot every runtime metric, for a binding to append to the kernel's
/// report.
pub fn snapshot() -> impl Iterator<Item = MetricSnapshot> {
    TABLE.iter().map(Metric::snapshot)
}

#[cfg(test)]
mod tests {
    use super::TABLE;

    #[test]
    fn table_names_are_unique_and_prefixed() {
        let mut names: Vec<&str> = TABLE.iter().map(|m| m.name).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n);
        assert!(TABLE.iter().all(|m| m.name.starts_with("runtime.")));
    }
}
