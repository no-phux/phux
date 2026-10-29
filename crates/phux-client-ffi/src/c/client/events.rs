use super::{
    AgentPublication, AgentRecordMode, Arc, BridgeError, Client, CloseReason, Event,
    EventProjection, Layer, Lifecycle, OwnedEffect, PhuxClientState, ResourceId, ResourceKind,
    RuntimeStatus, ServerFeature, TerminalSignal, encode_optional_i32, engine_bridge, grid,
    history_state_code, history_unavailable_code, projected_event, resource_summary,
};

impl Client {
    pub(crate) fn state(&self) -> PhuxClientState {
        if self.detached {
            return PhuxClientState::Detached;
        }
        // Bound the guard: a scrutinee temporary would hold the control
        // lock across every arm.
        let status = self.control().status();
        match status {
            RuntimeStatus::Idle => PhuxClientState::New,
            RuntimeStatus::Connecting => PhuxClientState::HelloQueued,
            RuntimeStatus::Negotiated => PhuxClientState::Negotiated,
            RuntimeStatus::Attached => PhuxClientState::Attached,
            RuntimeStatus::Closed => PhuxClientState::Detached,
            RuntimeStatus::Failed => PhuxClientState::Failed,
        }
    }

    pub(crate) fn set_error(&mut self, message: impl AsRef<str>) {
        self.last_error.clear();
        self.last_error
            .extend_from_slice(message.as_ref().as_bytes());
    }

    pub(crate) fn ensure_attached(&self) -> Result<(), BridgeError> {
        if self.control().status() == RuntimeStatus::Attached && !self.detached {
            Ok(())
        } else {
            Err(BridgeError::state("operation requires an attached client"))
        }
    }

    pub(crate) fn ensure_participant(&self, id: &ResourceId) -> Result<(), BridgeError> {
        if self.control().terminal_is_admitted(id) || self.is_agent_stream(id) {
            Ok(())
        } else {
            Err(BridgeError::protocol(
                "terminal state frame targets a terminal outside the active ATTACH",
            ))
        }
    }

    pub(crate) fn detach(&mut self) {
        self.retire_connection();
        self.render.clear();
        self.control().close();
        self.detached = true;
    }

    /// Retire binding-owned correlations, not the runtime's reconnect or
    /// durable-operation state. Published immutable frames remain displayable.
    fn retire_connection(&mut self) {
        self.agent_receipts.clear();
        self.operations.disconnect();
        self.arrangement.disconnect();
        self.directory.disconnect();
        self.session_query.disconnect();
        self.session_creates.disconnect();
        self.session_rename.disconnect();
        self.projection.disconnect();
        crate::c::organization::disconnect(self);
        self.outgoing.clear();
        self.sessions.clear();
        self.resources.clear();
        self.agent_streams.clear();
        self.attach_queued = false;
        self.expected_attach_id = None;
        self.attached = false;
        self.attached_notified = false;
        self.protocol_ready = false;
    }

    pub(crate) fn process_runtime_events(&mut self) -> Result<bool, BridgeError> {
        let mut attached = false;
        let events = self.runtime.take_events_leased();
        for event in events {
            attached |= self.process_runtime_event(event)?;
        }
        if std::mem::take(&mut self.org.sessions_dirty) {
            crate::c::organization::changed(self);
        }
        self.sync_server_features();
        self.publish_effects();
        self.drain_outbound();
        Ok(attached)
    }

    pub(crate) fn process_runtime_event(&mut self, event: Event) -> Result<bool, BridgeError> {
        if matches!(
            event,
            Event::ConnectionLost { .. } | Event::ConnectionOpened { .. }
        ) {
            self.owned_effects.clear();
            self.effect_count = 0;
        }
        if matches!(
            event,
            Event::ConnectionLost { .. }
                | Event::ConnectionOpened { .. }
                | Event::StatusChanged(RuntimeStatus::Failed)
        ) {
            self.retire_connection();
            return Ok(false);
        }
        let Some(event) = crate::c::organization::on_event(self, event) else {
            return Ok(false);
        };
        let event = match self.process_topology_event(event)? {
            EventProjection::Handled(attached) => return Ok(attached),
            EventProjection::Next(event) => event,
        };
        let event = match self.process_effect_event(event) {
            EventProjection::Handled(attached) => return Ok(attached),
            EventProjection::Next(event) => event,
        };
        let event = match self.process_engine_bridge_event(event)? {
            EventProjection::Handled(attached) => return Ok(attached),
            EventProjection::Next(event) => event,
        };
        self.process_lifecycle_event(event);
        Ok(false)
    }

