//! The relay's entire data path: two opaque byte pumps.
//!
//! Read, forward, nothing else: no frame decoding, so ADR-0051 invariants 1
//! and 5 hold by construction. Consumer bearer preambles cross as opaque
//! bytes.

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

/// Splice `a_recv -> b_send` and `b_recv -> a_send` concurrently until
/// EITHER direction finishes; that direction's writer is shut down
/// (propagating a FIN) and the other halves drop with the caller.
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

    /// Whichever side half-closes first, its last bytes and FIN reach the
    /// other side and the whole bridge ends.
    #[tokio::test]
    async fn half_close_from_either_side_propagates_and_ends_the_bridge() {
        for consumer_closes in [true, false] {
            let (mut consumer, a_side) = duplex(64);
            let (mut tunnel, b_side) = duplex(64);
            let (a_recv, a_send) = split(a_side);
            let (b_recv, b_send) = split(b_side);
            let bridge = tokio::spawn(splice(a_recv, b_send, b_recv, a_send));

            let (closer, reader) = if consumer_closes {
                (&mut consumer, &mut tunnel)
            } else {
                (&mut tunnel, &mut consumer)
            };
            closer.write_all(b"final").await.unwrap();
            closer.shutdown().await.unwrap();
            let mut received = Vec::new();
            reader.read_to_end(&mut received).await.unwrap();
            assert_eq!(received, b"final");
            bridge.await.unwrap();
        }
    }
}
