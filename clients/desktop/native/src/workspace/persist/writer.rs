//! Ordered, coalesced disk writes. UI callers only enqueue immutable snapshots;
//! close/quit flushes them before releasing the window. Serialization, fsync,
//! and atomic replacement all stay on the writer thread.

use std::collections::{HashSet, VecDeque};
use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use super::{FileLayoutStore, SavedLayout, layout_value};

enum Snapshot {
    Layout(SavedLayout),
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
            snapshot: Snapshot::Layout(layout),
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
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

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
        let root =
            std::env::temp_dir().join(format!("desktop-history-retry-{}", std::process::id()));
        let store = FileLayoutStore::new(root.join("projects.json"), None);
        let mut history = super::super::RecentProjects::from_store(store.clone());
        history.writer = fail_once();
        history.record("/checkouts/a".into()).unwrap();
        assert_eq!(history.writer.flush(), ["transient save failure"]);
        history.record("/checkouts/a".into()).unwrap();
        assert!(history.flush().is_empty());
        assert_eq!(
            super::super::RecentProjects::from_store(store.clone()).entries(),
            [std::path::PathBuf::from("/checkouts/a")]
        );
        history.writer = fail_once();
        history
            .forget(std::path::Path::new("/checkouts/a"))
            .unwrap();
        assert_eq!(history.writer.flush(), ["transient save failure"]);
        history
            .forget(std::path::Path::new("/checkouts/a"))
            .unwrap();
        assert!(history.flush().is_empty());
        assert!(
            super::super::RecentProjects::from_store(store.clone())
                .entries()
                .is_empty()
        );
        history.writer = fail_once();
        history.record("/checkouts/b".into()).unwrap();
        // No identical operation this time: close/quit makes the retry.
        assert_eq!(history.flush(), ["transient save failure"]);
        assert_eq!(
            super::super::RecentProjects::from_store(store.clone()).entries(),
            [std::path::PathBuf::from("/checkouts/b")]
        );
        std::fs::remove_file(store.path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn read_after_write_barrier_preserves_a_host_round_trip_and_migration_source() {
        let root =
            std::env::temp_dir().join(format!("desktop-save-barrier-{}", std::process::id()));
        let a = FileLayoutStore::new(root.join("a.json"), None);
        let b = FileLayoutStore::new(root.join("b.json"), None);
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
        assert_eq!(a.read(), super::super::StoreValue::Present(1.into()));
        release_tx.send(()).unwrap();
        release_tx.send(()).unwrap();
        // Host/session adoption must use this barrier before restoring A or
        // importing its named layout into the canonical key-scoped destination.
        assert!(writer.flush().is_empty());
        assert_eq!(a.read(), super::super::StoreValue::Present(2.into()));
        let migrated = FileLayoutStore::new(root.join("canonical.json"), Some(a.path.clone()));
        assert_eq!(migrated.read(), a.read());
        std::fs::remove_file(a.path).unwrap();
        std::fs::remove_file(root.join("b.json")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn flush_atomically_persists_the_final_layout_and_leaves_no_staging_file() {
        let root = std::env::temp_dir().join(format!("desktop-writer-{}", std::process::id()));
        let store = FileLayoutStore::new(root.join("layout.json"), None);
        let writer = Writer::default();
        let mut layout =
            super::super::save_layout("server", &[], &crate::settings::default_display(), None);
        writer.layout(store.clone(), layout.clone());
        layout.display.font_size = 22.;
        writer.layout(store.clone(), layout.clone());
        assert!(writer.flush().is_empty());
        assert_eq!(
            super::super::load_layout(&store.read()).initial,
            Some(layout)
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_file(&store.path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    #[ignore = "observational disk benchmark; run with --ignored --nocapture"]
    fn measure_ui_save_cost() {
        let root = std::env::temp_dir().join(format!("desktop-save-perf-{}", std::process::id()));
        let store = FileLayoutStore::new(root.join("layout.json"), None);
        let layout =
            super::super::save_layout("server", &[], &crate::settings::default_display(), None);
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
        std::fs::remove_file(&store.path).unwrap();
        std::fs::remove_dir(root).unwrap();
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
}
