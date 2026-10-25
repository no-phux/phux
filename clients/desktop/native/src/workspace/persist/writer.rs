//! Ordered, coalesced disk writes. UI callers only enqueue immutable snapshots;
//! close/quit flushes them before releasing the window. Serialization, fsync,
//! and atomic replacement all stay on the writer thread.

use std::collections::{HashSet, VecDeque};
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use super::{FileLayoutStore, SavedLayout, layout_value};

enum Snapshot {
    Layout(Box<SavedLayout>),
    Json(serde_json::Value),
}

struct Write {
    store: FileLayoutStore,
    snapshot: Snapshot,
}

impl Write {
    fn commit(self) -> io::Result<()> {
        let value = match self.snapshot {
            Snapshot::Layout(layout) => layout_value(&layout),
            Snapshot::Json(value) => value,
        };
        self.store.write(&value)
    }
}

#[derive(Default)]
struct Queue {
    pending: VecDeque<Write>,
    writing: bool,
    closing: bool,
    failed: HashSet<std::path::PathBuf>,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    changed: Condvar,
}

/// One serial writer for an owner's files. New snapshots replace queued writes
/// to the same path, never an in-flight write. Different paths (including a
/// layout's old and new migration destinations) are never discarded.
pub struct Writer {
    shared: Arc<Shared>,
    errors: async_channel::Receiver<String>,
    // Close the listener on the owning thread, not as the worker exits. GPUI's
    // deterministic scheduler also relies on this during entity teardown.
    _errors_open: async_channel::Sender<String>,
    thread: Option<JoinHandle<()>>,
}

impl Default for Writer {
    fn default() -> Self {
        Self::start(Write::commit)
    }
}

impl Writer {
    fn start(commit: impl Fn(Write) -> io::Result<()> + Send + 'static) -> Self {
        let shared = Arc::new(Shared::default());
        let (tx, errors) = async_channel::unbounded();
        let errors_open = tx.clone();
        let worker = shared.clone();
        let thread = std::thread::spawn(move || {
            while let Some(write) = take_write(&worker) {
                let path = write.store.path.clone();
                let result = commit(write);
                let mut queue = worker.queue.lock().unwrap();
                match result {
                    Ok(()) => {
                        queue.failed.remove(&path);
                    }
                    Err(error) => {
                        queue.failed.insert(path);
                        log::error!("desktop save: {error}");
                        let _ = tx.try_send(error.to_string());
                    }
                }
                queue.writing = false;
                worker.changed.notify_all();
            }
        });
        Self {
            shared,
            errors,
            _errors_open: errors_open,
            thread: Some(thread),
        }
    }

    pub fn layout(&self, store: FileLayoutStore, layout: SavedLayout) {
        self.enqueue(Write {
            store,
            snapshot: Snapshot::Layout(Box::new(layout)),
        });
    }

    pub fn json(&self, store: FileLayoutStore, value: serde_json::Value) {
        self.enqueue(Write {
            store,
            snapshot: Snapshot::Json(value),
        });
    }

    fn enqueue(&self, write: Write) {
        let mut queue = self.shared.queue.lock().unwrap();
        queue
            .pending
            .retain(|old| old.store.path != write.store.path);
        queue.pending.push_back(write);
        self.shared.changed.notify_one();
    }

    /// Listen without polling. Failure messages are also retained until close
    /// when no UI listener is installed.
    pub fn errors(&self) -> async_channel::Receiver<String> {
        self.errors.clone()
    }

    pub fn failed(&self, path: &std::path::Path) -> bool {
        self.shared.queue.lock().unwrap().failed.contains(path)
    }

    /// Only close/quit or a read-after-write boundary may wait for disk.
    pub fn flush(&self) -> Vec<String> {
        let mut queue = self.shared.queue.lock().unwrap();
        while queue.writing || !queue.pending.is_empty() {
            queue = self.shared.changed.wait(queue).unwrap();
        }
        std::iter::from_fn(|| self.errors.try_recv().ok()).collect()
    }
}

