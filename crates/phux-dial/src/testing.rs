//! Test-only network impairment (the `testing` feature).
//!
//! [`DropProxy`] sits between a QUIC client and a server on loopback and can
//! stop delivering the server's datagrams. With nothing reaching the client,
//! no acknowledgement comes back and the server's congestion window stops
//! moving — the extreme of a link slower than the output, and the one case
//! where "how much will this writer buffer?" has a crisp answer. The send
//! window tests here, in `phux-relay` and in `phux-server` all use it.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

/// A loopback UDP forwarder between one client and one upstream server.
///
/// Datagrams from the upstream address go to the most recent client; every
/// other datagram goes upstream. Aborts its task on drop.
#[derive(Debug)]
pub struct DropProxy {
    addr: SocketAddr,
    drop_downstream: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl DropProxy {
    /// Bind a proxy on an ephemeral loopback port forwarding to `upstream`.
    ///
    /// # Errors
    ///
    /// Returns the bind error when no loopback UDP port is available.
    pub async fn start(upstream: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind("127.0.0.1:0").await?;
        let addr = socket.local_addr()?;
        let drop_downstream = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn(forward(socket, upstream, Arc::clone(&drop_downstream)));
        Ok(Self {
            addr,
            drop_downstream,
            task,
        })
    }

    /// The address clients dial instead of the upstream server.
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Silently drop every upstream-to-client datagram from now on.
    pub fn blackhole_downstream(&self) {
        self.drop_downstream.store(true, Ordering::SeqCst);
    }
}

impl Drop for DropProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The forwarding loop. Ends on the first socket error.
async fn forward(socket: UdpSocket, upstream: SocketAddr, drop_downstream: Arc<AtomicBool>) {
    let mut buf = vec![0_u8; 64 * 1024];
    let mut client = None;
    while let Ok((len, from)) = socket.recv_from(&mut buf).await {
        let target = if from == upstream {
            if drop_downstream.load(Ordering::SeqCst) {
                continue;
            }
            match client {
                Some(client) => client,
                None => continue,
            }
        } else {
            client = Some(from);
            upstream
        };
        if socket.send_to(&buf[..len], target).await.is_err() {
            return;
        }
    }
}
