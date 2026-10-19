//! Span capture: every timed histogram sample as a timeline event.
//!
//! While a capture is armed, [`crate::Histogram::record_duration`] (and so
//! every [`crate::Timer`] and `record_elapsed`) also appends a complete event
//! to a per-thread buffer: which histogram, which thread, when it started,
//! how long it took. [`stop`] collects the buffers into a [`Capture`], and
//! [`Capture::to_chrome_json`] names each event from the crate's metric
//! tables and writes the Chrome trace-event format that
//! <https://ui.perfetto.dev> and `chrome://tracing` open.
//!
//! Unarmed, a sample pays one relaxed load. Armed, it pays an uncontended
//! per-thread lock and a push, bounded by the capture's event budget; events
//! past the budget are dropped and counted, never blocked on.
//!
//! Timestamps are microseconds since the Unix epoch, anchored once per
//! capture, so captures from the server and the desktop line up on one
//! timeline when [`merge_chrome_json`] puts them in one file.

use std::cell::OnceCell;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::{Metric, MetricSource};

/// Default event budget for one capture: about 40 MB of events in memory and
/// a trace Perfetto opens without complaint.
pub const DEFAULT_MAX_EVENTS: usize = 1_000_000;

static ARMED: AtomicBool = AtomicBool::new(false);
/// Bumped per capture; a buffer still holding a previous capture's events
/// discards them on its next push.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static BUDGET: AtomicUsize = AtomicUsize::new(0);
static RECORDED: AtomicUsize = AtomicUsize::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static NEXT_TID: AtomicU64 = AtomicU64::new(1);
/// [`now_ns`] when the armed capture started.
static ANCHOR_NS: AtomicU64 = AtomicU64::new(0);
/// The wall clock at that moment, microseconds since the Unix epoch.
static ANCHOR_UNIX_US: AtomicU64 = AtomicU64::new(0);
/// Serializes [`start`] and [`stop`]; never taken on the recording path.
static CONTROL: Mutex<()> = Mutex::new(());
static THREADS: Mutex<Vec<Arc<ThreadBuf>>> = Mutex::new(Vec::new());

/// Nanoseconds since a process-wide monotonic epoch.
fn now_ns() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    nanos(EPOCH.get_or_init(Instant::now).elapsed())
}

