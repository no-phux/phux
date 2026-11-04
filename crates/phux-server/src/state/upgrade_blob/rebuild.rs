//! Rebuilding server state from a blob.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

/// What the pane pass produces: the wire-id -> core-id map, each rebuilt
/// pane's exit receiver, and what the caller's wiring returned for each pane.
pub(in crate::state) struct RebuiltPanes<W> {
    pub(in crate::state) core_ids: HashMap<u32, ResourceId>,
    pub(in crate::state) exit_watchers: PaneExitWatchers,
    pub(in crate::state) wired: Vec<(ResourceId, W)>,
}

/// A committed rebuild: every resource's exit receiver, and each pane's
/// wiring (see [`ServerState::rebuild_from_blob`]).
#[derive(Debug)]
pub(crate) struct RebuiltTree<W> {
    /// Each rebuilt resource's (pane or agent session) exit receiver.
    pub(crate) exit_watchers: PaneExitWatchers,
    /// Each rebuilt pane's core id and what `wire_pane` returned for it.
    pub(crate) wired_panes: Vec<(ResourceId, W)>,
}

/// The records a blob names, by wire id, after they are merged with the
/// store's.
pub(in crate::state) struct Keyed {
    pub(in crate::state) sessions: HashMap<u32, SessionKey>,
    pub(in crate::state) panes: HashMap<u32, ResourceKey>,
    pub(in crate::state) agent_sessions: HashMap<u32, ResourceKey>,
}

