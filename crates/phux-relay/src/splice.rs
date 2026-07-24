//! The relay's entire data path: two opaque byte pumps.
//!
//! Read, forward — nothing else. No `FrameKind::decode`, no length-prefix
//! awareness, no ack emission anywhere: ADR-0051 invariants 1 and 5 hold
//! by construction, not by discipline. Each consumer's own bearer-token
//! preamble crosses here as ordinary opaque bytes (ADR-0051 Decision 4).

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

/// Splice two byte streams: `a_recv -> b_send` and `b_recv -> a_send`,
/// concurrently, until EITHER direction finishes (EOF or error) — the
/// stdio-bridge shape. The finished direction's writer is shut down
/// (propagating the half-close as a FIN); the other direction's halves
/// are dropped when the caller's connection state unwinds.
///
/// Generic over `AsyncRead`/`AsyncWrite` so the pump is unit-testable
/// with in-memory duplex pipes; production passes quinn stream halves.
pub(crate) async fn splice<AR, AW, BR, BW>(
    mut a_recv: AR,
    mut b_send: BW,
    mut b_recv: BR,
    mut a_send: AW,
) where
    AR: AsyncRead + Unpin,
    AW: AsyncWrite + Unpin,
    BR: AsyncRead + Unpin,
    BW: AsyncWrite + Unpin,
{
    let a_to_b = async {
        let _ = tokio::io::copy(&mut a_recv, &mut b_send).await;
        let _ = b_send.shutdown().await;
    };
    let b_to_a = async {
        let _ = tokio::io::copy(&mut b_recv, &mut a_send).await;
        let _ = a_send.shutdown().await;
    };
    tokio::select! {
        () = a_to_b => {}
        () = b_to_a => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, duplex, split};

    #[tokio::test]
    async fn bytes_flow_in_both_directions() {
        // consumer end <-> (a side | splice | b side) <-> tunnel end
        let (mut consumer, a_side) = duplex(64);
        let (mut tunnel, b_side) = duplex(64);
        let (a_recv, a_send) = split(a_side);
        let (b_recv, b_send) = split(b_side);
        let bridge = tokio::spawn(splice(a_recv, b_send, b_recv, a_send));

        consumer.write_all(b"to-tunnel").await.unwrap();
        let mut buf = [0u8; 9];
        tunnel.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"to-tunnel");

        tunnel.write_all(b"to-consumer").await.unwrap();
        let mut buf = [0u8; 11];
        consumer.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"to-consumer");

        drop(consumer);
        bridge.await.unwrap();
    }

    #[tokio::test]
    async fn half_close_propagates_as_eof_and_ends_the_bridge() {
        let (mut consumer, a_side) = duplex(64);
        let (mut tunnel, b_side) = duplex(64);
        let (a_recv, a_send) = split(a_side);
        let (b_recv, b_send) = split(b_side);
        let bridge = tokio::spawn(splice(a_recv, b_send, b_recv, a_send));

        // Consumer writes its last bytes and half-closes its write side.
        consumer.write_all(b"final").await.unwrap();
        consumer.shutdown().await.unwrap();

        // The bytes arrive, then the FIN: the far side reads to EOF.
        let mut received = Vec::new();
        tunnel.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, b"final");

        // First-finished direction ends the whole bridge.
        bridge.await.unwrap();
    }

    #[tokio::test]
    async fn tunnel_side_eof_also_ends_the_bridge() {
        let (mut consumer, a_side) = duplex(64);
        let (mut tunnel, b_side) = duplex(64);
        let (a_recv, a_send) = split(a_side);
        let (b_recv, b_send) = split(b_side);
        let bridge = tokio::spawn(splice(a_recv, b_send, b_recv, a_send));

        tunnel.write_all(b"bye").await.unwrap();
        tunnel.shutdown().await.unwrap();

        let mut received = Vec::new();
        consumer.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, b"bye");
        bridge.await.unwrap();
    }
}
