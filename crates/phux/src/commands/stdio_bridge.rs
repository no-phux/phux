//! `phux stdio-bridge` — splice stdin/stdout to the local server socket.
//!
//! The remote end of the SSH-stdio transport (ADR-0007, phux-v45.9): a
//! federation hub (or any remote consumer) runs
//! `ssh HOST phux stdio-bridge` and the wire protocol flows over the ssh
//! channel, through this process, into the phux server's Unix socket on
//! HOST. The bridge is byte-transparent — it never parses, frames, or
//! injects anything, so the peer on stdin/stdout talks to the server
//! exactly as a local UDS client would.
//!
//! Trust: connecting to the UDS makes this process an ordinary local
//! client, guarded by the socket's owner-only permissions
//! (docs/operations.md). The SSH channel above supplies remote
//! authentication and encryption, so no bearer preamble is expected or
//! consumed here (ADR-0038 addendum).
//!
//! stdout carries protocol bytes ONLY. Diagnostics go to stderr, which
//! ssh forwards out-of-band to the dialing side's logs.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_server::runtime::default_socket_path;

/// Run the bridge until either side closes.
///
/// Exit code 0 when the bridge ends because a side closed cleanly
/// (server shut down, or the remote peer hung up stdin); 1 when the
/// socket cannot be connected or the splice fails mid-stream.
pub(crate) fn run_stdio_bridge(socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let runtime = match crate::commands::cli_runtime() {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    let code = runtime.block_on(async move {
        let stream = match tokio::net::UnixStream::connect(&socket_path).await {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!(
                    "phux stdio-bridge: cannot connect to server socket {}: {err}",
                    socket_path.display()
                );
                return ExitCode::FAILURE;
            }
        };
        bridge(stream).await
    });
    // `tokio::io::stdin` reads on the blocking pool, and a plain runtime
    // drop would WAIT for that read — hanging the exit until the remote
    // peer types another byte after the server side already closed.
    // Abandon the pool instead: the process is exiting, the read has
    // nowhere to deliver.
    runtime.shutdown_background();
    code
}

/// Splice bytes both ways between (stdin, stdout) and the socket until
/// one direction finishes, then stop.
///
/// One finished direction ends the bridge: if the server closes, there
/// is nothing left to forward to stdout; if stdin reaches EOF, the
/// remote peer is gone and holding the socket open would only pin a
/// dead consumer on the server. The other direction's copy is dropped
/// (not drained) — the transport is gone either way, and the dialer's
/// reconnect logic owns recovery (hub link supervisor backoff, or the
/// client attach loop).
async fn bridge(stream: tokio::net::UnixStream) -> ExitCode {
    let (mut from_server, mut to_server) = stream.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let result = tokio::select! {
        inbound = tokio::io::copy(&mut stdin, &mut to_server) => inbound,
        outbound = tokio::io::copy(&mut from_server, &mut stdout) => outbound,
    };
    // Flush what the winning (or losing) copy already buffered toward
    // the remote peer before exiting; stdout may be a pipe with bytes
    // in flight.
    let _ = tokio::io::AsyncWriteExt::flush(&mut stdout).await;
    match result {
        Ok(_) => ExitCode::SUCCESS,
        // A closed stdout is how this bridge normally ends: the ssh client
        // on the other side hung up, which is the same event as stdin
        // reaching EOF one line above — and that arm exits 0. Reporting it
        // as a failure made every clean `ssh host phux stdio-bridge`
        // teardown look like a transport fault in the caller's logs. Every
        // other I/O error is still real. (This path never panicked: tokio's
        // `copy` returns the error rather than unwrapping it, unlike the
        // `println!` that motivated `crate::output` — same contract, a
        // different way of honoring it.)
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("phux stdio-bridge: {err}");
            ExitCode::FAILURE
        }
    }
}
