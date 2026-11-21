//! Rebind the UDS path when its socket file disappears under a live server.
//!
//! A listener keeps accepting on its inode after the path is unlinked (a tmp
//! cleaner, an `rm`, a pre-lock server's stale reap), so every pane becomes
//! unreachable by path. While the path stays absent, bind a fresh listener
//! there and serve it beside the original, which keeps its connected clients.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use tokio::net::UnixListener;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::accept_loop;
use super::socket::{SocketLock, prepare_socket_dir, secure_socket_file, socket_identity};
use crate::runtime::ServerError;
use crate::runtime::apps::InputLane;
use crate::state::SharedState;
use crate::transport::UdsListener;

/// How often the path is checked: a `stat`, never a connect, so the server
/// does not audit a connection to itself.
const CHECK_EVERY: Duration = Duration::from_secs(2);

/// The bound socket's `(device, inode)`, shared with shutdown so it unlinks
/// the listener the path names now.
pub(crate) type BoundSocket = Rc<Cell<Option<(u64, u64)>>>;

/// What the guardian needs from `serve`.
pub(crate) struct SocketGuard {
    pub(crate) socket_path: PathBuf,
    pub(crate) bound: BoundSocket,
    pub(crate) state: SharedState,
    pub(crate) root_token: CancellationToken,
    pub(crate) input_lane: Option<InputLane>,
}

impl SocketGuard {
    /// Watch the path until shutdown, serving every listener it rebinds.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: accept loops run on the server's LocalSet"
    )]
    pub(crate) async fn run(self) -> Result<(), ServerError> {
        let mut healed: JoinSet<Result<(), ServerError>> = JoinSet::new();
        // A lock file deleted with its directory is re-taken beside the new
        // socket, so a later cold start still sees this server as live.
        let mut relock: Option<SocketLock> = None;
        let mut ticker = tokio::time::interval(CHECK_EVERY);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = self.root_token.cancelled() => break,
                _ = ticker.tick() => {}
            }
            if socket_identity(&self.socket_path).is_some() {
                // Ours, or an entry someone else put there: never replace it.
                continue;
            }
            if !phux_config::socket::server_lock_path(&self.socket_path).exists() {
                relock = SocketLock::acquire(&self.socket_path).ok().or(relock);
            }
            match rebind(&self.socket_path) {
                Ok(listener) => self.serve_healed(listener, &mut healed),
                Err(err) => warn!(
                    path = %self.socket_path.display(),
                    error = %err,
                    "socket path is gone and could not be rebound",
                ),
            }
        }
        let mut result = Ok(());
        while let Some(joined) = healed.join_next().await {
            if let Ok(Err(err)) = joined
                && result.is_ok()
            {
                result = Err(err);
            }
        }
        drop(relock);
        result
    }

    fn serve_healed(&self, listener: UdsListener, healed: &mut JoinSet<Result<(), ServerError>>) {
        warn!(
            path = %self.socket_path.display(),
            "socket path was deleted under the live server; rebound it",
        );
        self.bound.set(socket_identity(&self.socket_path));
        // An upgrade hands over the listener the path reaches.
        let fd = listener.as_raw_fd();
        self.state
            .with_mut(|s| s.lifecycle_mut().set_upgrade_listener_fd(fd));
        let (state, token, lane) = (
            self.state.clone(),
            self.root_token.clone(),
            self.input_lane.clone(),
        );
        healed.spawn_local(async move { accept_loop(&listener, state, token, lane).await });
    }
}

fn rebind(path: &Path) -> Result<UdsListener, ServerError> {
    prepare_socket_dir(path)?;
    let listener = UnixListener::bind(path).map_err(ServerError::Bind)?;
    secure_socket_file(path)?;
    info!(path = %path.display(), "phux-server listening on UDS (rebound)");
    Ok(UdsListener::new(listener))
}
