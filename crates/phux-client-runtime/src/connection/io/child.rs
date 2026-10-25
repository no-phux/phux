//! Ownership and bounded diagnostics for a byte-transparent stdio bridge.

use std::io;
#[cfg(any(windows, test))]
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

#[cfg(any(windows, test))]
use tokio::io::AsyncReadExt as _;
use tokio::process::Child;
#[cfg(any(windows, test))]
use tokio::process::{ChildStdin, ChildStdout, Command};

#[cfg(any(windows, test))]
const STDERR_TAIL: usize = 8 * 1024;
const EXIT_WAIT: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(crate) struct ChildLink {
    child: Child,
    stderr: Option<tokio::task::JoinHandle<()>>,
    tail: Arc<Mutex<Vec<u8>>>,
}

impl ChildLink {
    #[cfg(any(windows, test))]
    pub(super) fn spawn(command: &mut Command) -> io::Result<(Self, ChildStdout, ChildStdin)> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let missing = || io::Error::other("stdio bridge is missing a pipe");
        let reader = child.stdout.take().ok_or_else(missing)?;
        let writer = child.stdin.take().ok_or_else(missing)?;
        let stderr = child.stderr.take().ok_or_else(missing)?;
        let tail = Arc::new(Mutex::new(Vec::new()));
        let stderr = tokio::spawn(drain(stderr, Arc::clone(&tail)));
        Ok((
            Self {
                child,
                stderr: Some(stderr),
                tail,
            },
            reader,
            writer,
        ))
    }

    pub(super) async fn closed(&mut self) -> io::Error {
        let status = tokio::time::timeout(EXIT_WAIT, self.child.wait()).await;
        // A child may close stdout before it exits. Neither exit nor an
        // inherited stderr pipe may hold teardown or reconnection hostage.
        if let Some(stderr) = self.stderr.as_mut() {
            let _ = tokio::time::timeout(EXIT_WAIT, stderr).await;
        }
        if let Some(stderr) = self.stderr.take() {
            stderr.abort();
        }
        let detail = {
            let tail = self.tail.lock().unwrap_or_else(PoisonError::into_inner);
            String::from_utf8_lossy(&tail).trim().to_owned()
        };
        let status = match status {
            Ok(Ok(status)) => status.to_string(),
            Ok(Err(error)) => error.to_string(),
            Err(_) => "stdout closed before the bridge exited".to_owned(),
        };
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("stdio bridge closed ({status}): {detail}"),
        )
    }
}

impl Drop for ChildLink {
    fn drop(&mut self) {
        if let Some(stderr) = &self.stderr {
            stderr.abort();
        }
        // `Child` was spawned with kill_on_drop: only this link's child is killed.
    }
}

#[cfg(any(windows, test))]
async fn drain(mut stderr: tokio::process::ChildStderr, tail: Arc<Mutex<Vec<u8>>>) {
    let mut chunk = [0; 4096];
    while let Ok(count) = stderr.read(&mut chunk).await {
        if count == 0 {
            return;
        }
        let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
        tail.extend_from_slice(&chunk[..count]);
        let excess = tail.len().saturating_sub(STDERR_TAIL);
        tail.drain(..excess);
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt as _;

    #[tokio::test]
    async fn child_stdio_is_full_duplex_and_drop_kills_only_the_owned_child() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec cat"]);
        let (owner, mut reader, mut writer) = ChildLink::spawn(&mut command).expect("spawn cat");
        writer.write_all(b"framed bytes").await.expect("write");
        let mut received = [0; 12];
        tokio::time::timeout(Duration::from_secs(5), reader.read_exact(&mut received))
            .await
            .expect("echo deadline")
            .expect("read");
        assert_eq!(&received, b"framed bytes");
        drop(owner);
        let count = tokio::time::timeout(Duration::from_secs(5), reader.read(&mut received))
            .await
            .expect("child must exit on drop")
            .expect("EOF");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn stderr_is_drained_bounded_and_reported_on_exit() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "head -c 65536 /dev/zero >&2; printf 'bridge failed' >&2; exit 7",
        ]);
        let (mut owner, mut reader, _writer) = ChildLink::spawn(&mut command).expect("spawn");
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), reader.read_to_end(&mut bytes))
            .await
            .expect("stderr must not stall stdout")
            .expect("EOF");
        let error = owner.closed().await;
        assert!(error.to_string().contains("bridge failed"), "{error}");
        assert!(error.to_string().contains('7'), "{error}");
        assert_eq!(owner.tail.lock().expect("tail").len(), STDERR_TAIL);
        assert!(
            bytes.is_empty(),
            "stderr must never contaminate protocol stdout"
        );
        assert!(
            owner.closed().await.to_string().contains("bridge failed"),
            "a repeated EOF must not repoll a completed task"
        );
    }

    #[tokio::test]
    async fn the_existing_framed_io_pumps_child_stdio_and_reports_failure() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec cat"]);
        let mut link = super::super::Io::child(&mut command).expect("stdio lane");
        let ping =
            crate::control::encode(&phux_protocol::wire::frame::FrameKind::Ping { nonce: 42 });
        link.write_frame(&ping).await.expect("send frame");
        let frames = tokio::time::timeout(Duration::from_secs(5), link.read_frames())
            .await
            .expect("frame deadline")
            .expect("read")
            .expect("frame batch");
        assert_eq!(frames.len(), 1);
        assert_eq!(&frames[0][..], ping);
        assert_eq!(link.peer_pid(), None);
        assert!(link.terminal_stream_connection().is_none());
        link.shutdown().await;

        let mut command = Command::new("sh");
        command.args(["-c", "printf 'no WSL socket' >&2; exit 9"]);
        let mut link = super::super::Io::child(&mut command).expect("stdio lane");
        let error = link
            .read_frames()
            .await
            .expect_err("failed bridge")
            .describe("WSL");
        assert!(error.contains("no WSL socket"), "{error}");
    }
}