/// What an event is named by.
#[derive(Debug, Clone, Copy)]
enum Key {
    /// A histogram's address, named from the metric tables at export.
    Histogram(usize),
    /// An explicit [`span`], [`instant`] or [`counter`] name.
    Named(&'static str),
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    /// A complete span (`ph: "X"`).
    Span { dur_ns: u64 },
    /// A point in time (`ph: "i"`).
    Instant,
    /// A counter-track sample (`ph: "C"`).
    Counter { value: f64 },
}

#[derive(Debug, Clone, Copy)]
struct Event {
    key: Key,
    kind: Kind,
    /// Start, on the [`now_ns`] clock.
    start_ns: u64,
}

/// One thread's events, tagged with the capture generation they belong to.
#[derive(Debug)]
struct ThreadBuf {
    tid: u64,
    name: String,
    events: Mutex<(u64, Vec<Event>)>,
}

thread_local! {
    static LOCAL: OnceCell<Arc<ThreadBuf>> = const { OnceCell::new() };
}

/// Is a capture armed?
#[must_use]
pub fn armed() -> bool {
    ARMED.load(Relaxed)
}

/// Arm a capture with room for `max_events`, discarding anything a previous
/// capture left unread. Returns `false` (and changes nothing) when one is
/// already armed.
pub fn start(max_events: usize) -> bool {
    let _control = CONTROL.lock().unwrap_or_else(PoisonError::into_inner);
    if ARMED.load(Relaxed) {
        return false;
    }
    GENERATION.fetch_add(1, Relaxed);
    RECORDED.store(0, Relaxed);
    DROPPED.store(0, Relaxed);
    BUDGET.store(max_events, Relaxed);
    ANCHOR_NS.store(now_ns(), Relaxed);
    ANCHOR_UNIX_US.store(unix_us_now(), Relaxed);
    ARMED.store(true, Relaxed);
    true
}

/// Disarm and collect. `None` when nothing was armed.
#[must_use]
pub fn stop() -> Option<Capture> {
    let _control = CONTROL.lock().unwrap_or_else(PoisonError::into_inner);
    if !ARMED.swap(false, Relaxed) {
        return None;
    }
    let generation = GENERATION.load(Relaxed);
    let anchor_ns = ANCHOR_NS.load(Relaxed);
    let duration = Duration::from_nanos(now_ns().saturating_sub(anchor_ns));
    let threads = THREADS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let mut collected = Vec::with_capacity(threads.len());
    for thread in threads {
        let mut slot = thread
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if slot.0 != generation || slot.1.is_empty() {
            continue;
        }
        collected.push(ThreadEvents {
            tid: thread.tid,
            name: thread.name.clone(),
            events: std::mem::take(&mut slot.1),
        });
    }
    Some(Capture {
        anchor_ns,
        anchor_unix_us: ANCHOR_UNIX_US.load(Relaxed),
        duration,
        dropped: DROPPED.load(Relaxed),
        threads: collected,
    })
}

/// Record a span that ended now and lasted `dur`, keyed by histogram address.
pub(crate) fn record_histogram(histogram: usize, dur: Duration) {
    if armed() {
        push(
            Key::Histogram(histogram),
            Kind::Span {
                dur_ns: nanos(dur),
            },
        );
    }
}

/// Time a named region for the current capture; a no-op guard when unarmed.
///
/// For code that has no histogram of its own. Prefer a histogram where the
/// timing is worth keeping in reports too.
#[must_use]
pub fn span(name: &'static str) -> Span {
    Span {
        name,
        started: armed().then(Instant::now),
    }
}

/// Mark a point in time on the current thread's track.
pub fn instant(name: &'static str) {
    if armed() {
        push(Key::Named(name), Kind::Instant);
    }
}

/// Sample a counter track (`rss`, `heap.live`, `fps`) at this moment.
pub fn counter(name: &'static str, value: f64) {
    if armed() {
        push(Key::Named(name), Kind::Counter { value });
    }
}

/// Guard returned by [`span`].
#[derive(Debug)]
pub struct Span {
    name: &'static str,
    started: Option<Instant>,
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            push(
                Key::Named(self.name),
                Kind::Span {
                    dur_ns: nanos(started.elapsed()),
                },
            );
        }
    }
}

/// Append one event that ends now to this thread's buffer; a span's start
/// is backdated by its length.
fn push(key: Key, kind: Kind) {
    if RECORDED.fetch_add(1, Relaxed) >= BUDGET.load(Relaxed) {
        DROPPED.fetch_add(1, Relaxed);
        return;
    }
    let now = now_ns();
    let start_ns = match kind {
        Kind::Span { dur_ns } => now.saturating_sub(dur_ns),
        Kind::Instant | Kind::Counter { .. } => now,
    };
    let generation = GENERATION.load(Relaxed);
    LOCAL.with(|cell| {
        let buf = cell.get_or_init(register_thread);
        let mut slot = buf.events.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.0 != generation {
            *slot = (generation, Vec::new());
        }
        slot.1.push(Event {
            key,
            kind,
            start_ns,
        });
    });
}

fn register_thread() -> Arc<ThreadBuf> {
    let current = std::thread::current();
    let tid = NEXT_TID.fetch_add(1, Relaxed);
    let buf = Arc::new(ThreadBuf {
        tid,
        name: current
            .name()
            .map_or_else(|| format!("thread-{tid}"), str::to_owned),
        events: Mutex::new((0, Vec::new())),
    });
    THREADS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Arc::clone(&buf));
    buf
}

#[derive(Debug, Clone)]
struct ThreadEvents {
    tid: u64,
    name: String,
    events: Vec<Event>,
}

