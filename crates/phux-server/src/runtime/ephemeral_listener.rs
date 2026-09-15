//! On-demand listeners: `OPEN_LISTENER` (`docs/spec/L1.md` §5.6, ADR-0120).
//!
//! `phux attach --ssh HOST` runs `phux bootstrap` on the far end, which asks
//! the server there for a door that exists for one attach: a QUIC listener on
//! the wildcard address that admits only a token minted for it and held in
//! memory. Everything after the door opens is the ordinary QUIC transport;
//! this module owns only opening the door and closing it again.
//!
//! **Lifetime.** A listener closes once its linger passes with no connection
//! open through it: before the first connection (the ssh round trip is over
//! and nobody came) or after the last one (the client's reconnect window,
//! which is shorter, has run out). Closing cancels the token its accept loop
//! runs under. No connection is live at that moment, so nothing is cut off.
//!
//! The listener is deliberately not a `RuntimeFlags` entry, so a graceful
//! upgrade (ADR-0032) drops it. A door opened for one attach must never
//! become a door the server re-opens on every restart.

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use phux_protocol::policy::TransportType;
use phux_protocol::wire::frame::{CommandResult, CommandValue, ErrorCode, ListenerTransport};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::input_lane::InputLaneHandle;
use crate::auth::{ConnectionIdentity, ListenerToken};
use crate::state::{ClientId, SharedState};
use crate::transport::quic::{QuicAdmission, QuicBindError, QuicListener};
use crate::transport::{AcceptErrorDisposition, FrameOrigin, FrameReader, Incoming};

/// The linger a request gets when it asks for the default (`linger_secs = 0`).
///
/// Longer than a remote client's 60s reconnect window, so a laptop that
/// roams or sleeps briefly finds its door still open.
const DEFAULT_LINGER: Duration = Duration::from_secs(120);

/// The longest linger a request may ask for. Larger requests are clamped, so
/// a door nobody uses cannot be held open indefinitely.
const MAX_LINGER: Duration = Duration::from_secs(3600);

/// The fields of one `OPEN_LISTENER` request.
#[derive(Debug, Clone, Copy)]
pub(super) struct OpenRequest {
    /// The transport asked for; only QUIC is defined.
    pub(super) transport: ListenerTransport,
    /// Inclusive port range, or `None` for any free port.
    pub(super) port_range: Option<(u16, u16)>,
    /// Requested linger in seconds; `0` means [`DEFAULT_LINGER`].
    pub(super) linger_secs: u32,
}

/// Workload mode (ADR-0116) covers this door too: the same client verifier
/// and live registry as the configured QUIC listener. A configured authority
/// that cannot load refuses the door rather than opening it bearer-only.
fn workload_authority() -> Result<Option<super::workload_auth::WorkloadAuth>, CommandResult> {
    super::workload_auth::WorkloadAuth::from_env().map_err(|err| {
        warn!(error = %err, "OPEN_LISTENER refused: configured workload mTLS is unavailable");
        refusal(
            ErrorCode::InternalError,
            "OPEN_LISTENER: workload mTLS is configured but its authority could not be loaded; see the server log"
                .to_owned(),
        )
    })
}

