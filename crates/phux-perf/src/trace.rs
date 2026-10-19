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

use std::borrow::Cow;
use std::cell::OnceCell;
use std::collections::HashMap;
use std::sync::atomic::{
    AtomicU64, AtomicUsize,
    Ordering::{Acquire, Relaxed, Release},
};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{Metric, MetricSource};

/// Default event budget for one capture: about 40 MB of events in memory and
/// a trace Perfetto opens without complaint.
pub const DEFAULT_MAX_EVENTS: usize = 1_000_000;

/// Generation in the high bits and armed in bit zero. One acquire reads
/// both the admission identity and the configuration published by start.
static STATE: AtomicU64 = AtomicU64::new(0);
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
    /// End, on the [`now_ns`] clock. Subtract span duration only at export:
    /// a retrospective span may have begun before this clock's epoch.
    end_ns: u64,
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
    STATE.load(Acquire) & 1 != 0
}

/// Arm a capture with room for `max_events`, discarding anything a previous
/// capture left unread. Returns `false` (and changes nothing) when one is
/// already armed.
pub fn start(max_events: usize) -> bool {
    let _control = CONTROL.lock().unwrap_or_else(PoisonError::into_inner);
    let state = STATE.load(Acquire);
    if state & 1 != 0 {
        return false;
    }
    RECORDED.store(0, Relaxed);
    DROPPED.store(0, Relaxed);
    BUDGET.store(max_events, Relaxed);
    ANCHOR_NS.store(now_ns(), Relaxed);
    ANCHOR_UNIX_US.store(unix_us_now(), Relaxed);
    STATE.store((state & !1).wrapping_add(2) | 1, Release);
    true
}

/// The armed capture's id; it changes with every [`start`].
#[must_use]
pub fn generation() -> u64 {
    STATE.load(Acquire) >> 1
}

/// Disarm and collect. `None` when nothing was armed.
#[must_use]
pub fn stop() -> Option<Capture> {
    stop_matching(None)
}

/// [`stop`], but only the capture [`generation`] named: a deadline set for
/// one capture must not end the next.
#[must_use]
pub fn stop_generation(generation: u64) -> Option<Capture> {
    stop_matching(Some(generation))
}

fn stop_matching(only: Option<u64>) -> Option<Capture> {
    let _control = CONTROL.lock().unwrap_or_else(PoisonError::into_inner);
    let state = STATE.load(Acquire);
    let generation = state >> 1;
    if only.is_some_and(|only| only != generation) || state & 1 == 0 {
        return None;
    }
    STATE.store(state & !1, Release);
    let anchor_ns = ANCHOR_NS.load(Relaxed);
    let duration = Duration::from_nanos(now_ns().saturating_sub(anchor_ns));
    let threads = {
        let mut threads = THREADS.lock().unwrap_or_else(PoisonError::into_inner);
        let all = threads.clone();
        // A buffer only this list holds belongs to a thread that has exited
        // (its thread-local dropped); collect it below, then forget it, so a
        // churning pool does not grow the list capture after capture.
        threads.retain(|buf| Arc::strong_count(buf) > 2);
        all
    };
    let mut collected = Vec::with_capacity(threads.len());
    for thread in threads {
        let mut slot = thread.events.lock().unwrap_or_else(PoisonError::into_inner);
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
    let admission = STATE.load(Acquire);
    if admission & 1 != 0 {
        push_generation(
            Key::Histogram(histogram),
            Kind::Span { dur_ns: nanos(dur) },
            admission,
        );
    }
}

/// Time a named region for the current capture; a no-op guard when unarmed.
///
/// For code that has no histogram of its own. Prefer a histogram where the
/// timing is worth keeping in reports too.
#[must_use]
pub fn span(name: &'static str) -> Span {
    let generation = STATE.load(Acquire);
    Span {
        name,
        started: (generation & 1 != 0).then(Instant::now),
        generation,
    }
}

/// Mark a point in time on the current thread's track.
pub fn instant(name: &'static str) {
    push(Key::Named(name), Kind::Instant);
}

/// Sample a counter track (`rss`, `heap.live`, `fps`) at this moment.
pub fn counter(name: &'static str, value: f64) {
    push(Key::Named(name), Kind::Counter { value });
}

/// Guard returned by [`span`].
#[derive(Debug)]
pub struct Span {
    name: &'static str,
    started: Option<Instant>,
    generation: u64,
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            push_generation(
                Key::Named(self.name),
                Kind::Span {
                    dur_ns: nanos(started.elapsed()),
                },
                self.generation,
            );
        }
    }
}

/// Append one event that ends now to this thread's buffer; a span's start
/// is backdated by its length.
fn push(key: Key, kind: Kind) {
    push_generation(key, kind, STATE.load(Acquire));
}

fn push_generation(key: Key, kind: Kind, admission: u64) {
    if admission & 1 == 0 {
        return;
    }
    let now = now_ns();
    let generation = admission >> 1;
    // `try_with`: a span can end while this thread's locals are being torn
    // down; that event is lost rather than panicking the thread.
    let _ = LOCAL.try_with(|cell| {
        let buf = cell.get_or_init(register_thread);
        let mut slot = buf.events.lock().unwrap_or_else(PoisonError::into_inner);
        // Stop drains under this same lock. An event racing stop (or the
        // next start) must not append to an already-collected capture.
        if STATE.load(Acquire) != admission {
            return;
        }
        if RECORDED.fetch_add(1, Relaxed) >= BUDGET.load(Relaxed) {
            DROPPED.fetch_add(1, Relaxed);
            return;
        }
        if slot.0 != generation {
            *slot = (generation, Vec::new());
        }
        slot.1.push(Event {
            key,
            kind,
            end_ns: now,
        });
    });
}