/// One finished capture, ready to name and export.
#[derive(Debug, Clone)]
pub struct Capture {
    anchor_ns: u64,
    anchor_unix_us: u64,
    duration: Duration,
    dropped: u64,
    threads: Vec<ThreadEvents>,
}

impl Capture {
    /// Events recorded across every thread.
    #[must_use]
    pub fn event_count(&self) -> usize {
        self.threads.iter().map(|t| t.events.len()).sum()
    }

    /// Events lost to the budget.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// How long the capture was armed.
    #[must_use]
    pub const fn duration(&self) -> Duration {
        self.duration
    }

    /// The Chrome trace-event JSON for this capture. `process` names the
    /// process track (`phux server`, `phux desktop`); `tables` name the
    /// histogram events, with the metric's dotted prefix as the category.
    #[must_use]
    pub fn to_chrome_json(&self, process: &str, tables: &[&[Metric]]) -> String {
        let pid = std::process::id();
        let names = histogram_names(tables);
        let mut events = vec![json!({
            "ph": "M", "name": "process_name", "pid": pid, "tid": 0,
            "args": { "name": process },
        })];
        for thread in &self.threads {
            events.push(json!({
                "ph": "M", "name": "thread_name", "pid": pid, "tid": thread.tid,
                "args": { "name": thread.name },
            }));
            for event in &thread.events {
                events.push(self.event_json(pid, thread.tid, event, &names));
            }
        }
        json!({
            "traceEvents": events,
            "displayTimeUnit": "ns",
            "otherData": {
                "process": process,
                "pid": pid,
                "anchor_unix_us": self.anchor_unix_us,
                "duration_us": u64::try_from(self.duration.as_micros()).unwrap_or(u64::MAX),
                "events": self.event_count(),
                "dropped": self.dropped,
            },
        })
        .to_string()
    }

    fn event_json(
        &self,
        pid: u32,
        tid: u64,
        event: &Event,
        names: &[(usize, &'static str)],
    ) -> Value {
        let name = match event.key {
            Key::Named(name) => name.to_owned(),
            Key::Histogram(addr) => names.iter().find(|(a, _)| *a == addr).map_or_else(
                || format!("histogram@{addr:#x}"),
                |(_, n)| (*n).to_owned(),
            ),
        };
        let cat = name.split('.').next().unwrap_or_default().to_owned();
        let ts = self.ts_us(event.start_ns);
        match event.kind {
            Kind::Span { dur_ns } => json!({
                "ph": "X", "name": name, "cat": cat, "pid": pid, "tid": tid,
                "ts": ts, "dur": ns_to_us(dur_ns),
            }),
            Kind::Instant => json!({
                "ph": "i", "s": "t", "name": name, "cat": cat, "pid": pid, "tid": tid, "ts": ts,
            }),
            Kind::Counter { value } => json!({
                "ph": "C", "name": name, "cat": cat, "pid": pid, "tid": tid, "ts": ts,
                "args": { "value": value },
            }),
        }
    }

    /// Wall-clock microseconds for a [`now_ns`] reading. A span that began
    /// before the capture was armed lands just before its anchor.
    #[allow(
        clippy::cast_precision_loss,
        reason = "microsecond timestamps since the epoch fit a 52-bit mantissa until the year 2112"
    )]
    fn ts_us(&self, start_ns: u64) -> f64 {
        let offset = if start_ns >= self.anchor_ns {
            ns_to_us(start_ns - self.anchor_ns)
        } else {
            -ns_to_us(self.anchor_ns - start_ns)
        };
        self.anchor_unix_us as f64 + offset
    }
}

/// Merge Chrome trace-event documents (from [`Capture::to_chrome_json`],
/// one per process) into one, so Perfetto shows every process on one
/// timeline. Inputs that do not parse are skipped.
#[must_use]
pub fn merge_chrome_json<'a>(docs: impl IntoIterator<Item = &'a str>) -> String {
    let mut events = Vec::new();
    let mut processes = Vec::new();
    for doc in docs {
        let Ok(mut value) = serde_json::from_str::<Value>(doc) else {
            continue;
        };
        if let Some(Value::Array(list)) = value.get_mut("traceEvents").map(Value::take) {
            events.extend(list);
        }
        if let Some(other) = value.get_mut("otherData").map(Value::take) {
            processes.push(other);
        }
    }
    json!({
        "traceEvents": events,
        "displayTimeUnit": "ns",
        "otherData": { "processes": processes },
    })
    .to_string()
}

