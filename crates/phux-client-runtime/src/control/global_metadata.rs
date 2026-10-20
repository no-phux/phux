//! Watched `Global` metadata keys: a binding asks for a key once and then
//! drains its value as it is read and as it changes. Generic: the key is the
//! binding's business (`phux.usage/v1`, say), not the runtime's.
//!
//! Subscriptions end with the attachment that made them (L3.md §1.2), so a
//! watched key is subscribed and read again on every attach and after a
//! journal gap. The drain is level-shaped: a key changed twice before the
//! binding drains it once yields only its newest value, so the queue is
//! bounded by the number of watched keys.

use phux_protocol::caps::Layer;
use phux_protocol::wire::frame::Scope;

use super::{ControlPlane, FrameKind, HashMap};

/// How many distinct keys one connection watches.
pub const MAX_WATCHED_GLOBAL_KEYS: usize = 32;

/// One observed value of a watched `Global` key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalMetadata {
    /// The metadata key.
    pub key: String,
    /// Its value; `None` when the key is absent or was deleted.
    pub value: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
pub(super) struct GlobalWatch {
    /// Keys to keep subscribed, in the order first asked for.
    keys: Vec<String>,
    /// In-flight `GET_METADATA` reads by request id.
    pending: HashMap<u32, String>,
    /// Values not yet drained, oldest first, one per key.
    values: Vec<GlobalMetadata>,
}

impl GlobalWatch {
    fn push(&mut self, key: String, value: Option<Vec<u8>>) {
        self.values.retain(|seen| seen.key != key);
        self.values.push(GlobalMetadata { key, value });
    }
}

impl ControlPlane {
    /// Subscribe to one `Global` metadata key and read its current value.
    /// Values arrive through [`Self::take_global_metadata`]. Idempotent; the
    /// key is subscribed and read again on every later attach.
    ///
    /// Returns the read's request id, or zero when nothing was sent: an
    /// empty key, more than [`MAX_WATCHED_GLOBAL_KEYS`] keys, or no L3
    /// connection yet (the key is still remembered for the next attach).
    pub fn watch_global_metadata(&mut self, key: String) -> u32 {
        if key.is_empty() {
            return 0;
        }
        if !self.global_watch.keys.contains(&key) {
            if self.global_watch.keys.len() >= MAX_WATCHED_GLOBAL_KEYS {
                return 0;
            }
            self.global_watch.keys.push(key.clone());
        }
        if !self.speaks_l3() {
            return 0;
        }
        self.issue_global_watch(key)
    }

    /// Drain the watched keys' values observed since the last drain: read
    /// replies and live changes, oldest first, newest value per key.
    pub fn take_global_metadata(&mut self) -> Vec<GlobalMetadata> {
        std::mem::take(&mut self.global_watch.values)
    }

    fn speaks_l3(&self) -> bool {
        self.handshake_ready
            && self
                .server
                .as_ref()
                .is_some_and(|server| server.layers.contains(Layer::L3))
    }

    fn issue_global_watch(&mut self, key: String) -> u32 {
        self.queue_frame(&FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: key.clone(),
        });
        let request_id = self.next_request_id();
        self.queue_frame(&FrameKind::GetMetadata {
            request_id,
            scope: Scope::Global,
            key: key.clone(),
        });
        self.global_watch.pending.insert(request_id, key);
        request_id
    }

    /// Subscribe to and read every watched key again: a new attachment
    /// starts with no subscriptions (`attached`, which also forgets reads
    /// a previous connection left unanswered), and a gap may have hidden a
    /// change.
    pub(super) fn rewatch_global_metadata(&mut self, attached: bool) {
        if attached {
            self.global_watch.pending.clear();
        }
        if !self.speaks_l3() {
            return;
        }
        for key in self.global_watch.keys.clone() {
            self.issue_global_watch(key);
        }
    }

    /// Consume this watch's read replies; record live changes of watched
    /// keys and leave those frames for other consumers.
    pub(super) fn take_global_metadata_frame(&mut self, frame: FrameKind) -> Option<FrameKind> {
        match frame {
            FrameKind::MetadataValue { request_id, value }
                if self.global_watch.pending.contains_key(&request_id) =>
            {
                if let Some(key) = self.global_watch.pending.remove(&request_id) {
                    self.global_watch.push(key, value);
                }
                None
            }
            FrameKind::MetadataChanged {
                scope: Scope::Global,
                key,
                value,
                actor,
            } => {
                if self.global_watch.keys.contains(&key) {
                    self.global_watch.push(key.clone(), value.clone());
                }
                Some(FrameKind::MetadataChanged {
                    scope: Scope::Global,
                    key,
                    value,
                    actor,
                })
            }
            other => Some(other),
        }
    }

    /// A refused read of a watched key ends quietly: the key stays absent
    /// to this connection (a grant without `observe@global`, say).
    pub(super) fn resolve_global_metadata_error(&mut self, request_id: Option<u32>) -> bool {
        request_id.is_some_and(|id| self.global_watch.pending.remove(&id).is_some())
    }
}