fn register_thread() -> Arc<ThreadBuf> {
    // Registration is a cold path. Serialize it with stop's thread-list
    // snapshot so stop always drains a registered writer before start can
    // publish another capture's budget.
    let _control = CONTROL.lock().unwrap_or_else(PoisonError::into_inner);
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
        let mut out = Vec::new();
        // Writing to a Vec cannot fail.
        let _ = self.write_chrome_json(&mut out, process, tables);
        String::from_utf8(out).unwrap_or_default()
    }

    /// [`Self::to_chrome_json`] streamed into `out` one event at a time, so
    /// a full capture never exists as a JSON tree in memory. Give it a
    /// `BufWriter` over the file.
    ///
    /// # Errors
    ///
    /// Whatever `out` returns.
    pub fn write_chrome_json(
        &self,
        mut out: impl std::io::Write,
        process: &str,
        tables: &[&[Metric]],
    ) -> std::io::Result<()> {
        let names: HashMap<usize, &'static str> = histogram_names(tables).into_iter().collect();
        out.write_all(b"{\"traceEvents\":[")?;
        write_metadata(&mut out, "process_name", 0, process)?;
        for thread in &self.threads {
            out.write_all(b",")?;
            write_metadata(&mut out, "thread_name", thread.tid, &thread.name)?;
            for event in &thread.events {
                out.write_all(b",")?;
                self.write_event(&mut out, event, &names, thread.tid)?;
            }
        }
        self.write_footer(out, process)
    }

    fn write_footer(&self, mut out: impl std::io::Write, process: &str) -> std::io::Result<()> {
        out.write_all(b"],\"displayTimeUnit\":\"ns\",\"otherData\":")?;
        serde_json::to_writer(
            &mut out,
            &json!({
                "process": process,
                "pid": std::process::id(),
                "anchor_unix_us": self.anchor_unix_us,
                "duration_us": u64::try_from(self.duration.as_micros()).unwrap_or(u64::MAX),
                "events": self.event_count(),
                "dropped": self.dropped,
            }),
        )?;
        out.write_all(b"}")?;
        out.flush()
    }

    fn write_event(
        &self,
        out: &mut impl std::io::Write,
        event: &Event,
        names: &HashMap<usize, &'static str>,
        tid: u64,
    ) -> std::io::Result<()> {
        let name: Cow<'_, str> = match event.key {
            Key::Named(name) => Cow::Borrowed(name),
            Key::Histogram(addr) => names.get(&addr).map_or_else(
                || Cow::Owned(format!("histogram@{addr:#x}")),
                |name| Cow::Borrowed(*name),
            ),
        };
        let mut line = OutEvent {
            ph: "X",
            cat: name.split('.').next().unwrap_or_default(),
            name: &name,
            pid: std::process::id(),
            tid,
            ts: Some(self.ts_us(event.end_ns)),
            dur: None,
            s: None,
            args: None,
        };
        match event.kind {
            Kind::Span { dur_ns } => {
                line.ts = line.ts.map(|end| end - ns_to_us(dur_ns));
                line.dur = Some(ns_to_us(dur_ns));
            }
            Kind::Instant => {
                line.ph = "i";
                line.s = Some("t");
            }
            Kind::Counter { value } => {
                line.ph = "C";
                line.args = Some(Args::Value { value });
            }
        }
        Ok(serde_json::to_writer(out, &line)?)
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

fn write_metadata(
    out: &mut impl std::io::Write,
    name: &str,
    tid: u64,
    label: &str,
) -> std::io::Result<()> {
    let meta = OutEvent {
        ph: "M",
        name,
        cat: "",
        pid: std::process::id(),
        tid,
        ts: None,
        dur: None,
        s: None,
        args: Some(Args::Name { name: label }),
    };
    Ok(serde_json::to_writer(out, &meta)?)
}

/// One trace event as written; fields absent for a kind are left out.
#[derive(Serialize)]
struct OutEvent<'a> {
    ph: &'static str,
    name: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    cat: &'a str,
    pid: u32,
    tid: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    ts: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dur: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    s: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    args: Option<Args<'a>>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Args<'a> {
    Name { name: &'a str },
    Value { value: f64 },
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

/// Exclusively create a trace file without overwriting an earlier capture,
/// including captures exported concurrently within one clock tick.
///
/// # Errors
///
/// The directory is not writable or no unique name could be created.
pub fn create_trace_file(
    dir: &std::path::Path,
    prefix: &str,
) -> std::io::Result<(std::path::PathBuf, std::fs::File)> {
    create_trace_file_at(dir, prefix, unix_us_now())
}

fn create_trace_file_at(
    dir: &std::path::Path,
    prefix: &str,
    stamp: u64,
) -> std::io::Result<(std::path::PathBuf, std::fs::File)> {
    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
    for _ in 0..100 {
        let sequence = NEXT_FILE.fetch_add(1, Relaxed);
        let path = dir.join(format!(
            "{prefix}-{stamp}-{}-{sequence}.json",
            std::process::id()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "trace filename collisions",
    ))
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
mod tests;
