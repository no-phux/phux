//! Real optional-receive APIs over Unix sockets; injected errors drive the
//! shared reader through every EOF, reset and partial-frame boundary.

use std::io::{self, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use phux_protocol::wire::frame::FrameKind;
use tokio::io::{AsyncRead, AsyncWriteExt, ReadBuf};
use tokio::net::UnixStream;
use tokio::time::Instant;

use super::read_framed_unbounded;
use crate::{WIRE_RECV_TIMEOUT, encode_frame_vec, try_recv_typed, try_recv_typed_before};

async fn receive(stream: &mut UnixStream, explicit_deadline: bool) -> Option<(u8, FrameKind)> {
    if explicit_deadline {
        try_recv_typed_before(stream, Instant::now() + WIRE_RECV_TIMEOUT)
            .await
            .unwrap()
    } else {
        try_recv_typed(stream).await
    }
}

fn frame() -> Vec<u8> {
    encode_frame_vec(&FrameKind::Ping { nonce: 7 })
}

#[tokio::test]
async fn optional_receives_decode_complete_frames_then_eof() {
    for explicit_deadline in [false, true] {
        let (mut reader, mut peer) = UnixStream::pair().unwrap();
        peer.write_all(&frame()).await.unwrap();
        drop(peer);
        assert!(matches!(
            receive(&mut reader, explicit_deadline).await,
            Some((_, FrameKind::Ping { nonce: 7 }))
        ));
        assert!(receive(&mut reader, explicit_deadline).await.is_none());
    }
}

#[tokio::test(start_paused = true)]
async fn optional_receives_keep_their_timeout_contracts() {
    let (mut reader, _peer) = UnixStream::pair().unwrap();
    assert!(
        try_recv_typed_before(&mut reader, Instant::now() + Duration::from_millis(10))
            .await
            .is_err()
    );
    assert!(
        tokio::spawn(async move { try_recv_typed(&mut reader).await })
            .await
            .unwrap_err()
            .is_panic()
    );
}

#[tokio::test(start_paused = true)]
async fn expired_deadline_rejects_even_a_ready_frame() {
    let (mut reader, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(&frame()).await.unwrap();
    assert!(
        try_recv_typed_before(&mut reader, Instant::now())
            .await
            .is_err()
    );
}

struct EndsWithError<'a> {
    remaining: &'a [u8],
    kind: ErrorKind,
}

impl AsyncRead for EndsWithError<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.remaining.is_empty() {
            return Poll::Ready(Err(self.kind.into()));
        }
        let n = self.remaining.len().min(buf.remaining());
        buf.put_slice(&self.remaining[..n]);
        self.remaining = &self.remaining[n..];
        Poll::Ready(Ok(()))
    }
}

async fn injected_read(
    bytes: Vec<u8>,
    kind: ErrorKind,
) -> Result<Option<Vec<u8>>, tokio::task::JoinError> {
    tokio::spawn(async move {
        read_framed_unbounded(&mut EndsWithError {
            remaining: &bytes,
            kind,
        })
        .await
    })
    .await
}

#[tokio::test]
async fn reader_accepts_only_boundary_eof_and_reset() {
    for kind in [ErrorKind::UnexpectedEof, ErrorKind::ConnectionReset] {
        assert_eq!(injected_read(vec![], kind).await.unwrap(), None);
        for n in 1..4 {
            assert!(
                injected_read(frame()[..n].to_vec(), kind)
                    .await
                    .unwrap_err()
                    .is_panic(),
                "{kind:?} after {n} header bytes"
            );
        }
        let bytes = frame();
        for n in [4, bytes.len() - 1] {
            assert!(
                injected_read(bytes[..n].to_vec(), kind)
                    .await
                    .unwrap_err()
                    .is_panic(),
                "{kind:?} after {n} frame bytes"
            );
        }
    }
}

#[tokio::test]
async fn reader_never_swallows_other_io_errors() {
    for n in [0, 1, 2, 3, 4, frame().len() - 1] {
        assert!(
            injected_read(frame()[..n].to_vec(), ErrorKind::PermissionDenied)
                .await
                .unwrap_err()
                .is_panic()
        );
    }
}
