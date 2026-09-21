//! Completion evidence belongs to the exact Client that queued the close.
//! Server Ok acknowledges cancellation; only `RESOURCE_CLOSED` proves reap.

use super::{BridgeError, Operations, Pending, ResourceId};

#[derive(Clone, Debug)]
pub(super) struct PendingClose {
    targets: Vec<ResourceId>,
    closed: Vec<bool>,
    acknowledged: bool,
}

impl PendingClose {
    /// Queue preflight bounds this to `1..=MAX_DYNAMIC_TERMINALS` unique targets.
    pub(super) fn new(targets: Vec<ResourceId>) -> Self {
        Self {
            closed: vec![false; targets.len()],
            targets,
            acknowledged: false,
        }
    }

    pub(super) fn terminal(&self) -> Option<ResourceId> {
        self.targets.first().cloned()
    }

    pub(super) fn contains(&self, id: &ResourceId) -> bool {
        self.targets.contains(id)
    }

    fn acknowledge(&mut self) -> Result<(), BridgeError> {
        if self.acknowledged {
            return Err(BridgeError::protocol("duplicate close acknowledgement"));
        }
        self.acknowledged = true;
        Ok(())
    }

    fn observe_closed(&mut self, id: &ResourceId) {
        if let Some(index) = self.targets.iter().position(|target| target == id) {
            self.closed[index] = true;
        }
    }

    fn complete(&self) -> bool {
        self.acknowledged && self.closed.iter().all(|closed| *closed)
    }
}

impl Pending {
    const fn close_mut(&mut self) -> Option<&mut PendingClose> {
        match self {
            Self::Close(close) | Self::CloseMany(close) => Some(close),
            _ => None,
        }
    }
}

impl Operations {
    /// Returns true when this was an explicit close acknowledgement, including
    /// when it must remain pending. Other operation kinds keep their contract.
    pub(super) fn acknowledge_close(&mut self, request_id: u32) -> Result<bool, BridgeError> {
        let Some(close) = self
            .pending
            .get_mut(&request_id)
            .and_then(Pending::close_mut)
        else {
            return Ok(false);
        };
        close.acknowledge()?;
        if close.complete() {
            self.complete(request_id, 1, None, 0, 0, "");
        }
        Ok(true)
    }

    /// Called exclusively after applying a real `RESOURCE_CLOSED`. Admission
    /// release, detach, and replacement inventory must never call this hook.
    pub(crate) fn observe_resource_closed(&mut self, id: &ResourceId) {
        let mut completed = Vec::new();
        for (request, pending) in &mut self.pending {
            let Some(close) = pending.close_mut() else {
                continue;
            };
            close.observe_closed(id);
            if close.complete() {
                completed.push(*request);
            }
        }
        completed.sort_unstable();
        for request in completed {
            self.complete(request, 1, None, 0, 0, "");
        }
    }
}
