//! Rebuilding server state from a blob.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

/// A committed rebuild: every rebuilt resource's exit receiver (see
/// [`ServerState::rebuild_from_blob`]).
#[derive(Debug)]
pub(crate) struct RebuiltTree {
    /// Each rebuilt resource's exit receiver.
    pub(crate) exit_watchers: ExitWatchers,
}

/// The records a blob names, by wire id, after they are merged with the
/// store's.
pub(in crate::state) struct Keyed {
    pub(in crate::state) sessions: HashMap<u32, SessionKey>,
    /// The tree's panes, which the windows and layouts name.
    pub(in crate::state) panes: HashMap<u32, ResourceKey>,
    /// Every other carried resource.
    pub(in crate::state) resources: HashMap<u32, ResourceKey>,
}

impl ServerState {
    /// Rebuild the tree from a [`StateBlob`] in the re-exec'd image
    /// (ADR-0032): merge the carried records with the store's (or import a
    /// pre-record tree), restore allocators and ledgers, then let each of
    /// `kinds` rebuild the engines its app carried, in order, so a parent's
    /// app runs before its children's. Returns each rebuilt resource's exit
    /// receiver.
    ///
    /// Transactional: built on a fresh state and committed only on full
    /// success; the store is written only once the tree is built. Runs
    /// inside the actors' `LocalSet`.
    ///
    /// # Errors
    ///
    /// [`RebuildError`] for bad topology, records, store, or actor
    /// failures, or dangling wire ids.
    pub(crate) fn rebuild_from_blob(
        &mut self,
        blob: &StateBlob,
        kinds: &mut [&mut dyn KindRebuild],
    ) -> Result<RebuiltTree, RebuildError> {
        validate_upgrade_blob(blob)?;
        let mut fresh = Self::new();
        fresh.config.scrollback = self.config.scrollback;
        fresh.config.agent_log_bytes = self.config.agent_log_bytes;
        // ADR-0109: a pre-token blob keeps this process's token.
        if blob.counters.server_instance.is_none() {
            fresh.idspace.set_instance(self.idspace.instance());
        }
        fresh.restore_counters(blob);
        let RebuiltRecords {
            keyed,
            imported,
            arranged_now,
        } = fresh.rebuild_records(self.org(), self.org.loaded, blob)?;
        let mut cx = ResumeCx::new(&mut fresh, blob, &keyed);
        for kind in kinds.iter_mut() {
            kind.rebuild(&mut cx)?;
        }
        let exit_watchers = cx.finish();
        if imported {
            self.org.store.import(
                fresh.org(),
                Some(crate::org_store::LIVE_IMPORT_MIGRATION),
                crate::state::now_stamp(),
            )?;
        } else if !self.org.loaded || arranged_now {
            self.org
                .store
                .import(fresh.org(), None, crate::state::now_stamp())?;
        }
        self.commit_rebuilt_tree(fresh);
        self.restore_upgrade_metadata(blob);
        for kind in kinds.iter_mut() {
            kind.restore(self, blob);
        }
        Ok(RebuiltTree { exit_watchers })
    }

    /// Install the blob's organization records in this fresh server and bind
    /// its sessions: every step of a resume that needs no live process, and
    /// so all [`dry_run_records`] can prove.
    fn rebuild_records(
        &mut self,
        current: &Org,
        loaded: bool,
        blob: &StateBlob,
    ) -> Result<RebuiltRecords, RebuildError> {
        let instance = self.instance_id();
        let (mut org, keyed, imported) = match &blob.org {
            Some(carried) => merge_carried(current, loaded, carried, blob, instance)?,
            None => import_tree(current, blob, instance)?,
        };
        // Every session has an arrangement (SPEC L2 §2.2); a carried session
        // written before that rule gets its missing one now, and the store
        // learns it after the rebuild.
        let arranged_now = org.sessions().any(|s| s.arrangement.is_none());
        if arranged_now {
            crate::state::org::ensure_arrangements(&mut org);
        }
        self.sessions.registry = phux_core::Registry::new(org);
        self.bind_session_wires(blob, &keyed)?;
        Ok(RebuiltRecords {
            keyed,
            imported,
            arranged_now,
        })
    }