/// Handle `OPEN_LISTENER`: open a QUIC listener for one remote attach and
/// report how to reach it.
///
/// **Local only**, for the reason `SHUTDOWN` is: the command widens who can
/// reach the server, so only a peer the kernel has already authenticated as
/// the serving user may ask for it. A paired phone cannot open more doors.
pub(super) fn handle_open_listener(
    state: &SharedState,
    client_id: ClientId,
    request: OpenRequest,
    input_lane: Option<&InputLaneHandle>,
    root_token: &CancellationToken,
) -> CommandResult {
    if !from_local_socket(state, client_id) {
        warn!(?client_id, "OPEN_LISTENER refused: local socket only");
        return refusal(
            ErrorCode::PermissionDenied,
            "OPEN_LISTENER is accepted on the local socket only".to_owned(),
        );
    }
    if let Err(message) = validate(&request) {
        return refusal(ErrorCode::InvalidCommand, message);
    }
    let linger = effective_linger(request.linger_secs);

    let Some((cert, key)) = super::quic_certificate(SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)))
    else {
        return refusal(
            ErrorCode::InternalError,
            "OPEN_LISTENER: the TLS certificate could not be provisioned; see the server log"
                .to_owned(),
        );
    };
    let fingerprint = match crate::transport::tls::cert_fingerprint(&cert) {
        Ok(fingerprint) => fingerprint,
        Err(err) => {
            return refusal(
                ErrorCode::InternalError,
                format!("OPEN_LISTENER: could not read the certificate fingerprint: {err}"),
            );
        }
    };
    let (token, secret) = match ListenerToken::mint() {
        Ok(minted) => minted,
        Err(err) => {
            return refusal(
                ErrorCode::InternalError,
                format!("OPEN_LISTENER: could not mint a token: {err}"),
            );
        }
    };
    let token = Arc::new(token);
    let workload = match workload_authority() {
        Ok(workload) => workload,
        Err(refused) => return refused,
    };

    let listener = match bind_listener(request.port_range, |addr| {
        QuicListener::with_admission(
            addr,
            &cert,
            &key,
            QuicAdmission::Listener(Arc::clone(&token)),
            workload
                .as_ref()
                .map(|auth| (&auth.ca, Arc::clone(&auth.registry))),
        )
    }) {
        Ok(listener) => listener,
        Err(BindFailure::RangeExhausted { min, max }) => {
            return refusal(
                ErrorCode::ResourceExhausted,
                format!("OPEN_LISTENER: no free UDP port in {min}-{max}"),
            );
        }
        Err(BindFailure::Bind(err)) => {
            return refusal(
                ErrorCode::InternalError,
                format!("OPEN_LISTENER: could not bind QUIC: {err}"),
            );
        }
    };
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(err) => {
            return refusal(
                ErrorCode::InternalError,
                format!("OPEN_LISTENER: the bound port is unknown: {err}"),
            );
        }
    };

    info!(
        port,
        linger_secs = linger.as_secs(),
        credential = token.id(),
        "on-demand QUIC listener opened"
    );
    spawn_listener(
        listener,
        port,
        linger,
        state.clone(),
        root_token.child_token(),
        input_lane.cloned(),
    );

    let document = serde_json::json!({
        "schema_version": 1,
        "transport": "quic",
        "port": port,
        "cert_fingerprint": fingerprint,
        "token": hex::encode(secret),
        "credential_id": token.id(),
        "linger_secs": linger.as_secs(),
    });
    CommandResult::OkWith(CommandValue::Json(document.to_string()))
}

/// Whether `client_id` reached the server over its Unix socket.
fn from_local_socket(state: &SharedState, client_id: ClientId) -> bool {
    let transport = state.with(|s| s.peer_identity(client_id).map(|peer| peer.transport));
    matches!(transport, Some(TransportType::UnixSocket))
}

/// Refuse a request this build cannot honor, naming why.
fn validate(request: &OpenRequest) -> Result<(), String> {
    if request.transport != ListenerTransport::Quic {
        return Err(format!(
            "OPEN_LISTENER: transport {} is not defined; only QUIC (0) is",
            request.transport.to_u8()
        ));
    }
    if let Some((min, max)) = request.port_range
        && (min == 0 || min > max)
    {
        return Err(format!(
            "OPEN_LISTENER: port range {min}-{max} must be within 1-65535 with the lower bound first"
        ));
    }
    Ok(())
}

/// The linger a request actually gets: the default for `0`, otherwise the
/// request clamped to [`MAX_LINGER`].
fn effective_linger(linger_secs: u32) -> Duration {
    if linger_secs == 0 {
        return DEFAULT_LINGER;
    }
    Duration::from_secs(u64::from(linger_secs)).min(MAX_LINGER)
}

const fn refusal(code: ErrorCode, message: String) -> CommandResult {
    CommandResult::Error { code, message }
}

/// Why no listener could be bound.
#[derive(Debug)]
enum BindFailure {
    /// Every port in the requested range was taken.
    RangeExhausted { min: u16, max: u16 },
    /// Binding failed for a reason other than a busy port.
    Bind(QuicBindError),
}

/// Bind on the wildcard address: any free port, or one from `range` tried
/// from a random starting point so concurrent attaches spread out.
fn bind_listener<L>(
    range: Option<(u16, u16)>,
    bind: impl Fn(SocketAddr) -> Result<L, QuicBindError>,
) -> Result<L, BindFailure> {
    let Some((min, max)) = range else {
        return bind_wildcard(0, &bind).map_err(BindFailure::Bind);
    };
    let span = u32::from(max - min) + 1;
    let start = random_below(span);
    for step in 0..span {
        let offset = u16::try_from((start + step) % span).unwrap_or(0);
        if let Ok(listener) = bind_wildcard(min + offset, &bind) {
            return Ok(listener);
        }
    }
    Err(BindFailure::RangeExhausted { min, max })
}

