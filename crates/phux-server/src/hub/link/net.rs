//! The network link transport and its connection handles.

use super::{AbortOnDrop, Arc, FramingError, Mutex, NegotiatedBootstrap, SatelliteHost, info};

/// The production [`LinkTransport`]: `phux-dial` QUIC/WS plus SSH-stdio.
#[derive(Debug, Clone)]
pub(crate) struct NetLinkTransport {
    /// Program for SSH dials: `ssh`, or `$PHUX_SSH`.
    pub(super) ssh_program: std::ffi::OsString,
    /// Where `inbound://` satellites' connections arrive (ADR-0182).
    pub(super) adopter: crate::hub::adopt::SatelliteAdopter,
}

impl NetLinkTransport {
    /// Build the production transport dialing SSH satellites with
    /// `ssh_program` (`ssh`, or `$PHUX_SSH` via the server's
    /// [`crate::runtime::ServerEnv`]) and adopting inbound ones from
    /// `adopter`.
    pub(crate) const fn new(
        ssh_program: std::ffi::OsString,
        adopter: crate::hub::adopt::SatelliteAdopter,
    ) -> Self {
        Self {
            ssh_program,
            adopter,
        }
    }
}

/// An established production link.
#[derive(Debug)]
pub(crate) enum NetLinkConn {
    /// QUIC connection plus bidi stream, length-prefixed like UDS.
    Quic {
        /// Owns the UDP socket + I/O driver; must outlive the connection.
        _endpoint: quinn::Endpoint,
        /// The established connection, kept for a clean close.
        _connection: quinn::Connection,
        /// Opened bidi send half (auth preamble already written).
        send: quinn::SendStream,
        /// Opened bidi recv half.
        recv: quinn::RecvStream,
        /// Cancel-safe reassembly buffer.
        buf: bytes::BytesMut,
        /// Exact selection installed once before `connect` returns.
        negotiated: Option<NegotiatedBootstrap>,
    },
    /// WebSocket connection: one binary message is one complete frame.
    Ws {
        /// The established stream.
        ws: Box<phux_dial::ws::Ws>,
        /// Last inbound activity, feeding the WS idle limit.
        last_inbound: std::time::Instant,
        /// Exact selection installed once before `connect` returns.
        negotiated: Option<NegotiatedBootstrap>,
    },
    /// SSH-stdio child whose stdin/stdout carry the UDS framing verbatim;
    /// `kill_on_drop` reaps it on teardown.
    Ssh {
        /// The spawned ssh process; its exit is the link-loss signal.
        child: tokio::process::Child,
        /// Bridged write half — frames land on the remote server's UDS.
        stdin: tokio::process::ChildStdin,
        /// Bridged read half — the satellite's frames, length-prefixed.
        stdout: tokio::process::ChildStdout,
        /// Continuous stderr drainer. An unread fd 2 would fill its pipe and
        /// stall stdout on the shared SSH channel; keeps a bounded tail for
        /// the loss reason.
        stderr_reader: AbortOnDrop<Vec<u8>>,
        /// Cancel-safe reassembly buffer.
        buf: bytes::BytesMut,
        /// Exact selection installed once before `connect` returns.
        negotiated: Option<NegotiatedBootstrap>,
    },
}

pub(crate) enum NetLinkReader {
    Quic {
        _endpoint: quinn::Endpoint,
        recv: quinn::RecvStream,
        buf: bytes::BytesMut,
        framing_violation: Option<FramingError>,
    },
    Ws {
        stream: futures_util::stream::SplitStream<phux_dial::ws::Ws>,
        last_inbound: Arc<Mutex<std::time::Instant>>,
        framing_violation: Option<FramingError>,
    },
    Ssh {
        child: tokio::process::Child,
        stdout: tokio::process::ChildStdout,
        stderr_reader: AbortOnDrop<Vec<u8>>,
        buf: bytes::BytesMut,
        framing_violation: Option<FramingError>,
    },
}

pub(crate) enum NetLinkWriter {
    Quic {
        _connection: quinn::Connection,
        send: quinn::SendStream,
    },
    Ws {
        sink: futures_util::stream::SplitSink<
            phux_dial::ws::Ws,
            tokio_tungstenite::tungstenite::Message,
        >,
        last_inbound: Arc<Mutex<std::time::Instant>>,
    },
    Ssh {
        stdin: tokio::process::ChildStdin,
    },
}

impl NetLinkConn {
    pub(super) fn install_negotiated(
        &mut self,
        selection: NegotiatedBootstrap,
    ) -> Result<(), String> {
        let slot = match self {
            Self::Quic { negotiated, .. }
            | Self::Ws { negotiated, .. }
            | Self::Ssh { negotiated, .. } => negotiated,
        };
        if slot.replace(selection).is_some() {
            return Err("satellite HELLO negotiation completed twice".to_owned());
        }
        Ok(())
    }

    pub(super) fn negotiated(&self) -> Result<NegotiatedBootstrap, String> {
        match self {
            Self::Quic { negotiated, .. }
            | Self::Ws { negotiated, .. }
            | Self::Ssh { negotiated, .. } => negotiated.ok_or_else(|| {
                "satellite connection returned before HELLO negotiation completed".to_owned()
            }),
        }
    }
}

impl NetLinkTransport {
    /// Take `host`'s next inbound connection and open the link's stream on
    /// it: the hub is the phux client, so it opens the stream and sends HELLO
    /// first (ADR-0182).
    pub(super) async fn adopt_inbound(&self, host: &SatelliteHost) -> Result<NetLinkConn, String> {
        let adopted = self
            .adopter
            .next(host)
            .await
            .ok_or_else(|| "the inbound satellite is no longer accepted".to_owned())?;
        info!(
            satellite = %host,
            credential = %adopted.credential,
            "adopting the satellite's inbound connection as its link"
        );
        let (send, recv) = adopted
            .connection
            .open_bi()
            .await
            .map_err(|err| format!("open a stream to the inbound satellite: {err}"))?;
        Ok(NetLinkConn::Quic {
            _endpoint: adopted.endpoint,
            _connection: adopted.connection,
            send,
            recv,
            buf: bytes::BytesMut::with_capacity(8192),
            negotiated: None,
        })
    }
}