    fn process_topology_event(&mut self, event: Event) -> Result<EventProjection, BridgeError> {
        match event {
            Event::TopologySnapshot {
                attach_id,
                snapshot,
            } => {
                self.resources = snapshot
                    .resources
                    .iter()
                    .map(|resource| resource_summary(&snapshot, resource))
                    .collect();
                for resource in &snapshot.resources {
                    if resource.kind == ResourceKind::AgentSession {
                        self.agent_streams
                            .entry(resource.id.clone())
                            .or_insert(None);
                    }
                }
                self.sessions = crate::c::arrangement::session_summaries(snapshot.clone())?;
                self.org.sessions_dirty = true;
                if attach_id.is_some() {
                    crate::c::arrangement::attached(self, snapshot);
                }
            }
            Event::TerminalChanged { terminal_id } => self.damage_effect(&terminal_id),
            Event::TerminalClosed { terminal_id, .. } => {
                self.finish_terminal_close(&terminal_id)?;
            }
            Event::Attached { .. } => {
                self.attach_queued = false;
                self.expected_attach_id = None;
                self.attached = true;
                crate::c::arrangement::initial_read(self);
                return Ok(EventProjection::Handled(true));
            }
            event => return Ok(EventProjection::Next(event)),
        }
        Ok(EventProjection::Handled(false))
    }

    fn process_effect_event(&mut self, event: Event) -> EventProjection {
        let signal = match projected_event::terminal_signal(event) {
            Ok(signal) => signal,
            Err(event) => return EventProjection::Next(event),
        };
        if let Some(effect) = self.signal_effect(signal) {
            self.owned_effects.push(effect);
        }
        EventProjection::Handled(false)
    }

    /// Encode one projected terminal signal as a `PHUX_CLIENT_EFFECT_STATUS`
    /// effect.
    ///
    /// The burst pair has no C vocabulary and never had one: Cockpit reads
    /// activity from the damage effects, so `OutputStarted`/`OutputSettled`
    /// encode to nothing. Clipboard writes remain unsupported in this lane.
    /// Every supported signal keeps its historical `(kind, detail)` pair and
    /// status code, which
    /// `include/phux/client.h` pins.
    fn signal_effect(&self, signal: TerminalSignal) -> Option<OwnedEffect> {
        Some(match signal {
            TerminalSignal::OutputStarted { .. } | TerminalSignal::OutputSettled { .. } => {
                return None;
            }
            // The C ABI has no clipboard effect or foreground/focus policy
            // contract. Do not turn a remote write into an unguarded desktop
            // clipboard action; only the UniFFI lane exposes it for now.
            TerminalSignal::ClipboardWrite { .. } => return None,
            TerminalSignal::Bell { terminal_id } => self.stamped(2, 1, &terminal_id),
            TerminalSignal::TitleChanged { terminal_id, title } => {
                let mut effect = self.stamped(2, 2, &terminal_id);
                effect.bytes = title.into_bytes();
                effect
            }
            TerminalSignal::Resync {
                terminal_id,
                reason,
            } => {
                let mut effect = self.stamped(2, 3, &terminal_id);
                effect.status_code = u32::from(reason.as_wire());
                effect
            }
            TerminalSignal::History {
                terminal_id,
                status,
            } => {
                let mut effect = self.stamped(2, 6, &terminal_id);
                effect.status_code = history_state_code(status.state);
                effect
            }
            TerminalSignal::HistoryUnavailable {
                terminal_id,
                reason,
            } => {
                let mut effect = self.stamped(2, 7, &terminal_id);
                effect.status_code = history_unavailable_code(reason);
                effect
            }
            TerminalSignal::CwdChanged { terminal_id, cwd } => {
                let mut effect = OwnedEffect::simple(2, 8, terminal_id);
                effect.bytes = cwd.into_bytes();
                effect
            }
            TerminalSignal::CommandStarted { terminal_id } => {
                OwnedEffect::simple(2, 9, terminal_id)
            }
            TerminalSignal::CommandFinished {
                terminal_id,
                exit_code,
            } => {
                let mut effect = OwnedEffect::simple(2, 10, terminal_id);
                encode_optional_i32(&mut effect.stream_id, &mut effect.bootstrap_id, exit_code);
                effect
            }
            TerminalSignal::InputHolder {
                terminal_id,
                holder,
                mine,
                action,
            } => {
                let mut effect = OwnedEffect::simple(2, 12, terminal_id);
                effect.status_code = u32::from(action.to_u8());
                if let Some(holder) = holder {
                    effect.stream_id = 1;
                    effect.bootstrap_id = u64::from(holder.get());
                }
                effect.first_row = u16::from(mine);
                effect
            }
        })
    }

