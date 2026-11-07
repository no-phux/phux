//! The connection's documents, and the worker that decodes surface frames.
//!
//! A producer can publish several PNG frames a second, and decoding one takes
//! milliseconds in a release build and far longer in a debug one, so frames
//! are decoded off the UI thread. Only the newest pending frame of each
//! surface is decoded; older ones are dropped unseen. Every image a newer one
//! replaces, or a closed resource leaves behind, is handed back through
//! [`Documents::take_retired`] so the window frees its GPU atlas entry.
use std::collections::HashMap;
use std::sync::Arc;
#[cfg(not(test))]
use std::sync::{Condvar, Mutex};

use phux_protocol::ResourceId;

use super::{Content, Document, decode};

pub(crate) struct Documents {
    map: HashMap<ResourceId, Document>,
    retired: Vec<Arc<gpui::RenderImage>>,
    #[cfg(not(test))]
    worker: Arc<Worker>,
}

#[cfg(not(test))]
#[derive(Default)]
struct Queue {
    pending: HashMap<ResourceId, Document>,
    done: Vec<(ResourceId, Document)>,
}

#[cfg(not(test))]
struct Worker {
    queue: Mutex<Queue>,
    ready: Condvar,
}

impl Documents {
    #[cfg_attr(test, expect(unused_variables, reason = "tests decode inline"))]
    pub(crate) fn new(wake: async_channel::Sender<()>) -> Self {
        Self {
            map: HashMap::new(),
            retired: Vec::new(),
            #[cfg(not(test))]
            worker: Worker::spawn(wake),
        }
    }

    /// Store a decoded document, retiring the image it replaces.
    pub(crate) fn insert(&mut self, id: ResourceId, document: Document) {
        if let Some(old) = self.map.insert(id, document) {
            self.retire(old);
        }
    }

    /// Queue a surface frame for the worker; tests decode it at once.
    pub(crate) fn submit(&mut self, id: ResourceId, document: Document) {
        #[cfg(test)]
        self.insert(id, decoded(document));
        #[cfg(not(test))]
        {
            lock(&self.worker.queue).pending.insert(id, document);
            self.worker.ready.notify_one();
        }
    }

    /// Fold in frames the worker finished since the last drain.
    pub(crate) fn collect(&mut self) {
        #[cfg(not(test))]
        {
            let done = std::mem::take(&mut lock(&self.worker.queue).done);
            for (id, document) in done {
                self.insert(id, document);
            }
        }
    }

    pub(crate) fn get(&self, id: &ResourceId) -> Option<&Document> {
        self.map.get(id)
    }

    pub(crate) fn remove(&mut self, id: &ResourceId) {
        if let Some(old) = self.map.remove(id) {
            self.retire(old);
        }
    }

    pub(crate) fn clear(&mut self) {
        let old: Vec<Document> = self.map.drain().map(|(_, document)| document).collect();
        for document in old {
            self.retire(document);
        }
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&ResourceId, &Document) -> bool) {
        let gone: Vec<ResourceId> = self
            .map
            .iter()
            .filter(|(id, document)| !keep(id, document))
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            self.remove(&id);
        }
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Images no document shows any more, for the window to free.
    pub(crate) fn take_retired(&mut self) -> Vec<Arc<gpui::RenderImage>> {
        std::mem::take(&mut self.retired)
    }

    fn retire(&mut self, document: Document) {
        if let Content::Image(image) = document.content {
            self.retired.push(image);
        }
    }
}

/// Decode a queued frame whose content still holds the raw PNG.
fn decoded(mut document: Document) -> Document {
    if let Content::Pending(bytes) = &document.content {
        document.content = decode(
            document.record.kind,
            document.content_type.as_deref(),
            bytes,
        );
    }
    document
}

#[cfg(not(test))]
impl Worker {
    fn spawn(wake: async_channel::Sender<()>) -> Arc<Self> {
        let worker = Arc::new(Self {
            queue: Mutex::new(Queue::default()),
            ready: Condvar::new(),
        });
        let weak = Arc::downgrade(&worker);
        let spawned = std::thread::Builder::new()
            .name("phux-surface-decode".into())
            .spawn(move || run(&weak, &wake));
        if let Err(error) = spawned {
            log::error!("surface decode worker: {error}");
        }
        worker
    }
}

/// Decode until the connection drops its handle; a wait times out each
/// second so a dropped connection ends the thread.
#[cfg(not(test))]
fn run(weak: &std::sync::Weak<Worker>, wake: &async_channel::Sender<()>) {
    while let Some(worker) = weak.upgrade() {
        let batch = {
            let mut queue = lock(&worker.queue);
            if queue.pending.is_empty() {
                queue = worker
                    .ready
                    .wait_timeout(queue, std::time::Duration::from_secs(1))
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
            }
            std::mem::take(&mut queue.pending)
        };
        if batch.is_empty() {
            continue;
        }
        let finished: Vec<_> = batch
            .into_iter()
            .map(|(id, document)| (id, decoded(document)))
            .collect();
        lock(&worker.queue).done.extend(finished);
        let _ = wake.try_send(());
    }
}

#[cfg(not(test))]
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
