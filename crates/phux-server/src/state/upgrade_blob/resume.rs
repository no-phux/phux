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

/// What a carried resource needed that did not cross.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Missing {
    /// No rebuilt resource that can parent it holds its parent's wire id.
    Parent,
    /// The merged records hold none under its key.
    Record,
}

impl Missing {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Parent => "parent",
            Self::Record => "record",
        }
    }
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
    /// Each carried resource a kind dropped, already logged.
    skipped: HashSet<u32>,
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
            skipped: HashSet::new(),
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

    /// Drop the `kind` resource carried under `wire`: what it needs did not
    /// cross. Losing one must not cost the whole tree. Logged once per
    /// resource, however many kinds look at it.
    pub(crate) fn skip(&mut self, kind: &'static str, wire: u32, parent: u32, missing: Missing) {
        if self.skipped.insert(wire) {
            tracing::warn!(
                kind,
                resource = wire,
                parent,
                missing = missing.as_str(),
                "resume: carried resource's {} did not cross; dropped",
                missing.as_str()
            );
        }
    }

    /// Every rebuilt resource's exit receiver. A carried resource no kind
    /// rebuilt or skipped had a record no kind claims; it is dropped too.
    pub(in crate::state) fn finish(self) -> ExitWatchers {
        for wire in resource_wires(self.blob) {
            if !self.rebuilt.contains_key(&wire) && !self.skipped.contains(&wire) {
                tracing::warn!(
                    resource = wire,
                    "resume: no kind claims the carried resource's record; dropped"
                );
            }
        }
        self.exit_watchers
    }
}
