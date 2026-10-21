//! Length-prefixed link reads, SSH exit reporting, and framing-loss reasons.

use super::{AbortOnDrop, Duration, FramingError, LINK_IDLE_TIMEOUT};

pub(super) async fn read_length_prefixed<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    buf: &mut bytes::BytesMut,
    framing_violation: &mut Option<FramingError>,
    source: &str,
) -> Result<Option<Vec<u8>>, String> {
    loop {
        match phux_protocol::wire::framing::split_frame(buf) {
            Ok(Some(framed)) => return Ok(Some(framed.to_vec())),
            Ok(None) => {}
            Err(violation) => {
                *framing_violation = Some(violation);
                return Err(framing_loss_reason(violation));
            }
        }
        let n = tokio::io::AsyncReadExt::read_buf(reader, buf)
            .await
            .map_err(|err| format!("read from {source}: {err}"))?;
        if n == 0 {
            return if buf.is_empty() {
                Ok(None)
            } else {
                Err(format!("{source} closed the stream mid-frame"))
            };
        }
    }
}

/// The teardown reason once a WS link has been idle for
/// [`LINK_IDLE_TIMEOUT`].
pub(super) fn ws_idle_error(idle_for: Duration) -> Option<String> {
    (idle_for >= LINK_IDLE_TIMEOUT).then(|| {
        format!(
            "satellite sent nothing for {}s (idle limit {}s); link presumed dead",
            idle_for.as_secs(),
            LINK_IDLE_TIMEOUT.as_secs()
        )
    })
}

/// How long an ssh child gets to exit after its stdout closes before the
/// hub kills it.
pub(super) const SSH_EXIT_GRACE: Duration = Duration::from_secs(5);

/// The loss reason for an ssh link at stdout EOF: exit status plus the
/// bounded stderr tail. Wait and join are bounded by [`SSH_EXIT_GRACE`];
/// a lingering child is killed.
pub(super) async fn ssh_exit_reason(
    child: &mut tokio::process::Child,
    stderr_reader: &mut AbortOnDrop<Vec<u8>>,
) -> String {
    let gathered = tokio::time::timeout(SSH_EXIT_GRACE, async {
        // The drainer finishes at process death, so joining yields the
        // complete tail. `Pin::new` is sound: `JoinHandle` is `Unpin`.
        tokio::join!(child.wait(), std::pin::Pin::new(&mut stderr_reader.0))
    })
    .await;
    match gathered {
        Ok((status, diagnostics)) => {
            let diagnostics = diagnostics.unwrap_or_default();
            let tail = String::from_utf8_lossy(&diagnostics);
            let tail = tail.trim();
            match status {
                Ok(status) if tail.is_empty() => format!("ssh transport ended: {status}"),
                Ok(status) => format!("ssh transport ended: {status}: {tail}"),
                Err(err) => format!("wait for ssh transport: {err}"),
            }
        }
        Err(_elapsed) => {
            let _ = child.start_kill();
            format!(
                "ssh transport closed its stream but the child did not exit within {}s; killed",
                SSH_EXIT_GRACE.as_secs()
            )
        }
    }
}

/// Compose the loss reason for a satellite that broke SPEC §5 framing.
pub(super) fn framing_loss_reason(violation: FramingError) -> String {
    format!("satellite sent a malformed frame: {violation}")
}

/// The SPEC §5 `ERROR { FRAME_TOO_LARGE }` goodbye, shared with the
/// server's client loop.
pub(super) fn encode_frame_too_large(violation: FramingError) -> Vec<u8> {
    let mut encoded = bytes::BytesMut::new();
    phux_protocol::wire::framing::frame_too_large_error(violation).encode(&mut encoded);
    encoded.to_vec()
}

/// Drain `reader` to EOF (or error), keeping only the last `max` bytes.
pub(super) async fn read_tail<R>(reader: &mut R, max: usize) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut tail = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return tail,
            Ok(n) => {
                tail.extend_from_slice(&chunk[..n]);
                if tail.len() > max {
                    tail.drain(..tail.len() - max);
                }
            }
        }
    }
}
