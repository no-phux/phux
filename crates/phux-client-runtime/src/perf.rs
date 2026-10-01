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
//! `runtime.apply_batches`. `runtime.request_wall` includes queue wait,
//! execution and response delivery;
//! none of these elapsed durations claim CPU time. Everything is a `static`
//! from [`phux_perf`], always on, with allocation-free relaxed atomic recording.

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
/// Microseconds from command enqueue to owner dequeue; includes scheduling delay.
pub static OWNER_QUEUE_WAIT: Histogram = Histogram::new();
/// Microseconds executing a dequeued command, including replying; excludes idle recv.
pub static OWNER_EXECUTE: Histogram = Histogram::new();
/// Microseconds from request channel creation through response/error receipt.
pub static REQUEST_WALL: Histogram = Histogram::new();
/// Microseconds executing an apply batch, including its publications.
pub static APPLY_EXECUTE: Histogram = Histogram::new();
/// Events delivered in an apply batch.
pub static APPLY_EVENTS: Histogram = Histogram::new();
/// Apply events that returned an error, including fatal batch-prefix endings.
pub static APPLY_ERRORS: Counter = Counter::new();
/// Microseconds executing a publication catch-up, excluding its request wait.
pub static CATCH_UP_EXECUTE: Histogram = Histogram::new();
/// Microseconds projecting one grid, excluding publication.
pub static PROJECT_GRID: Histogram = Histogram::new();
/// Microseconds publishing a projected frame and reclaiming its predecessor.
pub static PUBLISH_SWAP: Histogram = Histogram::new();
/// Previous frames still held by a consumer at the publication swap.
pub static BUFFER_HELD: Counter = Counter::new();
/// Timed grid projection/publication attempts that failed (setup excluded).
pub static PROJECT_ERRORS: Counter = Counter::new();
/// Command send or synchronous response receive failures (owner stopped).
pub static REQUEST_ERRORS: Counter = Counter::new();

/// The runtime's metric table, in render order.
pub static TABLE: &[Metric] = &[
    Metric::counter("runtime.apply_batches", Unit::Count, &APPLY_BATCHES),
    Metric::counter("runtime.publish", Unit::Count, &PUBLISHED),
    Metric::histogram("runtime.project", Unit::Micros, &PROJECT),
    Metric::counter("runtime.publish_deferred", Unit::Count, &DEFERRED),
    Metric::counter("runtime.acquire", Unit::Count, &ACQUIRED),
    Metric::counter("runtime.catch_up", Unit::Count, &CAUGHT_UP),
    Metric::histogram("runtime.owner_queue_wait", Unit::Micros, &OWNER_QUEUE_WAIT),
    Metric::histogram("runtime.owner_execute", Unit::Micros, &OWNER_EXECUTE),
    Metric::histogram("runtime.request_wall", Unit::Micros, &REQUEST_WALL),
    Metric::histogram("runtime.apply_execute", Unit::Micros, &APPLY_EXECUTE),
    Metric::histogram("runtime.apply_events", Unit::Count, &APPLY_EVENTS),
    Metric::counter("runtime.apply_errors", Unit::Count, &APPLY_ERRORS),
    Metric::histogram("runtime.catch_up_execute", Unit::Micros, &CATCH_UP_EXECUTE),
    Metric::histogram("runtime.project_grid", Unit::Micros, &PROJECT_GRID),
    Metric::histogram("runtime.publish_swap", Unit::Micros, &PUBLISH_SWAP),
    Metric::counter("runtime.buffer_held", Unit::Count, &BUFFER_HELD),
    Metric::counter("runtime.project_errors", Unit::Count, &PROJECT_ERRORS),
    Metric::counter("runtime.request_errors", Unit::Count, &REQUEST_ERRORS),
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
