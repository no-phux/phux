//! Recording a composited headless capture.

use super::{
    ATTACH_READY_DEADLINE, AttachError, AttachTarget, Connection, Duration, FrameKind,
    GhosttyAdapter, HeadlessCompletion, HeadlessSession, HistoryCacheConfig, Instant, Path,
    SessionId, SessionKernel, attach_client_caps, attach_client_name, headless_chrome,
    request_history_page, request_layout, restart_attach, send_attach_without_size_vote,
    wait_for_attached,
};

/// What `phux rec --composited` asks for: where the cast goes, the virtual
/// cell grid, the cast format, and an optional time limit.
#[derive(Debug, Clone, Copy)]
pub struct CompositedCapture<'a> {
    /// The `.cast` file the composite is written to.
    pub cast_path: &'a Path,
    /// Virtual viewport width in cells.
    pub cols: u16,
    /// Virtual viewport height in cells.
    pub rows: u16,
    /// The asciicast version written.
    pub version: phux_record::cast::CastVersion,
    /// Stop after this long; `None` records until Ctrl-C or the server leaves.
    pub max_duration: Option<Duration>,
}

/// Headless composited capture (`phux rec --composited`).
///
/// Attaches with a zero viewport, the same non-voting attach as
/// [`crate::attach::run_headless_rendered`], and composites into `capture.cast_path` at
/// the caller's cell grid. The grid is a virtual viewport owned by
/// [`crate::attach::record::SessionRecorder::create_virtual`]: the size
/// probe never reads the host terminal, and this function never sends
/// `VIEWPORT_RESIZE` or a pane resize. Real panes keep the size they had.
///
/// Stops on Ctrl-C, `capture.max_duration`, or the server going away.
/// Whatever was captured is left as a playable cast.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn record_composited(
    socket: &Path,
    target: AttachTarget,
    capture: CompositedCapture<'_>,
    mut progress: impl FnMut(Duration, u64),
) -> Result<(), AttachError> {
    let client_caps = attach_client_caps(None, &crate::attach::Dial::uds(socket));
    let mut conn =
        Connection::connect_with_hello(socket, attach_client_name(), client_caps).await?;
    let negotiated = conn.negotiated_bootstrap().ok_or_else(|| {
        AttachError::Protocol("composited recording lacks negotiated bootstrap".to_owned())
    })?;
    let history_config = HistoryCacheConfig {
        request_max_bytes: negotiated.limits.max_history_page_bytes(),
        ..HistoryCacheConfig::default()
    };
    let engine_kernel = SessionKernel::with_history_config(
        GhosttyAdapter::new(negotiated.limits),
        negotiated.profile,
        history_config,
    );
    // Zero viewport: the server resizes nothing (L1 §9). The cell grid
    // below is local to the compositor.
    let mut attach_id = send_attach_without_size_vote(&mut conn, target).await?;
    let attached = match wait_for_attached(&mut conn, attach_id).await {
        Ok(frame) => frame,
        Err(err) => {
            conn.shutdown().await;
            return Err(err);
        }
    };

    let viewport_dims = (capture.cols.max(1), capture.rows.max(1));
    let mut session =
        HeadlessSession::new(engine_kernel, headless_chrome(viewport_dims), viewport_dims);
    let outcome = session.ingest(attached, None, None)?;
    session.vcs.apply_snapshot(outcome.pane_cwds);
    let focused_session = outcome.sessions.map(|(_, focused)| focused);
    let title = session.mirror.session_name.clone();
    let mut recorder = match crate::attach::record::SessionRecorder::create_virtual(
        capture.cast_path,
        viewport_dims.0,
        viewport_dims.1,
        Some(title.as_str()).filter(|name| !name.is_empty()),
        capture.version,
    ) {
        Ok(recorder) => recorder,
        Err(err) => {
            let _ = conn.send(&FrameKind::Detach).await;
            conn.shutdown().await;
            return Err(err);
        }
    };

    if let Err(err) = session.request_agent_records(&mut conn).await {
        return end_composited(conn, &mut recorder, Err(err)).await;
    }
    let layout_get_request_id = match request_layout(
        &mut conn,
        socket,
        &mut session,
        outcome.subscribe_layout,
        focused_session,
    )
    .await
    {
        Ok(id) => id,
        Err(err) => return end_composited(conn, &mut recorder, Err(err)).await,
    };
    let mut completion = HeadlessCompletion::new(layout_get_request_id);

    let started = Instant::now();
    let stop_at = capture
        .max_duration
        .map(|limit| tokio::time::Instant::now() + limit);
    let settle_until = {
        let cap = tokio::time::Instant::now() + ATTACH_READY_DEADLINE;
        stop_at.map_or(cap, |stop| cap.min(stop))
    };
    let followed = {
        let mut link = RecordingLink {
            conn: &mut conn,
            session: &mut session,
            completion: &mut completion,
            attach_id: &mut attach_id,
            focused_session,
            layout_get_request_id,
        };
        match link.settle(settle_until).await {
            Ok(()) => {
                link.follow(&mut recorder, started, stop_at, &mut progress)
                    .await
            }
            Err(err) => Err(err),
        }
    };
    end_composited(conn, &mut recorder, followed).await
}

