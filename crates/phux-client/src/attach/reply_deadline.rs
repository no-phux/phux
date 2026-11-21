//! The deadline on a server exchange: one process-wide bound, installed by
//! the CLI from `PHUX_TIMEOUT`, on the HELLO wait and on each
//! request/response a [`super::connection::Connection`] makes.

use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use phux_client_runtime::session::{Dial, SessionError};

use super::outcome::AttachError;

// One bound for the handshake and every request/response exchange, set
// once by the CLI from `PHUX_TIMEOUT`. `None` (the library default) waits
// forever, as before.
static INSTALLED: Mutex<Option<Duration>> = Mutex::new(None);

/// Bound later handshakes and request/response exchanges by `deadline`.
///
/// Process-wide; `None` waits forever. A deadline that fires is an
/// [`AttachError::Io`] of kind `TimedOut` naming the server and the
/// duration (see [`AttachError::is_timeout`]).
pub fn install_deadline(deadline: Option<Duration>) {
    *INSTALLED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = deadline;
}

pub(super) fn installed() -> Option<Duration> {
    *INSTALLED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Who `dial` reaches, as a person names it.
pub(super) fn peer_label(dial: &Dial) -> String {
    match dial {
        Dial::Uds(path) => path.display().to_string(),
        #[cfg(windows)]
        Dial::Wsl { socket, .. } => socket.display().to_string(),
        Dial::Ssh { destination, .. } => destination.clone(),
        Dial::Quic(quic) => quic.addr.to_string(),
        Dial::Ws(ws) => ws.url.clone(),
    }
}

/// Await `exchange`, bounded by `deadline` when set; on expiry, a
/// `TimedOut` I/O error worded `"{what} timed out after {deadline}"`.
/// Callers pass the exchange boxed so the timer does not grow every
/// caller's future past the large-future lint.
pub(super) async fn bounded<T>(
    deadline: Option<Duration>,
    what: impl FnOnce() -> String,
    exchange: impl Future<Output = Result<T, SessionError>>,
) -> Result<T, AttachError> {
    let Some(deadline) = deadline else {
        return Ok(exchange.await?);
    };
    match tokio::time::timeout(deadline, exchange).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(AttachError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("{} timed out after {deadline:?}", what()),
        ))),
    }
}

/// Await one request's reply under the connection's reply deadline.
pub(super) async fn reply<T>(
    deadline: Option<Duration>,
    peer: &str,
    exchange: impl Future<Output = Result<T, SessionError>>,
) -> Result<T, AttachError> {
    bounded(
        deadline,
        || format!("waiting for the phux server at {peer} to answer"),
        exchange,
    )
    .await
}
