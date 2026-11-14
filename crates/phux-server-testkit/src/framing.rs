//! Strict frame reads shared by the testkit's bounded receive APIs.

use std::io::ErrorKind::{ConnectionReset, UnexpectedEof};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Read with no deadline of our own, so the caller's is the only one.
/// Panics on I/O error or a truncated frame.
#[allow(
    clippy::redundant_pub_crate,
    reason = "private reader shared only with the parent receive APIs"
)]
pub(super) async fn read_framed_unbounded(
    stream: &mut (impl AsyncRead + Unpin),
) -> Option<Vec<u8>> {
    let mut header = [0u8; 4];
    match stream.read_exact(&mut header[..1]).await {
        Ok(_) => {}
        // Linux resets a socket the server exits with unread. This is
        // EOF only before the first byte, never after a partial header.
        Err(e) if matches!(e.kind(), UnexpectedEof | ConnectionReset) => return None,
        Err(e) => panic!("recv header io error: {e}"),
    }
    stream
        .read_exact(&mut header[1..])
        .await
        .expect("recv header io error or truncated header");
    let body_len = u32::from_be_bytes(header) as usize;
    let mut framed = vec![0u8; 4 + body_len];
    framed[..4].copy_from_slice(&header);
    stream.read_exact(&mut framed[4..]).await.unwrap();
    Some(framed)
}

#[cfg(test)]
mod tests;
