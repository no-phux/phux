//! From a planned [`Dial`] to an established [`Io`], once for every lane.
//!
//! The reconnecting driver plans its [`Target`] and establishes it here on
//! every attempt; a one-shot [`crate::session::Session`] establishes the
//! [`Dial`] it was handed. Failures stay typed ([`DialFailure`]) so each
//! words and classifies them in its own vocabulary.

#![allow(
    clippy::redundant_pub_crate,
    reason = "crate-internal: the session module establishes links; `unreachable_pub` forbids plain `pub`"
)]

use std::path::{Path, PathBuf};

use phux_dial::DialError;
use phux_dial::quic::QuicDial;
use phux_dial::ws::WsDial;

use super::io::Io;
use super::{ConnectOptions, ConnectionEnd, Target};
use crate::dial::{Dial, dial_message, timed_out};
use crate::reconnect::is_fatal_refusal;

/// Why a dial did not produce a link.
#[derive(Debug)]
pub(crate) enum DialFailure {
    /// The Unix socket at `path` did not accept.
    Uds {
        /// The socket dialed.
        path: PathBuf,
        /// What `connect(2)` said.
        error: std::io::Error,
    },
    /// The WSL stdio bridge could not be started.
    #[cfg(windows)]
    Wsl(std::io::Error),
    /// The remote dial failed (reachability, TLS, pin, or token).
    Remote(DialError),
    /// The QUIC connection closed as it was established, for this reason.
    Closed(DialError),
}

impl DialFailure {
    /// How the reconnecting driver treats this failure: a refusal no retry
    /// can satisfy ends the session; everything else walks the ladder.
    fn end(self, name: &str) -> ConnectionEnd {
        match self {
            Self::Uds { path, error } => ConnectionEnd::Dropped(Some(format!(
                "{name}: could not connect to {}: {error}",
                path.display()
            ))),
            #[cfg(windows)]
            Self::Wsl(error) => ConnectionEnd::Dropped(Some(format!(
                "{name}: could not start the WSL stdio bridge: {error}"
            ))),
            Self::Remote(error) if is_fatal_refusal(&error) => {
                ConnectionEnd::Refused(dial_message(name, &error))
            }
            Self::Closed(error @ DialError::AuthRefused(_)) => {
                ConnectionEnd::Refused(dial_message(name, &error))
            }
            Self::Remote(error) | Self::Closed(error) => {
                ConnectionEnd::Dropped(Some(dial_message(name, &error)))
            }
        }
    }
}

/// Establish the link `dial` plans. Unbounded in time: callers wrap it in
/// their own dial timeout.
pub(crate) async fn establish(dial: &Dial) -> Result<Io, DialFailure> {
    match dial {
        Dial::Uds(path) => establish_uds(path).await,
        #[cfg(windows)]
        Dial::Wsl { socket, phux } => super::wsl::establish(socket, phux).map_err(DialFailure::Wsl),
        Dial::Quic(plan) => establish_quic(plan).await,
        Dial::Ws(plan) => establish_ws(plan).await,
    }
}

#[cfg(unix)]
async fn establish_uds(path: &Path) -> Result<Io, DialFailure> {
    let stream = tokio::net::UnixStream::connect(path)
        .await
        .map_err(|error| DialFailure::Uds {
            path: path.to_path_buf(),
            error,
        })?;
    Ok(Io::from_unix_stream(stream))
}

#[cfg(not(unix))]
async fn establish_uds(path: &Path) -> Result<Io, DialFailure> {
    Err(DialFailure::Uds {
        path: path.to_path_buf(),
        error: std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "native Unix sockets are unavailable; use the explicit WSL transport for a Linux server",
        ),
    })
}

async fn establish_ws(plan: &WsDial) -> Result<Io, DialFailure> {
    // The plan names its TLS identity explicitly, so this never reads one
    // from the environment.
    let ws = phux_dial::ws::dial(plan)
        .await
        .map_err(DialFailure::Remote)?;
    Ok(Io::ws(ws))
}

async fn establish_quic(plan: &QuicDial) -> Result<Io, DialFailure> {
    let (endpoint, connection, stream) = phux_dial::quic::dial_stream(plan)
        .await
        .map_err(DialFailure::Remote)?;
    if let Some(reason) = connection.close_reason() {
        return Err(DialFailure::Closed(phux_dial::quic::close_error(&reason)));
    }
    Ok(Io::quic(endpoint, connection, stream))
}

/// The driver's dial for one attempt: plan `target` (a refused plan is
/// configuration, which no retry changes), then establish it, all within
/// `options.dial_timeout`. The plan, and the token it owns, is dropped as
/// soon as the link is up.
pub(super) async fn dial(target: &Target, options: ConnectOptions) -> Result<Io, ConnectionEnd> {
    let name = target.name.as_str();
    tokio::time::timeout(options.dial_timeout, async {
        let plan = target.plan().await.map_err(ConnectionEnd::Refused)?;
        establish(&plan).await.map_err(|failure| failure.end(name))
    })
    .await
    .map_err(|_| ConnectionEnd::Dropped(Some(timed_out(name))))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(unix))]
    #[tokio::test]
    async fn native_unix_sockets_remain_explicitly_unsupported() {
        let Err(DialFailure::Uds { error, .. }) = establish_uds(Path::new("/linux.sock")).await
        else {
            panic!("a native non-Unix client must not reinterpret a Unix socket");
        };
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    }

    #[tokio::test]
    async fn a_loopback_dial_names_a_missing_captured_token_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let token = dir.path().join("captured-token-file");
        let mut target = Target::ws("ws://127.0.0.1:1");
        target.name = "mini".to_owned();
        target.token_file = Some(token);
        let Err(err) = dial(&target, ConnectOptions::default()).await else {
            panic!("a missing token file must refuse the dial");
        };
        let text = match err {
            ConnectionEnd::Refused(text) | ConnectionEnd::Dropped(Some(text)) => text,
            other => panic!("unexpected end: {other:?}"),
        };
        assert!(
            text.contains("captured-token-file"),
            "dial must name the captured token file, got {text}"
        );
    }

    #[tokio::test]
    async fn a_missing_socket_walks_the_ladder_naming_the_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("absent.sock");
        let Err(end) = dial(&Target::uds(&socket), ConnectOptions::default()).await else {
            panic!("nothing listens there");
        };
        match end {
            ConnectionEnd::Dropped(Some(text)) => {
                assert!(text.contains("absent.sock"), "{text}");
            }
            other => panic!("a missing socket is retryable, got {other:?}"),
        }
    }

    #[test]
    fn only_a_fatal_remote_refusal_ends_the_session() {
        let refused = DialFailure::Remote(DialError::AuthRefused("unauthorized".to_owned()));
        assert!(matches!(refused.end("mini"), ConnectionEnd::Refused(_)));
        let unreachable = DialFailure::Remote(DialError::Unreachable("no route".to_owned()));
        assert!(matches!(
            unreachable.end("mini"),
            ConnectionEnd::Dropped(Some(_))
        ));
        // A close as the link came up ends the session only when it refused
        // the token; any other close reason stays retryable.
        let refused = DialFailure::Closed(DialError::AuthRefused("unauthorized".to_owned()));
        assert!(matches!(refused.end("mini"), ConnectionEnd::Refused(_)));
        let closed = DialFailure::Closed(DialError::Connect("closed by peer".to_owned()));
        assert!(matches!(
            closed.end("mini"),
            ConnectionEnd::Dropped(Some(_))
        ));
    }
}