    /// One status effect with the published replica identity already copied
    /// onto it.
    fn stamped(&self, kind: u32, detail: u32, id: &ResourceId) -> OwnedEffect {
        let mut effect = OwnedEffect::simple(kind, detail, id.clone());
        self.stamp_replica_generation(&mut effect, id);
        effect
    }

    fn process_engine_bridge_event(
        &mut self,
        event: Event,
    ) -> Result<EventProjection, BridgeError> {
        match event {
            Event::AgentPublication { publication, .. } => {
                self.agent_publication_effect(publication);
            }
            Event::AgentSessionClosed {
                terminal_id,
                stream,
                ..
            } => {
                if let Some(view) = self.agent_streams.get_mut(&terminal_id) {
                    *view = stream;
                }
                self.finish_terminal_close(&terminal_id)?;
            }
            Event::KernelSend(send) => self.process_send(send)?,
            Event::Frame(frame) => crate::c::dispatch_extension_frame(self, *frame)?,
            event => return Ok(EventProjection::Next(event)),
        }
        Ok(EventProjection::Handled(false))
    }

    /// Encode the projected lifecycle facts the C ABI carries.
    ///
    /// A close is not one of them: `Event::TerminalClosed` is consumed
    /// earlier, because releasing a terminal is bookkeeping rather than an
    /// encoding. The spawn/attach/detach answers have no C vocabulary —
    /// `crate::c::operations` correlates those from the frames themselves.
    fn process_lifecycle_event(&mut self, event: Event) {
        let Ok(lifecycle) = projected_event::lifecycle(event) else {
            return;
        };
        match lifecycle {
            Lifecycle::Exited {
                terminal_id,
                exit_status,
                signal,
                reason,
            } => {
                if !self.is_agent_stream(&terminal_id) {
                    self.exit_effect(terminal_id, exit_status, signal, reason);
                }
            }
            Lifecycle::Detached { reason, message } => {
                let mut effect = OwnedEffect::simple(2, 5, ResourceId::local(0));
                effect.status_code = reason
                    .map_or(crate::c::types::DETACH_REASON_UNSTATED, |reason| {
                        u32::from(reason.as_wire())
                    });
                effect.bytes = message.into_bytes();
                self.detach();
                self.owned_effects.push(effect);
            }
            Lifecycle::ServerError { code, message, .. } => {
                self.set_error(&message);
                let mut effect = OwnedEffect::simple(2, 4, ResourceId::local(0));
                effect.bytes = format!("{code:?}: {message}").into_bytes();
                self.owned_effects.push(effect);
            }
            Lifecycle::PaneSpawned { .. }
            | Lifecycle::SpawnAnswered { .. }
            | Lifecycle::AttachAnswered { .. }
            | Lifecycle::DetachAnswered { .. }
            | Lifecycle::Closed { .. } => {}
        }
    }

    pub(crate) fn finish_terminal_close(&mut self, id: &ResourceId) -> Result<(), BridgeError> {
        self.arrangement.subscriptions.cancel(id);
        self.arrangement.subscriptions.mark_closed(id);
        if let Some(stream) = self.agent_streams.remove(id) {
            let (stream_id, bootstrap_id) = stream.map_or((0, 0), |info| {
                (info.stream_id.get(), info.bootstrap_id.get())
            });
            let mut effect = OwnedEffect::simple(
                crate::c::types::EFFECT_AGENT_RECORDS,
                crate::c::types::AGENT_RECORDS_CLOSED,
                id.clone(),
            );
            effect.stream_id = stream_id;
            effect.bootstrap_id = bootstrap_id;
            self.owned_effects.push(effect);
        } else {
            crate::c::operations::release_terminal(self, id)?;
            self.operations.observe_resource_closed(id);
            self.owned_effects
                .push(OwnedEffect::simple(1, 3, id.clone()));
        }
        self.render.remove(id);
        self.resources.retain(|resource| &resource.id != id);
        Ok(())
    }

