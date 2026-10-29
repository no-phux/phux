//! A duplex alone cannot model Tokio stdout's blocking-worker buffer.
#![allow(clippy::expect_used, reason = "tests")]

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::AsyncWrite;

struct BufferedOutput {
    fail_flush: bool,
}

impl AsyncWrite for BufferedOutput {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.fail_flush {
            Poll::Ready(Err(io::Error::other("buffered write failed")))
        } else {
            Poll::Pending
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

#[tokio::test]
async fn buffered_stdout_stalls_before_the_next_idle_read() {
    use tokio::io::AsyncWriteExt as _;
    let (mut input, mut sender) = tokio::io::duplex(32);
    sender.write_all(b"last server bytes").await.expect("input");
    // Keep sender open and idle: waiting for EOF would leak the bridge.
    let mut output = BufferedOutput { fail_flush: false };
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        super::copy_with_write_deadline(&mut input, &mut output, Duration::from_millis(10)),
    )
    .await
    .expect("flush deadline, not an unlimited read")
    .expect_err("blocked flush");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}

#[tokio::test]
async fn eof_does_not_hide_a_buffered_flush_error() {
    let mut input = &b""[..];
    let mut output = BufferedOutput { fail_flush: true };
    let error = super::copy_with_write_deadline(&mut input, &mut output, Duration::from_millis(10))
        .await
        .expect_err("EOF must flush");
    assert_eq!(error.kind(), io::ErrorKind::Other);
}

#[tokio::test]
async fn eof_does_not_hide_a_buffered_flush_timeout() {
    let mut input = &b""[..];
    let mut output = BufferedOutput { fail_flush: false };
    let error = super::copy_with_write_deadline(&mut input, &mut output, Duration::from_millis(10))
        .await
        .expect_err("EOF must flush within its deadline");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
}
