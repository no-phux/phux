//! Satellite pane adoption and the orphan kills that follow a refused attach.

use super::{
    AttachError, Command, Connection, FrameKind, FrameOutcome, HostAnswers, ParkedAdopt,
    ResourceId, SATELLITE_PROBE_INTERVAL, SatelliteHost, parked_spawned_panes,
    send_unless_peer_gone, unanswered_spawns,
};

impl super::SessionLoop {
    /// Attach layout leaves this client is not subscribed to yet (a peer's
    /// headless placement, a returned satellite).
    pub(super) async fn attach_discovered_panes(
        &mut self,
        conn: &mut Connection,
        terminal_ids: &[ResourceId],
    ) -> Result<(), AttachError> {
        for terminal_id in terminal_ids {
            let request_id = self.take_request_id();
            // Correlate the refusal: it is the only evidence a restored leaf
            // names a resource that died with a previous server.
            self.mirror
                .pending_resource_ops
                .insert(request_id, terminal_id.clone());
            self.send_attach_resource(conn, request_id, terminal_id)
                .await?;
        }
        Ok(())
    }

    /// `ATTACH_RESOURCE` one pane under `request_id`, tracking the QUIC
    /// stream bind its affirmative reply opens.
    pub(super) async fn send_attach_resource(
        &mut self,
        conn: &mut Connection,
        request_id: u32,
        terminal_id: &ResourceId,
    ) -> Result<(), AttachError> {
        if conn.multistream_enabled() {
            self.track_pending_stream_bind(request_id, terminal_id.clone())?;
        }
        let frame = FrameKind::Command {
            request_id,
            command: Command::AttachResource {
                terminal_id: terminal_id.clone(),
                role_policy: crate::attach::attach_role::pane_attach_role(),
            },
        };
        if let Err(error) = send_unless_peer_gone(conn, &frame).await {
            self.pending_stream_binds.remove(&request_id);
            return Err(error);
        }
        Ok(())
    }

    /// Park each window/split spawned on a satellite and attach its pane; the
    /// reply opens it or bells with the host name.
    pub(super) async fn attach_spawned_panes(
        &mut self,
        conn: &mut Connection,
        parked: Vec<ParkedAdopt>,
    ) -> Result<(), AttachError> {
        for adopt in parked {
            let Some(terminal_id) = adopt.pane().cloned() else {
                continue;
            };
            let request_id = self.take_request_id();
            self.park_adopt(request_id, adopt);
            self.send_attach_resource(conn, request_id, &terminal_id)
                .await?;
        }
        Ok(())
    }

    /// Best-effort kill of spawned satellite panes whose attach was refused.
    pub(super) async fn kill_orphaned_spawns(
        &mut self,
        conn: &mut Connection,
        panes: Vec<ResourceId>,
    ) -> Result<(), AttachError> {
        for frame in self
            .orphan_kills
            .kill_frames(panes, &mut self.next_request_id)
        {
            send_unless_peer_gone(conn, &frame).await?;
        }
        Ok(())
    }

    /// Act on the orphans one frame left: kill what is reachable, remember
    /// bound ones an unreachable satellite stranded, and kill strays on each
    /// satellite that just answered a spawn (`answered`).
    pub(super) async fn settle_orphans(
        &mut self,
        conn: &mut Connection,
        outcome: &mut FrameOutcome,
        answered: &[ResourceId],
    ) -> Result<(), AttachError> {
        self.kill_orphaned_spawns(conn, std::mem::take(&mut outcome.kill_orphans))
            .await?;
        self.orphan_kills.record_unreachable(
            std::mem::take(&mut outcome.unreachable_strays),
            std::time::Instant::now(),
        );
        self.orphan_kills.forget_reissued(answered);
        let hosts: Vec<SatelliteHost> = answered
            .iter()
            .filter_map(ResourceId::host)
            .cloned()
            .collect();
        self.retry_stray_kills(conn, &hosts, std::time::Instant::now())
            .await
    }

