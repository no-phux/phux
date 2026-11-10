//! What an app's rebuild works on: the fresh state a resume builds, the
//! blob, and the resources apps before it rebuilt.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

/// One kind's half of a resume. Its app wrote its part of the blob at the
/// cut and rebuilds that part here.
pub(crate) trait KindRebuild {
    /// Rebuild this kind's carried engines on the fresh state, under the
    /// blob's wire ids. An error aborts the whole resume; a resource that
    /// cannot cross on its own (its parent did not) is skipped instead.
    fn rebuild(&mut self, cx: &mut ResumeCx<'_>) -> Result<(), RebuildError>;

    /// The rebuilt tree is committed: restore what this kind keeps outside
    /// its engines. Runs under the same lock, before any client connects.
    fn restore(&mut self, state: &mut ServerState, blob: &StateBlob);
}

/// The fresh state of one resume, as each [`KindRebuild`] sees it.
pub(crate) struct ResumeCx<'a> {
    /// The state being built. Committed only if every kind rebuilds.
    pub(crate) state: &'a mut ServerState,
    /// What the old image carried.
    pub(crate) blob: &'a StateBlob,
    keyed: &'a Keyed,
    /// Each resource rebuilt so far, by wire id.
    rebuilt: HashMap<u32, ResourceId>,
    exit_watchers: ExitWatchers,
}

impl<'a> ResumeCx<'a> {
    pub(in crate::state) fn new(
        state: &'a mut ServerState,
        blob: &'a StateBlob,
        keyed: &'a Keyed,
    ) -> Self {
        Self {
            state,
            blob,
            keyed,
            rebuilt: HashMap::new(),
            exit_watchers: Vec::new(),
        }
    }

    /// The record the blob names for the resource under `wire`, after the
    /// carried records merged with the store's.
    pub(crate) fn record(&self, wire: u32) -> Option<ResourceId> {
        let key = self
            .keyed
            .panes
            .get(&wire)
            .or_else(|| self.keyed.resources.get(&wire))?;
        self.state.registry().resource_id(key)
    }

    /// The rebuilt resource under `wire`, if an earlier kind rebuilt one
    /// that can parent another.
    pub(crate) fn parent(&self, wire: u32) -> Option<ResourceId> {
        let core = *self.rebuilt.get(&wire)?;
        let record = self.state.registry().record(core)?;
        record.kind.can_parent().then_some(core)
    }

    /// Register `handle` as `core`'s engine under the blob's wire id and
    /// spawn it, watching `exit_notify` once the tree is committed.
    ///
    /// # Errors
    ///
    /// [`RebuildError::Registration`] when the engine does not match the
    /// record it would own.
    pub(crate) fn spawn(
        &mut self,
        core: ResourceId,
        wire: u32,
        handle: ResourceHandle,
        token: tokio_util::sync::CancellationToken,
        engine: impl std::future::Future<Output = ()> + 'static,
        exit_notify: Option<oneshot::Receiver<phux_core::process::ExitOutcome>>,
    ) -> Result<(), RebuildError> {
        // Pre-bind so the registration's intern returns the blob's id.
        self.state
            .idspace
            .bind_resource(core, WireResourceId::local(wire));
        self.state
            .spawn_resource_actor(core, handle, token, engine)?;
        self.exit_watchers.extend(exit_notify.map(|rx| (core, rx)));
        self.rebuilt.insert(wire, core);
        Ok(())
    }

    /// Every rebuilt resource's exit receiver. A carried resource no kind
    /// rebuilt is dropped; losing one must not cost the whole tree.
    pub(in crate::state) fn finish(self) -> ExitWatchers {
        for wire in resource_wires(self.blob) {
            if !self.rebuilt.contains_key(&wire) {
                tracing::warn!(
                    resource = wire,
                    "resume: carried resource's parent or record did not cross; dropped"
                );
            }
        }
        self.exit_watchers
    }
}
