//! Coalesce installer measurements onto the UI thread. The owning task is
//! cancelled when staging finishes, so old measurements cannot reach a retry.

use super::{Updater, Work};
use crate::updates::StageProgress;
use gpui::Context;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

impl Updater {
    pub(super) fn watch_progress(
        &mut self,
        receiver: Receiver<StageProgress>,
        cx: &mut Context<Self>,
    ) {
        self._progress = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let (latest, disconnected) = drain_progress(&receiver);
                if let Some(progress) = latest {
                    let _ = this.update(cx, |updater, cx| {
                        if updater.busy && updater.work == Work::Downloading {
                            updater.progress = Some(progress);
                            cx.notify();
                        }
                    });
                }
                if disconnected {
                    break;
                }
            }
        }));
    }
}

pub(super) fn drain_progress(receiver: &Receiver<StageProgress>) -> (Option<StageProgress>, bool) {
    let mut latest = None;
    loop {
        match receiver.try_recv() {
            Ok(progress) => latest = Some(progress),
            Err(TryRecvError::Empty) => return (latest, false),
            Err(TryRecvError::Disconnected) => return (latest, true),
        }
    }
}