    /// Bind every carried session's wire id, and its synthetic window's, to
    /// the session's new handle; restore its touch stamp, root, and cwd.
    pub(in crate::state) fn bind_session_wires(
        &mut self,
        blob: &StateBlob,
        keyed: &Keyed,
    ) -> Result<(), RebuildError> {
        let windows: HashMap<u32, &WindowBlob> =
            blob.windows.iter().map(|w| (w.wire_id, w)).collect();
        for s in &blob.sessions {
            let key = keyed
                .sessions
                .get(&s.wire_id)
                .ok_or(RebuildError::DanglingRef {
                    kind: "session",
                    id: s.wire_id,
                })?;
            let core = self
                .sessions
                .registry
                .session_id(key)
                .ok_or(RebuildError::DanglingRef {
                    kind: "session record",
                    id: s.wire_id,
                })?;
            self.idspace
                .bind_session(core, WireSessionId::new(s.wire_id));
            // The first window stands for the session's synthetic one.
            if let Some(&window) = s.window_wire_ids.first() {
                self.idspace.bind_window(core, WireWindowId::new(window));
                if let Some(cwd) = windows.get(&window).and_then(|w| w.last_cwd.clone()) {
                    self.sessions.record_last_cwd(core, cwd);
                }
            }
            if let Some(ts) = s.last_touched {
                self.sessions.bind_last_touched(core, ts);
            }
            if let Some(root) = &s.root {
                self.sessions.bind_root(core, root.clone());
            }
        }
        Ok(())
    }

    /// Reinstate the carried Group- and Global-scope metadata. No client is
    /// connected yet, so there is no one to notify.
    pub(in crate::state) fn restore_upgrade_metadata(&mut self, blob: &StateBlob) {
        for entry in &blob.metadata {
            let scope = entry
                .group
                .map_or(phux_protocol::wire::frame::Scope::Global, |gid| {
                    phux_protocol::wire::frame::Scope::Group(phux_protocol::ids::GroupId::new(gid))
                });
            let _ = self.metadata.set(&scope, &entry.key, entry.value.clone());
        }
    }

    /// Install a rebuilt tree, replacing only what reconstruction owns. The
    /// store stays this state's.
    pub(in crate::state) fn commit_rebuilt_tree(&mut self, mut fresh: Self) {
        std::mem::swap(&mut self.sessions, &mut fresh.sessions);
        std::mem::swap(&mut self.idspace, &mut fresh.idspace);
        std::mem::swap(&mut self.resources, &mut fresh.resources);
        std::mem::swap(&mut self.retained, &mut fresh.retained);
        self.org.loaded = true;
    }

    /// Close every pane the old image retained after exit (ADR-0124 §6),
    /// through its exit watcher. Returns how many.
    pub fn close_upgrade_retained(&mut self, blob: &StateBlob) -> u32 {
        let retained: Vec<ResourceId> = blob
            .panes
            .iter()
            .filter(|pane| pane.retained_exit.is_some())
            .filter_map(|pane| self.resource_from_wire(&WireResourceId::local(pane.wire_id)))
            .collect();
        self.close_resources(
            &retained,
            phux_protocol::wire::frame::CloseReason::ServerShutdown,
        )
    }

    /// Restore the allocators above every restored id.
    pub(in crate::state) const fn restore_counters(&mut self, blob: &StateBlob) {
        self.idspace
            .set_next_session_wire(blob.counters.next_session_wire_id);
        self.idspace
            .set_next_terminal_wire(blob.counters.next_terminal_wire_id);
        self.idspace
            .set_next_window_wire(blob.counters.next_window_wire_id);
        self.sessions
            .set_next_touch_timestamp(blob.counters.next_touch_timestamp);
        // ADR-0109: the terminal allocator was restored, so no id can repeat
        // and the token that names the id space must survive with it.
        if let Some(bytes) = blob.counters.server_instance {
            self.idspace
                .set_instance(phux_protocol::ids::ServerInstance::new(bytes));
        }
    }
}

/// What [`ServerState::rebuild_records`] leaves for the rest of the rebuild.
struct RebuiltRecords {
    keyed: Keyed,
    /// The tree came from an image that predates the records.
    imported: bool,
    /// A carried session gained its arrangement, so the store must learn it.
    arranged_now: bool,
}

/// Run a resume's record rebuild against an empty in-memory server. Touches
/// no store and no process, so a candidate binary can answer it while the
/// running image can still refuse the upgrade.
pub(crate) fn dry_run_records(blob: &StateBlob) -> Result<(), RebuildError> {
    validate_upgrade_blob(blob)?;
    let mut fresh = ServerState::new();
    fresh.restore_counters(blob);
    let empty = ServerState::new();
    fresh.rebuild_records(empty.org(), false, blob).map(drop)
}
