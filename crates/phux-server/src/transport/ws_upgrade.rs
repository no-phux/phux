//! Admitting one WebSocket peer: TLS, the HTTP upgrade, and typed accept errors.

use std::io;
use std::net::{IpAddr, SocketAddr};

use futures_util::StreamExt;
use phux_protocol::policy::{PeerIdentity, TransportType};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Error as WebSocketError;
use tokio_tungstenite::tungstenite::handshake::server::Request;

use phux_protocol::wire::framing::LENGTH_PREFIX_LEN as LENGTH_PREFIX;

#[cfg(doc)]
use super::WsListener;
use super::{
    Admitted, AllowedOrigins, HANDSHAKE_DEADLINE, ServerStream, Ws, WsAccepted, WsReader, WsWriter,
    admit_upgrade, anonymous_ws_upgrade, select_ws_protocol, tls_handshake, unauthorized_response,
};

/// TLS, then the upgrade and its token gate, each under
/// [`HANDSHAKE_DEADLINE`]; owned inputs so it runs beside other admissions.
pub(super) async fn ws_admit(
    tcp: TcpStream,
    peer: SocketAddr,
    tls: Option<tokio_rustls::TlsAcceptor>,
    tokens: Option<std::sync::Arc<crate::auth::ReloadingTokenStore>>,
    workload: Option<std::sync::Arc<crate::workload::ReloadingWorkloadRegistry>>,
    origins: std::sync::Arc<AllowedOrigins>,
) -> io::Result<WsAccepted> {
    // Only the source IP is retained; the ephemeral port identifies nothing.
    let source_ip = peer.ip();

    // Nagle would hold each short keystroke echo for the peer's delayed
    // ACK. Failure only costs latency.
    if let Err(err) = tcp.set_nodelay(true) {
        tracing::debug!(error = %err, "could not disable Nagle on accepted WebSocket TCP stream");
    }

    // TLS first, so the token in the upgrade request is encrypted.
    let (stream, peer_leaf) = match &tls {
        Some(acceptor) => tls_handshake(acceptor, tcp, source_ip).await?,
        None => (ServerStream::Left(tcp), None),
    };

    // With a token store, authenticate during the handshake and refuse
    // with HTTP 401 before any phux frame is read.
    let (ws, admitted) = if let Some(store) = &tokens {
        let store = store.clone();
        let workload = workload.clone();
        let captured: std::rc::Rc<std::cell::RefCell<Option<Admitted>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = captured.clone();
        #[allow(
            clippy::result_large_err,
            reason = "tokio-tungstenite fixes the HTTP rejection response type for its handshake callback"
        )]
        let callback = move |req: &Request, resp| {
            let workload = workload
                .as_deref()
                .map(|registry| (registry, peer_leaf.as_deref()));
            admit_upgrade(req, &store, workload).map_or_else(
                || Err(unauthorized_response()),
                |admitted| {
                    *sink.borrow_mut() = Some(admitted);
                    Ok(select_ws_protocol(req, resp))
                },
            )
        };
        let ws = ws_upgrade(stream, source_ip, callback).await?;
        let admitted = captured.borrow_mut().take();
        (ws, admitted)
    } else {
        #[allow(
            clippy::result_large_err,
            reason = "tokio-tungstenite fixes the HTTP rejection response type for its handshake callback"
        )]
        let callback = move |req: &Request, resp| anonymous_ws_upgrade(req, resp, &origins);
        (ws_upgrade(stream, source_ip, callback).await?, None)
    };
    // The connection's credential (the workload's under workload mTLS)
    // and the pairing-store bearer, kept so its revocation ends the
    // connection live.
    let (credential, bearer) = match admitted {
        Some((credential, bearer)) => (
            Some(credential),
            tokens.as_ref().map(|store| {
                crate::auth::BearerAdmission::new(std::sync::Arc::clone(store), &bearer)
            }),
        ),
        None => (None, None),
    };

    // The credential id rides `mcp_host_key` so policy and audit see a
    // non-anonymous peer.
    if let Some(credential) = credential.as_ref() {
        tracing::info!(
            transport = "ws",
            %source_ip,
            credential_id = %credential.id,
            "paired WebSocket consumer admitted"
        );
    }
    let peer_identity = PeerIdentity {
        uid: 0,
        pid: None,
        exe_path: None,
        mcp_host_key: credential.as_ref().map(|credential| credential.id.clone()),
        transport: TransportType::WebSocket,
        source_addr: Some(source_ip),
    };

    let (tx, rx) = ws.split();
    Ok((
        WsReader { rx },
        WsWriter { tx },
        crate::auth::ConnectionIdentity {
            peer: peer_identity,
            ssh_origin: None,
            credential,
            bearer,
        },
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WsAcceptStage {
    TlsHandshake,
    PairingAuthentication,
    Upgrade,
}

impl WsAcceptStage {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::TlsHandshake => "tls_handshake",
            Self::PairingAuthentication => "pairing_authentication",
            Self::Upgrade => "websocket_upgrade",
        }
    }
}