    /// An inventory reply is the satellite recovery signal: unreachable hosts
    /// stay grey; reached hosts are un-greyed and re-attached in place.
    pub(super) async fn replay_returned_satellites(
        &mut self,
        conn: &mut Connection,
        answers: &HostAnswers,
    ) -> Result<(), AttachError> {
        let mut changed = false;
        for host in &answers.unreachable {
            changed |= crate::attach::pane_state::mark_satellite_down(
                &mut self.mirror.panes,
                host.as_str(),
            );
        }
        let mut replay = Vec::new();
        for host in &answers.reachable {
            replay.extend(crate::attach::pane_state::satellite_panes_returned(
                &mut self.mirror.panes,
                host.as_str(),
            ));
        }
        if changed || !replay.is_empty() {
            self.peers.chrome_dirty = true;
        }
        self.attach_discovered_panes(conn, &replay).await
    }

    /// Arm [`SATELLITE_PROBE_INTERVAL`] while any satellite pane
    /// is down and no host inventory is already in flight.
    pub(super) fn arm_satellite_probe(&mut self, now: tokio::time::Instant) {
        if self.host_sessions_supported
            && self.peers.hosts_pending.is_none()
            && self.mirror.panes.values().any(|slot| slot.satellite_down)
        {
            self.satellite_probe_at
                .get_or_insert(now + SATELLITE_PROBE_INTERVAL);
        } else {
            self.satellite_probe_at = None;
        }
    }

    /// Kill the strays on `hosts` (known to answer at `answered_at`), skipping
    /// any this client now references.
    pub(super) async fn retry_stray_kills(
        &mut self,
        conn: &mut Connection,
        hosts: &[SatelliteHost],
        answered_at: std::time::Instant,
    ) -> Result<(), AttachError> {
        if hosts.is_empty() {
            return Ok(());
        }
        let due = self
            .orphan_kills
            .take_answered(hosts, answered_at, std::time::Instant::now());
        let strays = self.unreferenced_strays(due);
        for frame in self
            .orphan_kills
            .stray_kill_frames(strays, &mut self.next_request_id)
        {
            send_unless_peer_gone(conn, &frame).await?;
        }
        Ok(())
    }

    /// The strays nothing in this client references now; an adopted one is
    /// forgotten, not killed.
    pub(super) fn unreferenced_strays(
        &self,
        strays: Vec<super::super::orphans::Stray>,
    ) -> Vec<super::super::orphans::Stray> {
        strays
            .into_iter()
            .filter(|stray| {
                let pane = stray.pane();
                let adopted = crate::attach::server_frame::pane_is_referenced(
                    &self.mirror.workspace,
                    &self.mirror.pending_windows,
                    &self.mirror.pending_splits,
                    pane,
                );
                if adopted {
                    tracing::debug!(?pane, "a stray satellite pane was adopted; not killing it");
                }
                !adopted
            })
            .collect()
    }

    /// Take over the orphan record from an earlier entry on this connection,
    /// stamped with this entry's `CONDITIONAL_KILL` bit.
    pub(in super::super) fn set_orphan_kills(
        &mut self,
        mut kills: super::super::orphans::OrphanKills,
    ) {
        kills.set_conditional_kill(self.conditional_kill_supported);
        self.orphan_kills = kills;
    }

    /// Hand the orphan record to the next loop entry; windows and splits
    /// still opening become strays (no kill is sent on the way out).
    pub(super) fn orphans_for_switch(&mut self) -> super::super::orphans::OrphanKills {
        let mut kills = std::mem::take(&mut self.orphan_kills);
        kills.park_for_switch(
            parked_spawned_panes(&self.mirror.pending_windows, &self.mirror.pending_splits),
            unanswered_spawns(&self.mirror.pending_windows, &self.mirror.pending_splits),
            std::time::Instant::now(),
        );
        kills
    }

    /// Park a spawned satellite pane's window or split under `request_id`,
    /// in the map its kind's replies are looked up in.
    pub(super) fn park_adopt(&mut self, request_id: u32, adopt: ParkedAdopt) {
        match adopt {
            ParkedAdopt::Window(window) => {
                self.mirror.pending_windows.insert(request_id, window);
            }
            ParkedAdopt::Split(split) => {
                self.mirror.pending_splits.insert(request_id, split);
            }
        }
    }
}
