//! What the running server is listening on, as `phux pair` asks it before
//! minting (see [`super`]).

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

/// The remote listeners the running server reports bound: what a credential
/// minted now can actually authenticate against.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct LiveListeners {
    /// The bound wss listener's address.
    pub(super) wss: Option<String>,
    /// The bound QUIC listener's address.
    pub(super) quic: Option<String>,
}

impl LiveListeners {
    /// The wss bind as a socket address, when it parses as one.
    pub(super) fn wss_addr(&self) -> Option<SocketAddr> {
        self.wss.as_deref().and_then(|addr| addr.parse().ok())
    }

    /// The QUIC bind as a socket address, when it parses as one.
    pub(super) fn quic_addr(&self) -> Option<SocketAddr> {
        self.quic.as_deref().and_then(|addr| addr.parse().ok())
    }
}

/// Why asking the server which listeners are bound did not yield one.
#[derive(Debug)]
pub(super) enum ListenerQueryError {
    /// `GET_STATE` could not be asked. The string is the refusal to print.
    Unavailable(String),
    /// The server answered, and nothing is bound. `disabled` names each
    /// unhealthy row (`transport at addr disabled (reason)`).
    Unbound { disabled: Vec<String> },
}

/// Ask the server on `socket` which remote listeners it has bound.
pub(super) fn query_live_listeners(socket: &Path) -> Result<LiveListeners, ListenerQueryError> {
    let runtime = crate::commands::build_cli_runtime().map_err(|err| {
        ListenerQueryError::Unavailable(format!(
            "could not build a runtime to ask the server: {err}"
        ))
    })?;
    query_listeners_with(&runtime, socket, None)
}

/// [`query_live_listeners`] on a runtime the caller already holds. `timeout`
/// bounds one `GET_STATE` while waiting out a restart.
pub(super) fn query_listeners_with(
    runtime: &tokio::runtime::Runtime,
    socket: &Path,
    timeout: Option<Duration>,
) -> Result<LiveListeners, ListenerQueryError> {
    let asked = runtime.block_on(async {
        let pending = phux_client::state::get_state(socket);
        match timeout {
            Some(limit) => match tokio::time::timeout(limit, pending).await {
                Ok(result) => result,
                Err(_elapsed) => Err(phux_client::attach::AttachError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out asking the server which listeners are bound",
                ))),
            },
            None => pending.await,
        }
    });
    let view =
        asked.map_err(|err| ListenerQueryError::Unavailable(no_live_server(socket, &err)))?;
    live_listeners(view.snapshot().listeners())
}

/// The refusal when `GET_STATE` could not be asked at all.
pub(super) fn no_live_server(socket: &Path, err: &phux_client::attach::AttachError) -> String {
    let reason = match err {
        err if err.is_no_server() => {
            format!("no server is running at {}", socket.display())
        }
        other => format!(
            "could not ask the server at {} what it is listening on: {other}",
            socket.display()
        ),
    };
    format!(
        "{reason}; a credential minted now would pair with nothing, so none was minted.\n  \
         start one that stays up (`phux service install`, or `phux server --ensure`), \
         then rerun `phux pair`; pass --socket PATH to pair with a server on another socket"
    )
}

/// The pure half of [`query_live_listeners`]: the bound rows of `report`, or
/// the unbound rows that explain why nothing is bound.
pub(super) fn live_listeners(
    report: Option<&phux_protocol::wire::RemoteListenersReport>,
) -> Result<LiveListeners, ListenerQueryError> {
    use phux_protocol::wire::RemoteListenerTransport;

    let bound_addr = |transport| {
        report?
            .listeners
            .iter()
            .find(|slot| slot.transport == transport && slot.bound)
            .map(|slot| slot.addr.clone().unwrap_or_default())
    };
    let live = LiveListeners {
        wss: bound_addr(RemoteListenerTransport::Wss),
        quic: bound_addr(RemoteListenerTransport::Quic),
    };
    // A relay tunnel is not a listener a direct link can dial; it pairs
    // through `--relay-route` instead.
    let listener = |slot: &&phux_protocol::wire::RemoteListenerSlot| {
        slot.transport != RemoteListenerTransport::Relay
    };
    let any_bound = report.is_some_and(|report| {
        report
            .listeners
            .iter()
            .filter(listener)
            .any(|slot| slot.bound)
    });
    if any_bound {
        return Ok(live);
    }
    let disabled = report
        .map(|report| {
            report
                .unhealthy()
                .filter(listener)
                .map(|slot| {
                    format!(
                        "{} at {} {}",
                        slot.transport,
                        slot.addr.as_deref().unwrap_or("?"),
                        slot.unbound_label()
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Err(ListenerQueryError::Unbound { disabled })
}

#[cfg(test)]
mod tests {
    use phux_protocol::wire::{RemoteListenerSlot, RemoteListenersReport};

    use super::{ListenerQueryError, live_listeners};

    /// A relay tunnel is not a listener a direct link can dial: a server
    /// reachable only through its relay has nothing for plain `phux pair`.
    #[test]
    fn a_relay_tunnel_alone_is_no_direct_listener() {
        let report = RemoteListenersReport::new()
            .with_listeners(vec![RemoteListenerSlot::relay_up("relay.example:4433")]);
        assert!(matches!(
            live_listeners(Some(&report)),
            Err(ListenerQueryError::Unbound { ref disabled }) if disabled.is_empty()
        ));
    }
}
