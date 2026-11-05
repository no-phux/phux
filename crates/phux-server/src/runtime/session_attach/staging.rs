//! Connection-wide bounded, unpublished resource captures.

use futures_util::future::LocalBoxFuture;
use phux_protocol::wire::frame::FrameKind;
use tokio::task::JoinSet;

use crate::resource::ResourceHandle;
use crate::state::AttachSnapshotPane;

/// One maximal native prefix retained across all kinds until publication.
pub(crate) const MAX_STAGED_BOOTSTRAP_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_STAGED_BOOTSTRAP_FRAMES: usize = 4_096 + 2;
/// Conservative resource-count preflight under the aggregate frame ceiling.
pub(crate) const MAX_AGGREGATE_BOOTSTRAP_PANES: usize = MAX_STAGED_BOOTSTRAP_FRAMES / 3;

#[derive(Debug)]
pub(crate) struct BootstrapStagingBudget {
    pub(crate) max_bytes: usize,
    pub(crate) max_frames: usize,
    pub(crate) staged_bytes: usize,
    pub(crate) staged_frames: usize,
}

impl BootstrapStagingBudget {
    pub(crate) const fn with_limits(max_bytes: usize, max_frames: usize) -> Self {
        Self {
            max_bytes,
            max_frames,
            staged_bytes: 0,
            staged_frames: 0,
        }
    }

    pub(crate) const fn remaining_bytes(&self) -> usize {
        self.max_bytes.saturating_sub(self.staged_bytes)
    }

    pub(crate) const fn remaining_frames(&self) -> usize {
        self.max_frames.saturating_sub(self.staged_frames)
    }

    pub(crate) fn append_accounted(
        &mut self,
        staged: &mut Vec<FrameKind>,
        incoming: &mut Vec<FrameKind>,
        incoming_bytes: usize,
    ) -> Result<(), ()> {
        let incoming_frames = incoming.len();
        let next_frames = self.staged_frames.checked_add(incoming_frames).ok_or(())?;
        let next_bytes = self.staged_bytes.checked_add(incoming_bytes).ok_or(())?;
        if next_frames > self.max_frames || next_bytes > self.max_bytes {
            return Err(());
        }
        staged.try_reserve(incoming_frames).map_err(|_| ())?;
        staged.append(incoming);
        self.staged_frames = next_frames;
        self.staged_bytes = next_bytes;
        Ok(())
    }
}

/// A kind's capture could close naturally or fail while still live.
#[derive(Debug)]
pub(crate) enum CaptureFailure {
    Closed(String),
    Failed(String),
}

impl From<String> for CaptureFailure {
    fn from(reason: String) -> Self {
        Self::Failed(reason)
    }
}

/// The host retains only frames, accounting and opaque parked continuations.
/// Native publication fences, Terminal state and document cursors stay in apps.
pub(crate) struct SessionStaging {
    pub(crate) budget: BootstrapStagingBudget,
    pub(crate) frames: Vec<FrameKind>,
    pub(crate) handles: Vec<ResourceHandle>,
    pub(crate) pumps: JoinSet<()>,
}

impl Default for SessionStaging {
    fn default() -> Self {
        Self {
            budget: BootstrapStagingBudget::with_limits(
                MAX_STAGED_BOOTSTRAP_BYTES,
                MAX_STAGED_BOOTSTRAP_FRAMES,
            ),
            frames: Vec::new(),
            handles: Vec::new(),
            pumps: JoinSet::new(),
        }
    }
}

impl SessionStaging {
    /// Collect lazily generated frames without letting a tiny negotiated chunk
    /// size allocate an unbounded frame vector. Apps preflight source/peak bytes.
    pub(crate) fn collect_frames(
        &self,
        frames: impl Iterator<Item = FrameKind>,
    ) -> Result<Vec<FrameKind>, CaptureFailure> {
        let mut bounded = Vec::new();
        for frame in frames {
            if bounded.len() == self.budget.remaining_frames() {
                return Err(CaptureFailure::Failed(
                    "aggregate bootstrap frame budget exceeded".to_owned(),
                ));
            }
            bounded
                .try_reserve(1)
                .map_err(|_| "host allocation failed".to_owned())?;
            bounded.push(frame);
        }
        Ok(bounded)
    }

    pub(crate) fn append(
        &mut self,
        mut frames: Vec<FrameKind>,
        retained_bytes: usize,
    ) -> Result<(), CaptureFailure> {
        self.budget
            .append_accounted(&mut self.frames, &mut frames, retained_bytes)
            .map_err(|()| {
                CaptureFailure::Failed("aggregate bootstrap staging budget exceeded".to_owned())
            })
    }

    /// Spawn behind the aggregate publication gate. Capturing the future in
    /// the task also makes abort-before-first-poll drop its completion guards.
    pub(crate) fn park(
        &mut self,
        capture: &super::SessionCapture<'_>,
        pane: &AttachSnapshotPane,
        live: LocalBoxFuture<'static, ()>,
    ) {
        crate::runtime::pump::spawn_tracked(
            capture.state,
            capture.client_id,
            pane.terminal_id,
            Some(&mut self.pumps),
            None,
            after_publication(
                capture.live_gate.clone(),
                capture.connection_token.clone(),
                live,
            ),
        );
    }

    /// Own an app's already-tracked pumps without registering their supervisor
    /// as a second output pump. Otherwise natural-close drain would abort the
    /// supervisor and its children before their final capture could finish.
    pub(crate) fn park_owner(
        &mut self,
        capture: &super::SessionCapture<'_>,
        live: LocalBoxFuture<'static, ()>,
    ) {
        self.pumps.spawn_local(after_publication(
            capture.live_gate.clone(),
            capture.connection_token.clone(),
            live,
        ));
    }
}

async fn after_publication(
    mut gate: tokio::sync::watch::Receiver<bool>,
    token: tokio_util::sync::CancellationToken,
    live: LocalBoxFuture<'static, ()>,
) {
    tokio::select! {
        () = token.cancelled() => return,
        ready = gate.wait_for(|ready| *ready) => { if ready.is_err() { return; } }
    }
    tokio::select! {
        () = token.cancelled() => {},
        () = live => {},
    }
}