    fn damage_effect(&mut self, id: &ResourceId) {
        self.render.remove(id);
        let Some(frame) = self.control().publication().acquire(id) else {
            self.owned_effects
                .push(OwnedEffect::simple(1, 3, id.clone()));
            return;
        };
        let mut effect = OwnedEffect::simple(1, 0, id.clone());
        match grid::view(&frame).damage {
            // DECSCNM and other mode-only publication still change
            // metadata the host paints from. Swallowing Clean left
            // reverse-video and default colors on the previous canvas.
            phux_client_core::grid::GridDamage::Full
            | phux_client_core::grid::GridDamage::Clean => effect.detail = 1,
            phux_client_core::grid::GridDamage::Rows => {
                let Some((first, last)) = grid::dirty_row_span(&frame) else {
                    return;
                };
                effect.detail = 2;
                effect.first_row = first;
                effect.last_row = last;
            }
        }
        self.owned_effects.push(effect);
    }

    /// Copy the published replica identity onto a status effect.
    ///
    /// Cockpit fences bells and titles with `sameReplica(stream, bootstrap)`.
    /// The pre-shim bridge stamped those fields from the kernel key; the
    /// runtime `Event` no longer carries it, so the current publication is
    /// the remaining source.
    fn stamp_replica_generation(&self, effect: &mut OwnedEffect, id: &ResourceId) {
        let Ok(info) = self
            .engine()
            .and_then(|engine| engine.replica_info(id).map_err(engine_bridge))
        else {
            return;
        };
        effect.stream_id = info.stream_id;
        effect.bootstrap_id = info.bootstrap_id;
    }

    fn exit_effect(
        &mut self,
        id: ResourceId,
        exit_status: Option<i32>,
        signal: Option<i32>,
        reason: CloseReason,
    ) {
        let mut effect = OwnedEffect::simple(2, 11, id);
        effect.status_code = u32::from(reason.as_wire());
        encode_optional_i32(&mut effect.stream_id, &mut effect.bootstrap_id, exit_status);
        effect.first_row = signal
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(0);
        self.owned_effects.push(effect);
    }

    pub(crate) fn accept_agent_publications(&mut self) {
        let mut control = self.runtime.control();
        for publication in self.agent_receipts.drain(..) {
            control.accept_agent_publication(&publication);
        }
    }

    fn agent_publication_effect(&mut self, publication: Arc<AgentPublication>) {
        self.agent_streams
            .insert(publication.resource_id.clone(), Some(publication.stream));
        if let Some(batch) = crate::projection::agent_records::project(&publication) {
            let detail = match batch.mode {
                AgentRecordMode::Replace => crate::c::types::AGENT_RECORDS_RETAINED,
                AgentRecordMode::Append => crate::c::types::AGENT_RECORDS_LIVE,
            };
            let mut effect = OwnedEffect::simple(
                crate::c::types::EFFECT_AGENT_RECORDS,
                detail,
                publication.resource_id.clone(),
            );
            effect.stream_id = publication.stream.stream_id.get();
            effect.bootstrap_id = publication.stream.bootstrap_id.get();
            effect.seq = batch.seq;
            effect.bytes = batch.jsonl.into_bytes();
            self.owned_effects.push(effect);
        }
        self.agent_receipts.push(publication);
    }

    fn sync_server_features(&mut self) {
        let server = {
            let control = self.control();
            matches!(
                control.status(),
                RuntimeStatus::Negotiated | RuntimeStatus::Attached
            )
            .then(|| control.server().cloned())
            .flatten()
        };
        let Some(server) = server else {
            self.protocol_ready = false;
            return;
        };
        self.protocol_ready = true;
        self.server_id.clone_from(&server.id);
        self.list_directory = server.has(ServerFeature::ListDirectory);
        self.list_directory_host = server.has(ServerFeature::ListDirectoryHost);
        self.keep_empty_sessions = server.has(ServerFeature::KeepEmptySessions);
        self.conditional_kill = server.has(ServerFeature::ConditionalKill);
        self.event_journal = server.has(ServerFeature::EventJournal);
        self.retain_on_exit = server.has(ServerFeature::RetainOnExit);
        self.spawn_idempotency = server.has(ServerFeature::SpawnIdempotency);
        self.close_tab_resources = server.has(ServerFeature::CloseTabResources);
        self.attach_roles = server.has(ServerFeature::AttachRoles);
        self.l3_metadata = server.layers.contains(Layer::L3);
        crate::c::organization::seed(self);
    }
}