/// A typed, privacy-safe peer rejection created by [`WsListener`]. It never
/// carries the source error, which may hold header, URI, certificate, or
/// token material; the cause is logged at `debug` where it happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WsPeerRejection {
    pub(super) stage: WsAcceptStage,
    pub(super) source_ip: IpAddr,
}

impl std::fmt::Display for WsPeerRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let diagnosis = match self.stage {
            WsAcceptStage::TlsHandshake => "WebSocket TLS handshake failed",
            WsAcceptStage::PairingAuthentication => "WebSocket pairing authentication rejected",
            WsAcceptStage::Upgrade => "WebSocket upgrade failed",
        };
        write!(formatter, "{diagnosis} (source_ip={})", self.source_ip)
    }
}

impl std::error::Error for WsPeerRejection {}

pub(super) fn ws_accept_error(stage: WsAcceptStage, source_ip: IpAddr) -> io::Error {
    io::Error::other(WsPeerRejection { stage, source_ip })
}

/// Run the RFC 6455 upgrade under [`HANDSHAKE_DEADLINE`], mapping every
/// failure to a typed stage+IP rejection.
async fn ws_upgrade<C>(stream: ServerStream, source_ip: IpAddr, callback: C) -> io::Result<Ws>
where
    C: tokio_tungstenite::tungstenite::handshake::server::Callback + Unpin,
{
    tokio::time::timeout(
        HANDSHAKE_DEADLINE,
        tokio_tungstenite::accept_hdr_async_with_config(stream, callback, Some(ws_config())),
    )
    .await
    .map_err(|_| {
        tracing::debug!(%source_ip, "WebSocket upgrade timed out");
        ws_accept_error(WsAcceptStage::Upgrade, source_ip)
    })?
    .map_err(|error| classify_ws_upgrade_error(&error, source_ip))
}

/// One WebSocket message carries one frame, so nothing larger than a
/// maximum frame is ever buffered (tungstenite's default is 64 MiB).
fn ws_config() -> tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
    let max = LENGTH_PREFIX + phux_protocol::wire::frame::MAX_FRAME_LEN as usize;
    tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(max))
        .max_frame_size(Some(max))
}

/// A 401 from our callback is the pairing stage; every other failure is the
/// upgrade stage. The tungstenite cause stays at debug.
pub(super) fn classify_ws_upgrade_error(error: &WebSocketError, source_ip: IpAddr) -> io::Error {
    let stage = classify_ws_upgrade_stage(error);
    tracing::debug!(
        stage = stage.as_str(),
        %source_ip,
        error = %error,
        "WebSocket upgrade rejected"
    );
    ws_accept_error(stage, source_ip)
}

pub(super) fn classify_ws_upgrade_stage(error: &WebSocketError) -> WsAcceptStage {
    match error {
        WebSocketError::Http(response)
            if response.status()
                == tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED =>
        {
            WsAcceptStage::PairingAuthentication
        }
        _ => WsAcceptStage::Upgrade,
    }
}