/// Bind `[::]:port`, which serves IPv4 too on the dual-stack default of
/// Linux and macOS, falling back to `0.0.0.0:port` on a host without IPv6.
fn bind_wildcard<L>(
    port: u16,
    bind: &impl Fn(SocketAddr) -> Result<L, QuicBindError>,
) -> Result<L, QuicBindError> {
    bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)))
        .or_else(|_| bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))))
}

/// A uniform-enough offset in `0..span` for spreading port choices. Not a
/// secret, so a failed CSPRNG read degrades to `0` rather than refusing.
fn random_below(span: u32) -> u32 {
    let mut bytes = [0u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        return 0;
    }
    u32::from_ne_bytes(bytes) % span.max(1)
}

/// Serve `listener` until it has gone unused for `linger`, or the server
/// shuts down.
fn spawn_listener(
    listener: QuicListener,
    port: u16,
    linger: Duration,
    state: SharedState,
    token: CancellationToken,
    input_lane: Option<InputLaneHandle>,
) {
    let (live_tx, live_rx) = watch::channel(0usize);
    let tracked = Tracked {
        inner: listener,
        live: Arc::new(live_tx),
    };
    tokio::task::spawn_local(async move {
        let accept = async {
            let result =
                super::client::accept_loop(&tracked, state, token.clone(), input_lane).await;
            token.cancel();
            result
        };
        let close = async {
            let reason = close_when_idle(live_rx, linger, &token).await;
            token.cancel();
            reason
        };
        let (result, reason) = tokio::join!(accept, close);
        match result {
            Ok(()) => info!(port, ?reason, "on-demand QUIC listener closed"),
            Err(err) => warn!(port, error = %err, "on-demand QUIC listener failed"),
        }
    });
}

/// Why an on-demand listener closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseReason {
    /// Nobody used it for its whole linger.
    Idle,
    /// The server is shutting down, or the accept loop ended.
    Cancelled,
}

/// Resolve once `live` has read zero for `linger` without interruption, or
/// when `token` is cancelled.
///
/// The idle clock restarts whenever the count changes while it is zero,
/// which only happens when a last connection leaves, so the linger is always
/// measured from the moment the listener last became unused.
async fn close_when_idle(
    mut live: watch::Receiver<usize>,
    linger: Duration,
    token: &CancellationToken,
) -> CloseReason {
    loop {
        let idle = *live.borrow_and_update() == 0;
        tokio::select! {
            () = token.cancelled() => return CloseReason::Cancelled,
            () = tokio::time::sleep(linger), if idle => return CloseReason::Idle,
            changed = live.changed() => {
                if changed.is_err() {
                    return CloseReason::Cancelled;
                }
            }
        }
    }
}

/// An [`Incoming`] that counts the connections it has handed out and still
/// has open, so the listener knows when it has become unused.
struct Tracked<L> {
    inner: L,
    live: Arc<watch::Sender<usize>>,
}

impl<L: Incoming> Incoming for Tracked<L> {
    type Reader = TrackedReader<L::Reader>;
    type Writer = L::Writer;

    async fn accept(&self) -> io::Result<(Self::Reader, Self::Writer, ConnectionIdentity)> {
        let (reader, writer, identity) = self.inner.accept().await?;
        self.live.send_modify(|count| *count += 1);
        let reader = TrackedReader {
            inner: reader,
            _live: LiveGuard(Arc::clone(&self.live)),
        };
        Ok((reader, writer, identity))
    }

    fn accept_error_disposition(&self, error: &io::Error) -> AcceptErrorDisposition {
        self.inner.accept_error_disposition(error)
    }

    fn accept_errors_are_fatal(&self) -> bool {
        self.inner.accept_errors_are_fatal()
    }

    fn supports_quic_streams(&self) -> bool {
        self.inner.supports_quic_streams()
    }

    fn transport_type(&self) -> phux_protocol::policy::TransportType {
        self.inner.transport_type()
    }

    fn kind(&self) -> &'static str {
        self.inner.kind()
    }
}

/// Decrements the live count when the connection's reader is dropped, which
/// happens exactly once, when its client task ends.
struct LiveGuard(Arc<watch::Sender<usize>>);

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.send_modify(|count| *count = count.saturating_sub(1));
    }
}

/// A connection reader that holds its listener's [`LiveGuard`].
struct TrackedReader<R> {
    inner: R,
    _live: LiveGuard,
}

