//! First paint, subscriptions, and the acknowledged-input journal.

use super::{
    AttachClaim, AttachEnd, AttachError, AttachMoment, AttachTarget, CONFIG_RELOAD_KEY, Connection,
    DEFAULT_GROUP_ID, FrameKind, FrameStep, LoopExit, Notice, RepaintAccumulator, SESSION_NAME_KEY,
    Scope, SidebarReservation, ToastOverlay, apply_initial_notice, attach_participants,
    detached_loop_exit, emit_bootstrap_workspace_reflow, finish_onboarding_claim, layout_key,
    send_attach, send_unless_peer_gone, undelivered_notices,
};

impl super::SessionLoop {
    // ---- bootstrap ----------------------------------------------------

    /// Replay the `ATTACHED` frame through `handle_server_frame` and set up
    /// the first paint. `Some(exit)` ⇒ the replayed frame ended the attach.
    pub(in super::super) async fn bootstrap<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        initial_attached: FrameKind,
        initial_notice: Option<Notice>,
    ) -> Result<Option<LoopExit>, AttachError> {
        if conn.multistream_enabled()
            && let FrameKind::Attached { snapshot, .. } = &initial_attached
        {
            for terminal_id in attach_participants(snapshot) {
                conn.bind_terminal(&terminal_id).await?;
            }
        }
        let moment = self
            .onboarding_claim
            .as_ref()
            .map_or(AttachMoment::None, AttachClaim::moment);
        // The sidebar reservation for this bootstrap frame (recomputed
        // per-iteration in the loop below to track `toggle-sidebar`).
        let sidebar = self.sidebar();
        // Single replayed frame — no burst to coalesce, paint it.
        let outcome = self.handle_frame(out, initial_attached, sidebar, false)?;
        if outcome.exit {
            let end = outcome
                .exit_reason
                .unwrap_or(AttachEnd::Detached { reason: None });
            return Ok(Some(detached_loop_exit(end, false)));
        }
        // Defer outbound writes to the recv-arm drain: a last-pane
        // RESOURCE_CLOSED may already be queued and must fold first.
        self.bootstrap_outbound = Some(outcome.subscribe_layout);
        self.vcs.apply_snapshot(outcome.pane_cwds);
        if let Some((list, focused)) = outcome.sessions {
            self.peers.sessions = list;
            self.peers.focused_session = Some(focused);
        }
        self.fold_inventory(outcome.inventory);
        self.resolve_cross_session_pick();
        // The peer sweep is deferred to the first drain (`sweep_pending`) so
        // the first paint never queues behind peer traffic.
        if outcome.own_client_id.is_some() {
            self.own_client_id = outcome.own_client_id;
        }
        // Seed the tab strip and sidebar from the bootstrap layout; peer zones
        // start empty and fill as sweep replies land.
        self.refresh_chrome();
        self.seed_initial_notice(initial_notice, moment);
        self.show_intro(out, sidebar, moment);
        Ok(None)
    }

    /// Size every workspace pane to its current chrome-inset layout. Used
    /// after bootstrap, late stream binding, and every outer viewport vote.
    pub(super) async fn size_workspace_panes(
        &self,
        conn: &mut Connection,
        sidebar: Option<SidebarReservation>,
    ) -> Result<(), AttachError> {
        emit_bootstrap_workspace_reflow(
            conn,
            &self.mirror.workspace,
            self.mirror.zoomed.as_ref(),
            self.content(sidebar),
            self.resize_cell_px,
        )
        .await?;
        self.size_floating_pane(conn, sidebar).await
    }

    /// ADR-0147: size the floating overlay's PTY to its box interior, which
    /// follows the content rect rather than any layout tile.
    pub(super) async fn size_floating_pane(
        &self,
        conn: &mut Connection,
        sidebar: Option<SidebarReservation>,
    ) -> Result<(), AttachError> {
        let Some(id) = crate::attach::floating::floating_pane(&self.mirror.panes) else {
            return Ok(());
        };
        let inner = crate::attach::floating::floating_box(self.content(sidebar)).inner;
        if inner.w == 0 || inner.h == 0 || !conn.can_route_terminal(id) {
            return Ok(());
        }
        send_unless_peer_gone(
            conn,
            &FrameKind::ResizeTerminal {
                terminal_id: id.clone(),
                cols: inner.w,
                rows: inner.h,
                cell_px: self.resize_cell_px,
            },
        )
        .await
    }

    /// Open every subscription this attach lives on: agent events, the
    /// config-reload doorbell, the persisted layout key, and each bootstrap
    /// pane's `phux.agent/v1` record.
    pub(super) async fn subscribe_bootstrap(
        &mut self,
        conn: &mut Connection,
        subscribe_layout: bool,
    ) -> Result<(), AttachError> {
        // Server-scoped (`terminal: None`) so we see control events for every
        // pane, not just one.
        conn.send(&FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: None,
        })
        .await?;
        // The `phux config reload` doorbell.
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: CONFIG_RELOAD_KEY.to_owned(),
        })
        .await?;
        // ADR-0105: follow the keep-empty mark, so a mark set or cleared after
        // attach still decides whether the last pane's close detaches.
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: phux_protocol::wire::frame::SESSION_KEEP_EMPTY_KEY.to_owned(),
        })
        .await?;
        // Session renames, so the roster and status name follow them.
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: SESSION_NAME_KEY.to_owned(),
        })
        .await?;
        if subscribe_layout && let Some(session) = self.peers.focused_session {
            // Fetch and watch this session's persisted layout (best effort).
            let key = layout_key(session);
            let req_id = self.take_request_id();
            self.layout_get_request_id = Some(req_id);
            self.layout_read_complete = false;
            conn.send(&FrameKind::GetMetadata {
                request_id: req_id,
                scope: Scope::Group(DEFAULT_GROUP_ID),
                key: key.clone(),
            })
            .await?;
            conn.send(&FrameKind::SubscribeMetadata {
                scope: Scope::Group(DEFAULT_GROUP_ID),
                key,
            })
            .await?;
        }
        // ADR-0040: read + watch every bootstrap pane's agent record.
        self.sync_agent_meta(conn).await?;
        self.adopt_input_replay(conn).await
    }

    /// Size the bootstrap PTYs and open the attach-lifetime subscriptions,
    /// from the recv-arm drain (see `bootstrap_outbound`).
    pub(in super::super) async fn emit_deferred_bootstrap_outbound(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        let Some(subscribe_layout) = self.bootstrap_outbound.take() else {
            return Ok(());
        };
        let sidebar = self.sidebar();
        self.size_workspace_panes(conn, sidebar).await?;
        self.subscribe_bootstrap(conn, subscribe_layout).await
    }

    /// Install the ADR-0053 replay journal the CLI's reconnect loop owns.
    pub(in super::super) fn set_input_replay(
        &mut self,
        journal: Option<
            std::rc::Rc<std::cell::RefCell<crate::attach::input_replay::InputReplayJournal>>,
        >,
    ) {
        self.input_replay = journal;
    }

    /// ADR-0053: adopt this connection into the acknowledged-input journal.
    /// Survivors are resent under their original operation ids; anything that
    /// cannot be replayed resolves as a status-bar notice.
    pub(super) async fn adopt_input_replay(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        let Some(journal) = self.input_replay.clone() else {
            return Ok(());
        };
        let mut reports = journal
            .borrow_mut()
            .begin_connection(conn.server_id(), self.acknowledged_input_supported);
        let (more, replay_frames) = journal.borrow_mut().next_frames(&mut self.next_request_id);
        reports.extend(more);
        self.show_notices(undelivered_notices(reports));
        crate::attach::input_dispatch::send_replay_frames(conn, journal.as_ref(), &replay_frames)
            .await
    }

    /// ADR-0053: the reply to one of the journal's `APPLY_INPUT` attempts;
    /// non-delivery raises a notice and the next queued operation is sent.
    pub(super) async fn resolve_input_replay(
        &mut self,
        conn: &mut Connection,
        request_id: u32,
        result: &phux_protocol::wire::frame::CommandResult,
        repaint: &mut RepaintAccumulator,
    ) -> Result<(), AttachError> {
        let Some(journal) = self.input_replay.clone() else {
            return Ok(());
        };
        let mut reports: Vec<_> = journal
            .borrow_mut()
            .resolve(request_id, result)
            .into_iter()
            .collect();
        let (more, next_frames) = journal.borrow_mut().next_frames(&mut self.next_request_id);
        reports.extend(more);
        if self.show_notices(undelivered_notices(reports)) {
            repaint.raise_chrome();
        }
        crate::attach::input_dispatch::send_replay_frames(conn, journal.as_ref(), &next_frames)
            .await
    }

    /// Seed the post-reconnect (or return-onboarding) notice now that the bar
    /// painter exists; the first bar paint shows it and the tick expires it.
    pub(super) fn seed_initial_notice(
        &mut self,
        initial_notice: Option<Notice>,
        moment: AttachMoment,
    ) {
        let return_notice_available = initial_notice.is_none() && moment == AttachMoment::Return;
        let initial_notice = initial_notice.or_else(|| {
            return_notice_available.then(|| Notice::info(crate::attach::onboarding::RETURN_NOTICE))
        });
        let notice_accepted =
            apply_initial_notice(self.settings.status_bar.as_mut(), initial_notice);
        if moment == AttachMoment::Return && (!return_notice_available || !notice_accepted) {
            self.onboarding_claim.take();
        }
    }

    /// The introduction toast: passthrough, so the first key dismisses it and
    /// still reaches the resolver/pane.
    pub(super) fn show_intro<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        moment: AttachMoment,
    ) {
        if moment != AttachMoment::Intro {
            return;
        }
        self.overlays.push(Box::new(ToastOverlay::passthrough(
            crate::attach::onboarding::ONBOARDING_TITLE,
            crate::attach::onboarding::hint_lines(
                self.settings.keybindings.as_ref(),
                self.sidebar_enabled,
            ),
            &self.settings.theme,
        )));
        // The intro commits when its paint reaches the sink, not when a
        // return notice is published, so it skips `finish_paint`.
        self.paint_overlay_layer(out, sidebar);
        let paint_accepted = out.flush().is_ok();
        finish_onboarding_claim(self.onboarding_claim.take(), paint_accepted);
    }

    /// The engine rejected a generation: re-ATTACH in-connection while the
    /// frozen replica stays visible.
    pub(super) async fn request_rebootstrap(
        &mut self,
        conn: &mut Connection,
    ) -> Result<FrameStep, AttachError> {
        if self.mirror.session_name.is_empty() {
            return Err(AttachError::Protocol(
                "engine requested rebootstrap before ATTACHED named the session".to_owned(),
            ));
        }
        conn.unbind_all_terminals();
        self.pending_stream_binds.clear();
        let attach_id =
            send_attach(conn, AttachTarget::ByName(self.mirror.session_name.clone())).await?;
        tracing::warn!(
            attach_id,
            session = %self.mirror.session_name,
            "engine generation rejected; requested replacement bootstrap"
        );
        Ok(FrameStep::Rebootstrap)
    }
}
