//! Pending snapshots, request ids, and the satellite metadata mirror.

use super::{
    FrameKind, Outbound, ProxySubscriber, RelaySession, ResourceId, Scope, SnapshotGate, mpsc,
};

impl RelaySession {
    /// Retry one subscriber's retained bootstrap prefix. Returns
    /// `(may_send_delta, subscriber_alive)`.
    pub(super) fn flush_pending_snapshot(sub: &mut ProxySubscriber) -> (bool, bool) {
        let gate = std::mem::replace(&mut sub.gate, SnapshotGate::AwaitingFirst);
        if let SnapshotGate::Retiring {
            failure,
            deadline,
            detach_upstream,
        } = gate
        {
            return match sub.out_tx.try_send(Outbound::Frame(failure.clone())) {
                Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => (false, false),
                Err(mpsc::error::TrySendError::Full(_)) => {
                    if std::time::Instant::now() >= deadline {
                        // Other clones keep the mailbox alive; cancel the
                        // connection itself.
                        sub.consumer_cancel.cancel();
                        (false, false)
                    } else {
                        sub.gate = SnapshotGate::Retiring {
                            failure,
                            deadline,
                            detach_upstream,
                        };
                        (false, true)
                    }
                }
            };
        }
        let SnapshotGate::Retained {
            mut frames,
            mut retained_bytes,
            open_after,
        } = gate
        else {
            let open = matches!(gate, SnapshotGate::Open);
            sub.gate = gate;
            return (open, true);
        };
        while let Some(frame) = frames.front() {
            match sub.out_tx.try_send(Outbound::Frame(frame.clone())) {
                Ok(()) => {
                    retained_bytes =
                        retained_bytes.saturating_sub(Self::retained_frame_bytes(frame));
                    frames.pop_front();
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    sub.gate = SnapshotGate::Retained {
                        frames,
                        retained_bytes,
                        open_after,
                    };
                    return (false, true);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    sub.gate = SnapshotGate::Open;
                    return (false, false);
                }
            }
        }
        sub.gate = if open_after {
            SnapshotGate::Open
        } else {
            SnapshotGate::AwaitingFirst
        };
        (open_after, !sub.retire_after_flush)
    }

    /// Retry every retained snapshot (keepalive tick), for terminals with no
    /// further traffic to trigger the inline retry.
    pub(crate) fn flush_pending_snapshots(&mut self) {
        let mut orphaned = Vec::new();
        for (terminal, subs) in &mut self.subscribers {
            let detach_if_empty = subs.iter().any(Self::detach_upstream_on_removal);
            subs.retain_mut(|sub| {
                if matches!(
                    sub.gate,
                    SnapshotGate::Retained { .. } | SnapshotGate::Retiring { .. }
                ) {
                    Self::flush_pending_snapshot(sub).1
                } else {
                    true
                }
            });
            if subs.is_empty() && detach_if_empty {
                orphaned.push(*terminal);
            }
        }
        self.subscribers.retain(|_, subs| !subs.is_empty());
        self.orphaned_by_delivery.extend(orphaned);
        self.recalculate_retained_totals();
    }

    pub(crate) fn take_delivery_detaches(&mut self) -> Vec<Vec<u8>> {
        let orphaned = std::mem::take(&mut self.orphaned_by_delivery)
            .into_iter()
            .filter(|terminal| !self.subscribers.contains_key(terminal))
            .collect();
        self.encode_withdrawals(orphaned)
    }

    pub(super) fn recalculate_retained_totals(&mut self) {
        let mut bytes = 0usize;
        let mut frames = 0usize;
        for sub in self.subscribers.values().flatten() {
            if let SnapshotGate::Retained {
                frames: queued,
                retained_bytes,
                ..
            } = &sub.gate
            {
                bytes = bytes.saturating_add(*retained_bytes);
                frames = frames.saturating_add(queued.len());
            }
        }
        self.retained_bytes = bytes;
        self.retained_frames = frames;
    }

    /// Next link-side request id, skipping ids still pending (wrap safety).
    pub(super) fn allocate_request_id(&mut self) -> u32 {
        loop {
            let id = self.next_request_id;
            self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
            if !self.request_id_in_use(id) {
                return id;
            }
        }
    }

    /// Whether any reply map still holds the link-side id `id`.
    pub(super) fn request_id_in_use(&self, id: u32) -> bool {
        self.pending.contains_key(&id)
            || self.pending_spawns.contains_key(&id)
            || self.pending_listings.contains_key(&id)
            || self.pending_paths.contains_key(&id)
            || self.pending_detaches.contains_key(&id)
            || self.pending_mirror_gets.contains_key(&id)
    }

    /// Store one allowlisted `METADATA_CHANGED`, retagged to `Satellite`,
    /// for a terminal this link asked to mirror. The satellite chooses the
    /// ids it reports, so an unrequested one would let it grow the hub's
    /// store without bound.
    pub(super) fn apply_mirrored_metadata(&self, scope: &Scope, key: &str, value: Option<Vec<u8>>) {
        let Scope::Resource(ResourceId::Local { id }) = scope else {
            return;
        };
        if !self.mirrored.contains(id) {
            return;
        }
        let Some(journal) = &self.journal else {
            return;
        };
        let journal = journal.clone();
        let host = self.host.clone();
        journal.with_mut(|state| {
            crate::hub::metadata_mirror::apply_mirrored(state, &host, scope, key, value);
        });
    }

    /// Store one allowlisted `METADATA_VALUE` correlated to a mirror GET.
    pub(super) fn apply_mirrored_value(&mut self, request_id: u32, value: Option<Vec<u8>>) {
        let Some((terminal, key)) = self.pending_mirror_gets.remove(&request_id) else {
            return;
        };
        self.apply_mirrored_metadata(&Scope::Resource(ResourceId::local(terminal)), &key, value);
    }

    /// Drop the mirror for a satellite terminal that closed.
    pub(super) fn forget_mirror(&mut self, terminal: u32) {
        self.mirrored.remove(&terminal);
        self.pending_mirror_gets
            .retain(|_, (id, _)| *id != terminal);
        let Some(journal) = &self.journal else {
            return;
        };
        let journal = journal.clone();
        let host = self.host.clone();
        journal.with_mut(|state| {
            crate::hub::metadata_mirror::forget_mirrored_terminal(state, &host, terminal);
            state.forget_satellite_lease_terminal(&host, terminal);
        });
    }

    pub(super) fn encode(&mut self, frame: &FrameKind) -> Vec<u8> {
        self.encode_buf.clear();
        frame.encode(&mut self.encode_buf);
        self.encode_buf.to_vec()
    }
}