impl<R: FrameReader> FrameReader for TrackedReader<R> {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        self.inner.read_frame().await
    }

    fn frame_origin(&self) -> FrameOrigin {
        self.inner.frame_origin()
    }

    fn take_stream_events(
        &mut self,
    ) -> Option<tokio::sync::mpsc::Receiver<crate::transport::quic::QuicStreamEvent>> {
        self.inner.take_stream_events()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(transport: ListenerTransport, port_range: Option<(u16, u16)>) -> OpenRequest {
        OpenRequest {
            transport,
            port_range,
            linger_secs: 0,
        }
    }

    #[test]
    fn linger_defaults_and_clamps() {
        assert_eq!(effective_linger(0), DEFAULT_LINGER);
        assert_eq!(effective_linger(5), Duration::from_secs(5));
        assert_eq!(effective_linger(u32::MAX), MAX_LINGER);
    }

    #[test]
    fn only_quic_and_well_formed_ranges_validate() {
        assert!(validate(&request(ListenerTransport::Quic, None)).is_ok());
        assert!(validate(&request(ListenerTransport::Quic, Some((60000, 61000)))).is_ok());
        assert!(validate(&request(ListenerTransport::Quic, Some((7, 7)))).is_ok());

        let unknown = validate(&request(ListenerTransport::Unknown(3), None)).unwrap_err();
        assert!(unknown.contains("transport 3"), "{unknown}");
        let inverted = validate(&request(ListenerTransport::Quic, Some((9, 3)))).unwrap_err();
        assert!(inverted.contains("9-3"), "{inverted}");
        assert!(validate(&request(ListenerTransport::Quic, Some((0, 5)))).is_err());
    }

    #[test]
    fn a_range_binds_inside_it_and_reports_exhaustion() {
        let tried = std::cell::RefCell::new(Vec::new());
        let bound = bind_listener(Some((40000, 40009)), |addr| {
            tried.borrow_mut().push(addr.port());
            Ok(addr.port())
        })
        .expect("the first port binds");
        assert!((40000..=40009).contains(&bound));

        let exhausted = bind_listener::<u16>(Some((40000, 40003)), |_| {
            Err(QuicBindError::Io(io::Error::from(io::ErrorKind::AddrInUse)))
        })
        .unwrap_err();
        assert!(matches!(
            exhausted,
            BindFailure::RangeExhausted {
                min: 40000,
                max: 40003
            }
        ));
    }

    #[test]
    fn ipv6_failure_falls_back_to_ipv4() {
        let bound = bind_listener(None, |addr| {
            if addr.is_ipv6() {
                Err(QuicBindError::Io(io::Error::from(
                    io::ErrorKind::AddrNotAvailable,
                )))
            } else {
                Ok(addr)
            }
        })
        .expect("ipv4 fallback");
        assert_eq!(bound, SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)));
    }

    #[tokio::test(start_paused = true)]
    async fn an_unused_listener_closes_after_its_linger() {
        let (_tx, rx) = watch::channel(0usize);
        let token = CancellationToken::new();
        let started = tokio::time::Instant::now();
        let reason = close_when_idle(rx, Duration::from_secs(30), &token).await;
        assert_eq!(reason, CloseReason::Idle);
        assert_eq!(started.elapsed(), Duration::from_secs(30));
    }

    #[tokio::test(start_paused = true)]
    async fn a_live_connection_holds_it_open_and_the_clock_restarts_on_leave() {
        let (tx, rx) = watch::channel(0usize);
        let token = CancellationToken::new();
        let started = tokio::time::Instant::now();
        let driver = async {
            tokio::time::sleep(Duration::from_secs(20)).await;
            tx.send_modify(|count| *count += 1);
            // Held far past the linger: an attached client never times out.
            tokio::time::sleep(Duration::from_secs(300)).await;
            tx.send_modify(|count| *count -= 1);
        };
        let (reason, ()) =
            tokio::join!(close_when_idle(rx, Duration::from_secs(30), &token), driver);
        assert_eq!(reason, CloseReason::Idle);
        // 20s idle + 300s attached + a full 30s linger after the last leave.
        assert_eq!(started.elapsed(), Duration::from_secs(350));
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_closes_it_immediately() {
        let (_tx, rx) = watch::channel(1usize);
        let token = CancellationToken::new();
        token.cancel();
        let reason = close_when_idle(rx, Duration::from_secs(30), &token).await;
        assert_eq!(reason, CloseReason::Cancelled);
    }
}
