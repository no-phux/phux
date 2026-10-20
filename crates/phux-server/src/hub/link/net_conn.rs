//! Dialing the network transport and the connection's writer half.

use super::{
    AbortOnDrop, Arc, BootstrapLimits, BootstrapProfile, DialSpec, LinkConn, LinkTransport,
    LinkWriter, Mutex, NetLinkConn, NetLinkReader, NetLinkTransport, NetLinkWriter, QuicDial,
    SSH_STDERR_TAIL_MAX, SatelliteHost, ServerFeatureExtSet, WsDial, negotiate_link, read_tail,
    ssh_argv, ws_idle_error,
};

impl LinkTransport for NetLinkTransport {
    type Conn = NetLinkConn;

    async fn connect(
        &self,
        host: &SatelliteHost,
        spec: &DialSpec,
        token: Option<String>,
    ) -> Result<NetLinkConn, String> {
        let result: Result<NetLinkConn, String> = match spec {
            DialSpec::Inbound => self.adopt_inbound(host).await,
            DialSpec::Quic {
                host, port, trust, ..
            } => {
                let bare = host.trim_matches(['[', ']']);
                let addr = tokio::net::lookup_host((bare, *port))
                    .await
                    .map_err(|err| format!("resolve {host}:{port}: {err}"))?
                    .next()
                    .ok_or_else(|| format!("resolve {host}:{port}: no addresses"))?;
                let token = token
                    .as_deref()
                    .map(phux_dial::quic::parse_token_hex)
                    .transpose()
                    .map_err(|err| err.to_string())?;
                let dial = QuicDial {
                    addr,
                    server_name: bare.to_owned(),
                    token,
                    trust: trust.clone(),
                    identity: None,
                    inner: None,
                };
                let (endpoint, connection, send, recv) = phux_dial::quic::dial(&dial)
                    .await
                    .map_err(|err| err.to_string())?;
                Ok(NetLinkConn::Quic {
                    _endpoint: endpoint,
                    _connection: connection,
                    send,
                    recv,
                    buf: bytes::BytesMut::with_capacity(8192),
                    negotiated: None,
                })
            }
            DialSpec::Ws { url, trust, .. } => {
                let dial = WsDial {
                    url: url.clone(),
                    token,
                    trust: trust.clone(),
                    tls_server_name: None,
                    identity: None,
                };
                let ws = phux_dial::ws::dial(&dial)
                    .await
                    .map_err(|err| err.to_string())?;
                Ok(NetLinkConn::Ws {
                    ws: Box::new(ws),
                    last_inbound: std::time::Instant::now(),
                    negotiated: None,
                })
            }
            DialSpec::Ssh { user, host, port } => {
                // SSH authenticates the channel; there is no token.
                debug_assert!(token.is_none(), "ssh dials carry no bearer token");
                let mut child = tokio::process::Command::new(&self.ssh_program)
                    .args(ssh_argv(user.as_deref(), host, *port))
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|err| {
                        format!("spawn {}: {err}", self.ssh_program.to_string_lossy())
                    })?;
                // Missing pipes are a failed dial, not a panic.
                let stdin = child.stdin.take().ok_or("ssh child has no stdin pipe")?;
                let stdout = child.stdout.take().ok_or("ssh child has no stdout pipe")?;
                let mut stderr = child.stderr.take().ok_or("ssh child has no stderr pipe")?;
                // Drain fd 2 for the link's life (see `stderr_reader`).
                let stderr_reader = AbortOnDrop(tokio::task::spawn_local(async move {
                    read_tail(&mut stderr, SSH_STDERR_TAIL_MAX).await
                }));
                Ok(NetLinkConn::Ssh {
                    child,
                    stdin,
                    stdout,
                    stderr_reader,
                    buf: bytes::BytesMut::with_capacity(8192),
                    negotiated: None,
                })
            }
        };
        let mut conn = result?;

        // HELLO/profile negotiation is mandatory on every transport,
        // including the SSH bridge.
        let selection = negotiate_link(&mut conn).await?;
        conn.install_negotiated(selection)?;
        Ok(conn)
    }
}

impl LinkConn for NetLinkConn {
    type Reader = NetLinkReader;
    type Writer = NetLinkWriter;

    fn into_parts(self) -> (Self::Reader, Self::Writer) {
        match self {
            Self::Quic {
                _endpoint: endpoint,
                _connection: connection,
                send,
                recv,
                buf,
                ..
            } => (
                NetLinkReader::Quic {
                    _endpoint: endpoint,
                    recv,
                    buf,
                    framing_violation: None,
                },
                NetLinkWriter::Quic {
                    _connection: connection,
                    send,
                },
            ),
            Self::Ws {
                ws, last_inbound, ..
            } => {
                let (sink, stream) = futures_util::StreamExt::split(*ws);
                let last_inbound = Arc::new(Mutex::new(last_inbound));
                (
                    NetLinkReader::Ws {
                        stream,
                        last_inbound: Arc::clone(&last_inbound),
                        framing_violation: None,
                    },
                    NetLinkWriter::Ws { sink, last_inbound },
                )
            }
            Self::Ssh {
                child,
                stdin,
                stdout,
                stderr_reader,
                buf,
                ..
            } => (
                NetLinkReader::Ssh {
                    child,
                    stdout,
                    stderr_reader,
                    buf,
                    framing_violation: None,
                },
                NetLinkWriter::Ssh { stdin },
            ),
        }
    }

    fn bootstrap_limits(&self) -> Result<BootstrapLimits, String> {
        self.negotiated().map(|selection| selection.limits)
    }

    fn bootstrap_profile(&self) -> Result<BootstrapProfile, String> {
        self.negotiated().map(|selection| selection.profile)
    }

    fn server_features(&self) -> Result<phux_protocol::caps::ServerFeatureSet, String> {
        self.negotiated().map(|selection| selection.server_features)
    }

    fn server_features_ext(&self) -> Result<ServerFeatureExtSet, String> {
        self.negotiated()
            .map(|selection| selection.server_features_ext)
    }

    fn satellite_incarnation(&self) -> Option<[u8; 16]> {
        self.negotiated()
            .ok()
            .and_then(|selection| selection.server_id)
    }
}

impl LinkWriter for NetLinkConn {
    async fn send_frame(&mut self, frame: &[u8]) -> Result<(), String> {
        match self {
            Self::Quic { send, .. } => send
                .write_all(frame)
                .await
                .map_err(|err| format!("write to satellite: {err}")),
            Self::Ws { ws, .. } => futures_util::SinkExt::send(
                ws.as_mut(),
                tokio_tungstenite::tungstenite::Message::Binary(frame.to_vec().into()),
            )
            .await
            .map_err(|err| format!("write to satellite: {err}")),
            // The child's stdin is the wire, bounded like any other.
            Self::Ssh { stdin, .. } => tokio::io::AsyncWriteExt::write_all(stdin, frame)
                .await
                .map_err(|err| format!("write to ssh transport: {err}")),
        }
    }

    async fn keepalive(&mut self) -> Result<(), String> {
        match self {
            Self::Quic { .. } | Self::Ssh { .. } => Ok(()),
            Self::Ws {
                ws, last_inbound, ..
            } => {
                if let Some(reason) = ws_idle_error(last_inbound.elapsed()) {
                    return Err(reason);
                }
                futures_util::SinkExt::send(
                    ws.as_mut(),
                    tokio_tungstenite::tungstenite::Message::Ping(Vec::new().into()),
                )
                .await
                .map_err(|err| format!("keepalive ping to satellite: {err}"))
            }
        }
    }
}
