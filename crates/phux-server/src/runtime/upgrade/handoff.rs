//! Collecting each pane's PTY handoff for the upgrade cut (ADR-0032).

use std::collections::HashMap;
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt as _};
use tokio::sync::{mpsc, oneshot};

use crate::resource::handoff::{PaneUpgradeHandle, UpgradeHandleRequest};

use super::UpgradeError;

/// Ask one pane's actor for its handoff. Only live tree Terminals are asked
/// (see `ServerState::upgrade_handles`), so a closed upgrade mailbox means
/// a pane whose engine already exited and whose exit watcher has not reaped
/// it yet: [`UpgradeError::PaneExiting`], which a retry clears. Carrying it
/// would resurrect a killed pane as a blank one that never exits. Any
/// failure after the request was accepted (a dropped reply, a half PTY pair,
/// the deadline) means a live actor could not hand off.
pub(super) async fn request_pane_handoff(
    pane: phux_core::ids::ResourceId,
    upgrade: &mpsc::Sender<UpgradeHandleRequest>,
) -> Result<PaneUpgradeHandle, UpgradeError> {
    let (reply, rx) = oneshot::channel();
    if upgrade.send(UpgradeHandleRequest { reply }).await.is_err() {
        tracing::warn!(
            ?pane,
            "upgrade: pane engine already exited and is awaiting its reap; aborting for retry"
        );
        return Err(UpgradeError::PaneExiting { pane });
    }
    rx.await.map_err(|_| UpgradeError::PaneHandoff {
        pane,
        reason: "actor dropped its reply",
    })
}

pub(super) async fn collect_pane_handoffs(
    handles: Vec<(
        phux_core::ids::ResourceId,
        mpsc::Sender<UpgradeHandleRequest>,
    )>,
    deadline: Duration,
) -> Result<HashMap<phux_core::ids::ResourceId, PaneUpgradeHandle>, UpgradeError> {
    tokio::time::timeout(deadline, async move {
        let pane_count = handles.len();
        let mut pending = handles
            .into_iter()
            .map(|(pane, sender)| async move {
                request_pane_handoff(pane, &sender)
                    .await
                    .map(|handoff| (pane, handoff))
            })
            .collect::<FuturesUnordered<_>>();
        let mut handoffs = HashMap::with_capacity(pane_count);
        while let Some(result) = pending.next().await {
            let (pane, handoff) = result?;
            let pair_is_valid = matches!(
                (&handoff.master_fd, handoff.child_pid),
                (Some(_), Some(1..)) | (None, None)
            );
            if !pair_is_valid {
                return Err(UpgradeError::InvalidPaneHandoff { pane });
            }
            handoffs.insert(pane, handoff);
        }
        Ok(handoffs)
    })
    .await
    .map_err(|_| UpgradeError::HandoffDeadline)?
}