impl ServerState {
    /// Rebuild the tree from a [`StateBlob`] in the re-exec'd image
    /// (ADR-0032): merge the carried records with the store's (or import a
    /// pre-record tree), recreate handles under their wire ids, restore
    /// allocators and ledgers, and spawn actors that re-adopt PTYs (or
    /// replay snapshots), then rebind carried agent sessions to their panes.
    /// `wire_pane` runs on each pane's actor before it starts, so the runtime
    /// installs the same sinks a fresh spawn gets. Returns each rebuilt
    /// resource's exit receiver and each pane's wiring.
    ///
    /// Transactional: built on a fresh state and committed only on full
    /// success; the store is written only once the tree is built. Runs
    /// inside the actors' `LocalSet`.
    ///
    /// # Errors
    ///
    /// [`RebuildError`] for bad topology, records, store, or actor
    /// failures, or dangling wire ids.
    #[allow(
        clippy::type_complexity,
        reason = "the runtime immediately consumes each rebuilt pane id and its one-shot exit receiver"
    )]
    pub(crate) fn rebuild_from_blob<W>(
        &mut self,
        blob: &StateBlob,
        build_pane: impl FnMut(
            ResourceId,
            &PaneBlob,
            phux_config::ScrollbackLimits,
        ) -> Result<RebuiltPane<W>, RebuildError>,
    ) -> Result<RebuiltTree<W>, RebuildError> {
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
        let mut panes = fresh.rebuild_panes(blob, &keyed, build_pane)?;
        fresh.rebuild_agent_sessions(blob, &keyed, &mut panes);
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
        self.restore_native_agents(blob);
        Ok(RebuiltTree {
            exit_watchers: panes.exit_watchers,
            wired_panes: panes.wired,
        })
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

    /// Bind every carried pane's wire id to its record's new handle and
    /// spawn the actor that re-adopts its PTY.
    pub(in crate::state) fn rebuild_panes<W>(
        &mut self,
        blob: &StateBlob,
        keyed: &Keyed,
        mut build_pane: impl FnMut(
            ResourceId,
            &PaneBlob,
            phux_config::ScrollbackLimits,
        ) -> Result<RebuiltPane<W>, RebuildError>,
    ) -> Result<RebuiltPanes<W>, RebuildError> {
        let scrollback = self.config.scrollback;
        let mut panes = RebuiltPanes {
            core_ids: HashMap::new(),
            exit_watchers: Vec::with_capacity(blob.panes.len()),
            wired: Vec::with_capacity(blob.panes.len()),
        };
        for p in &blob.panes {
            let core = keyed
                .panes
                .get(&p.wire_id)
                .and_then(|key| self.sessions.registry.resource_id(key))
                .ok_or(RebuildError::DanglingRef {
                    kind: "pane record",
                    id: p.wire_id,
                })?;
            if let Some(desc) = self.sessions.registry.terminal_mut(core) {
                desc.dims = (p.cols, p.rows);
                desc.cwd.clone_from(&p.cwd);
                desc.title.clone_from(&p.title);
            }

            let RebuiltPane {
                handle,
                token,
                engine,
                exit_notify,
                wired,
            } = build_pane(core, p, scrollback)?;
            // Pre-bind so the spawn's intern returns the blob's id.
            self.idspace
                .bind_resource(core, WireResourceId::local(p.wire_id));
            panes.wired.push((core, wired));
            self.spawn_resource_actor(core, handle, token, engine);
            // ADR-0124: what the old image knew about this pane's retention.
            if let Some(secs) = p.retain_secs {
                self.note_retain_request(core, secs);
            }
            if let Some(exit) = p.retained_exit {
                self.restore_retained_exit(core, exit_facet(exit));
            }
            if let Some(exit_notify) = exit_notify {
                panes.exit_watchers.push((core, exit_notify));
            }
            panes.core_ids.insert(p.wire_id, core);
        }
        Ok(panes)
    }

    /// Rebind every carried `AgentSession` to its record under its recorded
    /// wire id, with an engine that continues the old stream: same record
    /// counter, same retained tail, same `ended`. The parent's
    /// `REPORT_AGENT_STATE` is re-bound to the stream (ADR-0103 §6). A
    /// session whose parent pane is not in the blob is dropped; losing one
    /// session must not cost the whole tree.
    pub(in crate::state) fn rebuild_agent_sessions<W>(
        &mut self,
        blob: &StateBlob,
        keyed: &Keyed,
        panes: &mut RebuiltPanes<W>,
    ) {
        let log_bytes = self.config.agent_log_bytes;
        for carried in &blob.agent_sessions {
            let Some(&parent) = panes.core_ids.get(&carried.parent_wire_id) else {
                tracing::warn!(
                    session = carried.wire_id,
                    parent = carried.parent_wire_id,
                    "resume: agent session's parent pane did not cross; dropped"
                );
                continue;
            };
            let Some(core) = keyed
                .agent_sessions
                .get(&carried.wire_id)
                .and_then(|key| self.sessions.registry.resource_id(key))
            else {
                tracing::warn!(
                    session = carried.wire_id,
                    "resume: agent session has no record; dropped"
                );
                continue;
            };
            if let Some(facet) = self
                .sessions
                .registry
                .resource_mut(core)
                .and_then(|d| d.agent.as_mut())
            {
                facet.state.clone_from(&carried.state);
            }
            let token = CancellationToken::new();
            let bundle = AgentSessionActor::restore(
                parent,
                &carried.provider,
                carried.native_id.as_deref(),
                token.clone(),
                log_bytes,
                agent_session_cut(carried),
            );
            let agent_parent = self
                .resource_handle(parent)
                .and_then(|handle| handle.companion::<phux_agent_session::parent::AgentParent>());
            if let (Some(agent_parent), Ok(session)) = (agent_parent, bundle.handle.agent_session())
            {
                // The parent's mailbox is fresh, so this cannot be full.
                let _ = agent_parent.mailbox.try_send(
                    phux_agent_session::parent::AgentParentRequest::BindSession {
                        append: session.append.clone(),
                    },
                );
            }
            // Pre-bind so the registration's intern returns the blob's id.
            self.idspace
                .bind_resource(core, WireResourceId::local(carried.wire_id));
            self.spawn_resource_actor(core, bundle.handle, token, bundle.actor.run());
            panes.exit_watchers.push((core, bundle.exit_notify));
        }
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
