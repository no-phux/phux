//! Keep the replies a QUIC listener could not send, for `phux doctor`.
//!
//! quinn's own socket logs a failed `sendmsg` and carries on, so a host with
//! no return route to a peer (`EHOSTUNREACH`) looks healthy to every local
//! check: the listener is bound and a loopback probe answers. This is
//! quinn's Tokio socket with the last failure kept.
#![allow(
    clippy::redundant_pub_crate,
    reason = "private quic module; server state reads it through the quic re-export"
)]

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use quinn::udp::{RecvMeta, Transmit, UdpSocketState};
use quinn::{AsyncUdpSocket, UdpPoller};
use tokio::io::Interest;
use tracing::warn;

/// Failures older than this are not reported. A missing route fails every
/// retransmit and keep-alive, so a live one is always fresh.
const RECENT: Duration = Duration::from_secs(300);

/// The last reply this listener could not send, shared with server state.
#[derive(Debug, Default)]
pub(crate) struct ReplyFailures(Mutex<Option<Failure>>);

#[derive(Debug)]
struct Failure {
    at: Instant,
    peer: SocketAddr,
    error: String,
    count: u64,
}

impl ReplyFailures {
    fn record(&self, peer: SocketAddr, error: &io::Error) {
        let mut last = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let earlier = last
            .as_ref()
            .filter(|failure| failure.at.elapsed() < RECENT)
            .map_or(0, |failure| failure.count);
        if earlier == 0 {
            warn!(%peer, %error, "QUIC reply could not leave this host");
        }
        *last = Some(Failure {
            at: Instant::now(),
            peer,
            error: error.to_string(),
            count: earlier + 1,
        });
    }

    /// The recent failure, worded for the listener report.
    pub(crate) fn recent(&self) -> Option<String> {
        let last = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let failure = last
            .as_ref()
            .filter(|failure| failure.at.elapsed() < RECENT)?;
        let worded = format!(
            "{} sends failed, the last to {} {}s ago: {}",
            failure.count,
            failure.peer,
            failure.at.elapsed().as_secs(),
            failure.error
        );
        drop(last);
        Some(worded)
    }
}

/// A UDP socket for quinn that records send failures in [`ReplyFailures`].
#[derive(Debug)]
pub(super) struct RecordingSocket {
    io: tokio::net::UdpSocket,
    state: UdpSocketState,
    failures: Arc<ReplyFailures>,
}

impl RecordingSocket {
    /// Bind `addr` as `quinn::Endpoint::server` does.
    pub(super) fn bind(addr: SocketAddr, failures: Arc<ReplyFailures>) -> io::Result<Self> {
        let socket = std::net::UdpSocket::bind(addr)?;
        Ok(Self {
            state: UdpSocketState::new((&socket).into())?,
            io: tokio::net::UdpSocket::from_std(socket)?,
            failures,
        })
    }
}

impl AsyncUdpSocket for RecordingSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(Writable(self))
    }

    fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        self.io.try_io(Interest::WRITABLE, || {
            match self.state.try_send((&self.io).into(), transmit) {
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => Err(err),
                // An MTU probe too large for the path, as quinn expects.
                Err(err) if err.raw_os_error() == Some(libc::EMSGSIZE) => Ok(()),
                // UDP is lossy: quinn retransmits, so the send "succeeds".
                Err(err) => {
                    self.failures.record(transmit.destination, &err);
                    Ok(())
                }
                Ok(()) => Ok(()),
            }
        })
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [io::IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        loop {
            std::task::ready!(self.io.poll_recv_ready(cx))?;
            if let Ok(read) = self.io.try_io(Interest::READABLE, || {
                self.state.recv((&self.io).into(), bufs, meta)
            }) {
                return Poll::Ready(Ok(read));
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.local_addr()
    }

    fn may_fragment(&self) -> bool {
        self.state.may_fragment()
    }

    fn max_transmit_segments(&self) -> usize {
        self.state.max_gso_segments()
    }

    fn max_receive_segments(&self) -> usize {
        self.state.gro_segments()
    }
}

#[derive(Debug)]
struct Writable(Arc<RecordingSocket>);

impl UdpPoller for Writable {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.io.poll_send_ready(cx)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    /// A send the kernel refuses is recorded, not dropped: a loopback-bound
    /// socket has no route to a routable peer, like a host whose return
    /// route is missing.
    #[tokio::test]
    async fn a_reply_that_cannot_leave_the_host_is_recorded() {
        let failures = Arc::new(ReplyFailures::default());
        let socket = RecordingSocket::bind("127.0.0.1:0".parse().expect("addr"), failures.clone())
            .expect("bind");
        assert_eq!(failures.recent(), None, "nothing sent yet");
        socket.io.writable().await.expect("writable");
        let peer: SocketAddr = "192.0.2.1:9".parse().expect("addr");
        let transmit = Transmit {
            destination: peer,
            ecn: None,
            contents: b"reply",
            segment_size: None,
            src_ip: None,
        };
        socket
            .try_send(&transmit)
            .expect("UDP send errors are absorbed");
        let recent = failures.recent().expect("the refused send is recorded");
        assert!(
            recent.contains("1 sends failed") && recent.contains("192.0.2.1:9"),
            "{recent}"
        );
    }
}