/// Every histogram in `tables`, by address.
fn histogram_names(tables: &[&[Metric]]) -> Vec<(usize, &'static str)> {
    tables
        .iter()
        .flat_map(|table| table.iter())
        .filter_map(|metric| match metric.source {
            MetricSource::Histogram(h) => Some((std::ptr::from_ref(h) as usize, metric.name)),
            MetricSource::Counter(_) | MetricSource::Gauge(_) => None,
        })
        .collect()
}

#[allow(
    clippy::cast_precision_loss,
    reason = "sub-microsecond precision of a span length; f64 is exact to 2^53 ns"
)]
fn ns_to_us(ns: u64) -> f64 {
    ns as f64 / 1000.0
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(not(target_arch = "wasm32"))]
fn unix_us_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

#[cfg(target_arch = "wasm32")]
const fn unix_us_now() -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Histogram, Unit};

    static TRACED: Histogram = Histogram::new();
    static TABLE: &[Metric] = &[Metric::histogram("test.traced", Unit::Micros, &TRACED)];

    /// Captures are process-global; one test drives the whole lifecycle so a
    /// parallel test cannot disarm another's capture.
    #[test]
    fn capture_names_spans_from_tables_and_respects_the_budget() {
        TRACED.record_duration(Duration::from_millis(1));
        assert!(stop().is_none(), "nothing armed yet");
        assert!(start(4));
        assert!(!start(4), "a second start while armed is refused");
        {
            let _t = TRACED.timer();
            std::thread::sleep(Duration::from_millis(2));
        }
        {
            let _s = span("test.explicit");
        }
        instant("test.mark");
        counter("test.rss", 42.0);
        TRACED.record_duration(Duration::from_micros(5));
        let capture = stop().unwrap_or_else(|| panic!("armed capture"));
        assert_eq!(capture.event_count(), 4);
        assert_eq!(capture.dropped(), 1);
        TRACED.record_duration(Duration::from_millis(1));
        assert!(stop().is_none(), "disarmed after stop");

        let doc: Value = serde_json::from_str(&capture.to_chrome_json("phux test", &[TABLE]))
            .unwrap_or_else(|e| panic!("{e}"));
        let events = doc["traceEvents"]
            .as_array()
            .unwrap_or_else(|| panic!("events"));
        let span = events
            .iter()
            .find(|e| e["name"] == "test.traced")
            .unwrap_or_else(|| panic!("named histogram span in {events:?}"));
        assert_eq!(span["ph"], "X");
        assert_eq!(span["cat"], "test");
        assert!(span["dur"].as_f64().unwrap_or_default() >= 2000.0);
        assert!(
            events
                .iter()
                .any(|e| e["name"] == "test.explicit" && e["ph"] == "X")
        );
        assert!(
            events
                .iter()
                .any(|e| e["name"] == "test.mark" && e["ph"] == "i")
        );
        assert!(
            events
                .iter()
                .any(|e| e["name"] == "test.rss" && e["args"]["value"] == 42.0)
        );
        assert!(
            events
                .iter()
                .any(|e| e["name"] == "process_name" && e["args"]["name"] == "phux test")
        );
        assert_eq!(doc["otherData"]["dropped"], 1);

        let merged: Value = serde_json::from_str(&merge_chrome_json([
            capture.to_chrome_json("a", &[TABLE]).as_str(),
            "not json",
            capture.to_chrome_json("b", &[TABLE]).as_str(),
        ]))
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            merged["traceEvents"].as_array().map(Vec::len),
            Some(events.len() * 2)
        );
        assert_eq!(
            merged["otherData"]["processes"].as_array().map(Vec::len),
            Some(2)
        );
    }
}
