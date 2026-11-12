//! After a cold start: say once that panes a server restart ended can come
//! back, for a server that does not restore on its own.

use std::sync::atomic::{AtomicBool, Ordering};

use super::Dial;
use super::jobs::{Job, JobDone, JobKind, Toast};

/// Whether this process has already looked. Once is enough: the hint is for
/// the first attach after a restart, not every session switch.
static LOOKED: AtomicBool = AtomicBool::new(false);
/// `[restore] on-start`: the server brings panes back itself, so say nothing.
static SERVER_RESTORES: AtomicBool = AtomicBool::new(false);

/// Set `[restore] on-start`.
pub(in crate::attach) fn set_server_restores(on: bool) {
    SERVER_RESTORES.store(on, Ordering::Relaxed);
}

/// The job that counts what `phux restore` would relaunch and toasts the
/// offer, or `None` when this process already looked, the server restores on
/// its own, or the session is on another machine.
// ponytail: reads the records over a second connection so the attach stream
// is left alone; share the attach connection's read if that ever costs.
pub(in crate::attach) fn job(dial: Option<&Dial>) -> Option<Job> {
    let Some(Dial::Uds(socket)) = dial else {
        return None;
    };
    if SERVER_RESTORES.load(Ordering::Relaxed) || LOOKED.swap(true, Ordering::Relaxed) {
        return None;
    }
    let socket = socket.clone();
    Some(Job::new(JobKind::PluginAction, async move {
        let waiting = phux_client::org::fetch_from_socket(&socket)
            .await
            .ok()
            .flatten()
            .map_or(0, |inventory| inventory.restorable(None).len());
        offer(waiting).map_or_else(JobDone::quiet, JobDone::toast)
    }))
}

/// The toast for `waiting` panes that can come back; none for zero.
fn offer(waiting: usize) -> Option<Toast> {
    let panes = match waiting {
        0 => return None,
        1 => "1 pane".to_owned(),
        n => format!("{n} panes"),
    };
    Some(Toast::lines(
        "Restore sessions",
        vec![
            format!("{panes} ended when the server restarted."),
            "Run Restore sessions from the command palette to relaunch what ended.".to_owned(),
        ],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offer_counts_panes_and_is_silent_for_none() {
        assert_eq!(offer(0), None);
        let lines = |n| offer(n).map(|toast| toast.lines[0].clone());
        assert_eq!(
            lines(1).as_deref(),
            Some("1 pane ended when the server restarted.")
        );
        assert_eq!(
            lines(3).as_deref(),
            Some("3 panes ended when the server restarted.")
        );
    }
}