/// The live attach a composited capture reads from: the connection, the
/// headless compositor, its readiness barrier, and the attach identity a
/// resync replaces.
pub(super) struct RecordingLink<'a> {
    conn: &'a mut Connection,
    session: &'a mut HeadlessSession,
    completion: &'a mut HeadlessCompletion,
    attach_id: &'a mut u32,
    focused_session: Option<SessionId>,
    layout_get_request_id: Option<u32>,
}

impl RecordingLink<'_> {
    /// Ingest until the composite is ready, the settle cap, or the server
    /// drops. A timeout starts the recording on the frame already in hand.
    #[allow(
        clippy::future_not_send,
        reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
    )]
    async fn settle(&mut self, settle_until: tokio::time::Instant) -> Result<(), AttachError> {
        while !self
            .completion
            .is_complete(self.session.agent_metadata_complete())
            && tokio::time::Instant::now() < settle_until
        {
            let remaining = settle_until.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, self.conn.recv()).await {
                Ok(Ok(frame)) => self.absorb(frame).await?,
                Err(_) | Ok(Err(_)) => break,
            }
        }
        Ok(())
    }

    /// Record the composite until Ctrl-C, `stop_at`, or the server drops.
    /// A protocol error is returned so the caller can still close the cast.
    #[allow(
        clippy::future_not_send,
        reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
    )]
    async fn follow(
        &mut self,
        recorder: &mut crate::attach::record::SessionRecorder,
        started: Instant,
        stop_at: Option<tokio::time::Instant>,
        progress: &mut impl FnMut(Duration, u64),
    ) -> Result<(), AttachError> {
        let mut last_vt = Vec::new();
        capture_composite(self.session, recorder, &mut last_vt);
        progress(started.elapsed(), recorder.events_written());

        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let interrupt = tokio::signal::ctrl_c();
        tokio::pin!(interrupt);
        loop {
            if stop_at.is_some_and(|at| tokio::time::Instant::now() >= at) {
                break;
            }
            tokio::select! {
                biased;
                _ = &mut interrupt => break,
                _ = tick.tick() => {
                    recorder.poll_marks();
                    progress(started.elapsed(), recorder.events_written());
                }
                frame = self.conn.recv() => {
                    match frame {
                        Ok(frame) => {
                            self.absorb(frame).await?;
                            capture_composite(self.session, recorder, &mut last_vt);
                        }
                        Err(_) => break,
                    }
                }
            }
        }
        Ok(())
    }

    /// Fold one server frame into the composite. Re-attach, when the engine
    /// asks, still votes no size.
    #[allow(
        clippy::future_not_send,
        reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
    )]
    async fn absorb(&mut self, frame: FrameKind) -> Result<(), AttachError> {
        self.completion.observe_frame(&frame, *self.attach_id);
        let outcome =
            self.session
                .ingest(frame, self.focused_session, self.layout_get_request_id)?;
        if outcome.resync_required {
            *self.attach_id = restart_attach(
                self.conn,
                &self.session.mirror.session_name,
                self.completion,
            )
            .await?;
            return Ok(());
        }
        if let Some(request) = outcome.history_request {
            request_history_page(self.conn, self.completion, request).await?;
        }
        Ok(())
    }
}

/// Record a full redraw when the composite changed, and pick up markers.
pub(super) fn capture_composite(
    session: &mut HeadlessSession,
    recorder: &mut crate::attach::record::SessionRecorder,
    last_vt: &mut Vec<u8>,
) {
    let vt = crate::attach::frame_vt::frame_to_vt(&session.compose());
    if vt != *last_vt {
        recorder.record(&vt);
        *last_vt = vt;
    }
    recorder.poll_marks();
}

/// Detach without a size vote, close the cast, and shut the connection.
#[allow(
    clippy::future_not_send,
    reason = "SessionRecorder's size probe is !Send; the capture stays on the current-thread runtime"
)]
pub(super) async fn end_composited(
    mut conn: Connection,
    recorder: &mut crate::attach::record::SessionRecorder,
    result: Result<(), AttachError>,
) -> Result<(), AttachError> {
    recorder.poll_marks();
    let finished = recorder.finish_in_place();
    let _ = conn.send(&FrameKind::Detach).await;
    conn.shutdown().await;
    finished.and(result)
}