fn take_write(shared: &Shared) -> Option<Write> {
    let mut queue = shared.queue.lock().unwrap();
    loop {
        if let Some(write) = queue.pending.pop_front() {
            queue.writing = true;
            return Some(write);
        }
        if queue.closing {
            return None;
        }
        queue = shared.changed.wait(queue).unwrap();
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        {
            let mut queue = self.shared.queue.lock().unwrap();
            queue.closing = true;
            self.shared.changed.notify_one();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::Scratch;
    use super::super::{RecentProjects, StoreValue, load_layout, save_layout};
    use super::*;
    use crate::settings::default_display;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::Duration;

    type Committed = Arc<Mutex<Vec<(PathBuf, serde_json::Value)>>>;

    fn fail_once() -> Writer {
        let first = std::sync::atomic::AtomicBool::new(true);
        Writer::start(move |write| {
            if first.swap(false, std::sync::atomic::Ordering::Relaxed) {
                Err(io::Error::other("transient save failure"))
            } else {
                write.commit()
            }
        })
    }

    #[test]
    fn identical_history_operations_and_quit_retry_failed_saves() {
        let scratch = Scratch::new("history-retry");
        let store = FileLayoutStore::new(scratch.join("projects.json"), None);
        let entries = || RecentProjects::from_store(store.clone()).entries().to_vec();
        let mut history = RecentProjects::from_store(store.clone());
        history.writer = fail_once();
        history.record("/checkouts/a".into()).unwrap();
        assert_eq!(history.writer.flush(), ["transient save failure"]);
        history.record("/checkouts/a".into()).unwrap();
        assert!(history.flush().is_empty());
        assert_eq!(entries(), [PathBuf::from("/checkouts/a")]);
        history.writer = fail_once();
        history.forget(Path::new("/checkouts/a")).unwrap();
        assert_eq!(history.writer.flush(), ["transient save failure"]);
        history.forget(Path::new("/checkouts/a")).unwrap();
        assert!(history.flush().is_empty());
        assert!(entries().is_empty());
        history.writer = fail_once();
        history.record("/checkouts/b".into()).unwrap();
        // No identical operation this time: close/quit makes the retry.
        assert_eq!(history.flush(), ["transient save failure"]);
        assert_eq!(entries(), [PathBuf::from("/checkouts/b")]);
    }

    #[test]
    fn read_after_write_barrier_preserves_a_host_round_trip_and_migration_source() {
        let scratch = Scratch::new("save-barrier");
        let a = FileLayoutStore::new(scratch.join("a.json"), None);
        let b = FileLayoutStore::new(scratch.join("b.json"), None);
        a.write(&1.into()).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer = Writer::start(move |write| {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            write.commit()
        });
        writer.json(a.clone(), 2.into());
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        writer.json(b, 3.into());
        assert_eq!(a.read(), StoreValue::Present(1.into()));
        release_tx.send(()).unwrap();
        release_tx.send(()).unwrap();
        // Host/session adoption must use this barrier before restoring A or
        // importing its named layout into the canonical key-scoped destination.
        assert!(writer.flush().is_empty());
        assert_eq!(a.read(), StoreValue::Present(2.into()));
        let migrated = FileLayoutStore::new(scratch.join("canonical.json"), Some(a.path.clone()));
        assert_eq!(migrated.read(), a.read());
    }

    #[test]
    fn flush_atomically_persists_the_final_layout_and_leaves_no_staging_file() {
        let scratch = Scratch::new("writer");
        let store = FileLayoutStore::new(scratch.join("layout.json"), None);
        let writer = Writer::default();
        let mut layout = save_layout("server", &[], &default_display(), None);
        writer.layout(store.clone(), layout.clone());
        layout.display.font_size = 22.;
        writer.layout(store.clone(), layout.clone());
        assert!(writer.flush().is_empty());
        assert_eq!(load_layout(&store.read()).initial, Some(layout));
        assert_eq!(std::fs::read_dir(&scratch.path).unwrap().count(), 1);
    }

    #[test]
    #[ignore = "observational disk benchmark; run with --ignored --nocapture"]
    fn measure_ui_save_cost() {
        let scratch = Scratch::new("save-perf");
        let store = FileLayoutStore::new(scratch.join("layout.json"), None);
        let layout = save_layout("server", &[], &default_display(), None);
        let start = std::time::Instant::now();
        for _ in 0..100 {
            store.write(&layout_value(&layout)).unwrap();
        }
        let synchronous = start.elapsed();
        let writer = Writer::default();
        let start = std::time::Instant::now();
        for _ in 0..100 {
            writer.layout(store.clone(), layout.clone());
        }
        let enqueue = start.elapsed();
        assert!(writer.flush().is_empty());
        println!(
            "100 saves: synchronous UI serialization+fsync {synchronous:?}; UI enqueue {enqueue:?} (disk flushed afterward)"
        );
    }

    #[test]
    fn a_blocked_disk_does_not_block_enqueue_and_latest_pending_snapshot_wins() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let committed = Arc::new(Mutex::new(Vec::new()));
        let writes = committed.clone();
        let writer = Writer::start(move |write| {
            let Snapshot::Json(value) = write.snapshot else {
                panic!("JSON fixture")
            };
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            writes.lock().unwrap().push((write.store.path, value));
            Ok(())
        });
        let a = FileLayoutStore::new("a.json", None);
        let b = FileLayoutStore::new("b.json", None);
        writer.json(a.clone(), 1.into());
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // The worker cannot finish until this thread releases it. These calls
        // therefore prove the UI enqueue does not await that disk operation.
        writer.json(a.clone(), 2.into());
        writer.json(b, 3.into());
        writer.json(a, 4.into());
        for _ in 0..3 {
            release_tx.send(()).unwrap();
        }
        assert!(writer.flush().is_empty());
        assert_eq!(
            *committed.lock().unwrap(),
            vec![
                ("a.json".into(), 1.into()),
                ("b.json".into(), 3.into()),
                ("a.json".into(), 4.into()),
            ]
        );
    }

    #[test]
    fn failures_are_observable_and_do_not_stop_subsequent_saves() {
        let writer = Writer::start(|_| Err(io::Error::other("disk unavailable")));
        writer.json(FileLayoutStore::new("a.json", None), 1.into());
        assert_eq!(writer.flush(), ["disk unavailable"]);
        writer.json(FileLayoutStore::new("b.json", None), 2.into());
        assert_eq!(writer.flush(), ["disk unavailable"]);
    }

    #[test]
    fn dropping_the_owner_finishes_queued_saves() {
        let committed = Arc::new(Mutex::new(0));
        let writes = committed.clone();
        let writer = Writer::start(move |_| {
            *writes.lock().unwrap() += 1;
            Ok(())
        });
        writer.json(FileLayoutStore::new("a.json", None), 1.into());
        drop(writer);
        assert_eq!(*committed.lock().unwrap(), 1);
    }

    /// Holds the worker inside its first commit so later enqueues are pending,
    /// not in flight. Those enqueues run on this thread while the worker is
    /// blocked, which is the responsiveness proof: they return before `release`.
    fn block_writer() -> (Writer, mpsc::Sender<()>, Committed) {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let committed = Arc::new(Mutex::new(Vec::new()));
        let writes = Arc::clone(&committed);
        let writer = Writer::start(move |write| {
            let Snapshot::Json(value) = write.snapshot else {
                panic!("JSON fixture");
            };
            let path = write.store.path;
            if path.ends_with("stall.json") {
                started_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("stall released");
            }
            writes.lock().unwrap().push((path, value));
            Ok(())
        });
        writer.json(FileLayoutStore::new("stall.json", None), 0.into());
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("writer started");
        (writer, release_tx, committed)
    }

    #[test]
    fn rapid_persists_to_one_destination_keep_only_the_latest_payload() {
        let (writer, release, committed) = block_writer();
        let destination = FileLayoutStore::new("layout.json", None);
        // The worker cannot finish the stall until `release`. These enqueues
        // therefore neither wait on that commit nor schedule A or B.
        writer.json(destination.clone(), "A".into());
        writer.json(destination.clone(), "B".into());
        writer.json(destination, "C".into());
        release.send(()).unwrap();
        assert!(writer.flush().is_empty());
        assert_eq!(
            *committed.lock().unwrap(),
            vec![
                ("stall.json".into(), 0.into()),
                ("layout.json".into(), "C".into()),
            ]
        );
    }

    #[test]
    fn retarget_keeps_later_snapshots_off_the_previous_destination() {
        let (writer, release, committed) = block_writer();
        let mut store = FileLayoutStore::new("session-a.json", None);
        writer.json(store.clone(), "layout-a".into());
        // Host/session adoption replaces the desk's store. The snapshot already
        // queued keeps the path it captured; later persists use the new one.
        store.path = PathBuf::from("session-b.json");
        writer.json(store.clone(), "layout-b".into());
        writer.json(store, "layout-c".into());
        release.send(()).unwrap();
        assert!(writer.flush().is_empty());
        assert_eq!(
            *committed.lock().unwrap(),
            vec![
                ("stall.json".into(), 0.into()),
                ("session-a.json".into(), "layout-a".into()),
                ("session-b.json".into(), "layout-c".into()),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_write_keeps_the_previous_bytes_and_stays_unacknowledged() {
        let scratch = Scratch::new("failed-write");
        let path = scratch.join("layout.json");
        let store = FileLayoutStore::new(&path, None);
        let writer = Writer::default();
        writer.json(store.clone(), "good".into());
        assert!(writer.flush().is_empty());
        assert!(!writer.failed(&path));
        let good = std::fs::read(&path).unwrap();

        let original = std::fs::metadata(&scratch.path).unwrap().permissions();
        let mut locked = original.clone();
        locked.set_readonly(true);
        std::fs::set_permissions(&scratch.path, locked).unwrap();
        writer.json(store.clone(), "bad".into());
        let errors = writer.flush();
        let mut restored = original.clone();
        restored.set_mode(original.mode());
        std::fs::set_permissions(&scratch.path, restored).unwrap();

        assert!(
            !errors.is_empty(),
            "failure is reported asynchronously, got {errors:?}"
        );
        assert!(writer.failed(&path));
        assert_eq!(std::fs::read(&path).unwrap(), good);
        assert!(!scratch.has_staging());

        let other = FileLayoutStore::new(scratch.join("other.json"), None);
        writer.json(other.clone(), "elsewhere".into());
        assert!(writer.flush().is_empty());
        assert!(
            writer.failed(&path),
            "another destination's success does not acknowledge this one"
        );
        assert!(!writer.failed(&other.path));

        writer.json(store.clone(), "recovered".into());
        assert!(writer.flush().is_empty());
        assert!(!writer.failed(&path));
        assert_eq!(store.read(), StoreValue::Present("recovered".into()));
    }

    #[test]
    fn dropping_the_writer_flushes_the_latest_coalesced_snapshot() {
        let scratch = Scratch::new("quit-flush");
        let store = FileLayoutStore::new(scratch.join("layout.json"), None);
        let writer = Writer::default();
        writer.json(store.clone(), "A".into());
        writer.json(store.clone(), "B".into());
        writer.json(store.clone(), "C".into());
        drop(writer);
        assert_eq!(store.read(), StoreValue::Present("C".into()));
        assert_eq!(std::fs::read_dir(&scratch.path).unwrap().count(), 1);
    }
}
