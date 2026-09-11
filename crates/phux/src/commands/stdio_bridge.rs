//! `phux stdio-bridge` — splice stdin/stdout to the local server socket.
//!
//! The remote end of the SSH-stdio transport (ADR-0007, phux-v45.9): a
//! federation hub (or any remote consumer) runs
//! `ssh HOST phux stdio-bridge` and the wire protocol flows over the ssh
//! channel, through this process, into the phux server's Unix socket on
//! HOST. After the client's opening HELLO the bridge is byte-transparent:
//! it never parses, frames, or injects anything, so the peer on stdin/stdout
//! talks to the server exactly as a local UDS client would.
//!
//! The one exception is that HELLO. sshd tells the remote command which
//! connection it serves (`SSH_CONNECTION`). The bridge stamps those endpoints
//! on the HELLO as its `ssh_origin` field, always removing whatever the
//! remote client put there, so the server can report the connection as
//! `ssh-stdio` rather than as a local `uds` client (`docs/spec/L3.md` §3.9).
//! The stamp is a label, not a verified fact: the ssh client chooses the
//! remote command and can set this process's `SSH_CONNECTION`, unless ssh
//! forces the command. The server accepts it only because this process is a
//! same-uid Unix-socket peer, and grants nothing for it.
//!
//! Trust: connecting to the UDS makes this process an ordinary local
//! client, guarded by the socket's owner-only permissions
//! (docs/operations.md). The SSH channel above supplies remote
//! authentication and encryption, so no bearer preamble is expected or
//! consumed here (ADR-0038 addendum).
//!
//! stdout carries protocol bytes ONLY. Diagnostics go to stderr, which
//! ssh forwards out-of-band to the dialing side's logs.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;

use phux_protocol::wire::LENGTH_PREFIX_LEN;
use phux_protocol::wire::frame::TYPE_PING;
use phux_protocol::wire::framing::decode_length;
use phux_protocol::wire::ssh_origin::{SshOrigin, restamp_hello};
use phux_server::runtime::default_socket_path;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Run the bridge until either side closes.
///
/// Exit code 0 when the bridge ends because a side closed cleanly
/// (server shut down, or the remote peer hung up stdin); 1 when the
/// socket cannot be connected or the splice fails mid-stream.
pub(crate) fn run_stdio_bridge(socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let origin = ssh_origin_from_env(
        std::env::var("SSH_CONNECTION").ok().as_deref(),
        std::env::var("SSH_CLIENT").ok().as_deref(),
    );
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
        bridge(stream, origin).await
    });
    // `tokio::io::stdin` reads on the blocking pool, and a plain runtime
    // drop would WAIT for that read — hanging the exit until the remote
    // peer types another byte after the server side already closed.
    // Abandon the pool instead: the process is exiting, the read has
    // nowhere to deliver.
    runtime.shutdown_background();
    code
}

/// The ssh endpoints sshd exposed to this process: `SSH_CONNECTION`
/// (`client_ip client_port server_ip server_port`) or, failing that, the
/// older `SSH_CLIENT` (`client_ip client_port server_port`, no server
/// address). `None` outside ssh or when neither value parses, and then the
/// bridge announces nothing.
fn ssh_origin_from_env(connection: Option<&str>, client: Option<&str>) -> Option<SshOrigin> {
    connection
        .and_then(parse_ssh_connection)
        .or_else(|| client.and_then(parse_ssh_client))
}

fn parse_ssh_connection(value: &str) -> Option<SshOrigin> {
    let fields: Vec<&str> = value.split_whitespace().collect();
    let [client_ip, client_port, server_ip, server_port] = fields.as_slice() else {
        return None;
    };
    Some(SshOrigin {
        client: endpoint(client_ip, client_port)?,
        server: endpoint(server_ip, server_port),
    })
}

fn parse_ssh_client(value: &str) -> Option<SshOrigin> {
    let fields: Vec<&str> = value.split_whitespace().collect();
    let [client_ip, client_port, _server_port] = fields.as_slice() else {
        return None;
    };
    Some(SshOrigin {
        client: endpoint(client_ip, client_port)?,
        server: None,
    })
}

fn endpoint(ip: &str, port: &str) -> Option<SocketAddr> {
    // A link-local IPv6 address can carry a `%zone` suffix, which `IpAddr`
    // does not parse and the server has no use for.
    let ip: IpAddr = ip.split('%').next()?.parse().ok()?;
    Some(SocketAddr::new(ip, port.parse().ok()?))
}

/// Splice bytes both ways between (stdin, stdout) and the socket until
/// one direction finishes, then stop.
///
/// The client-to-server direction first forwards the opening frames
/// through [`forward_hello`]. The server-to-client direction runs from the
/// start, so a `PONG` to a pre-HELLO `PING` is never held back.
///
/// One finished direction ends the bridge: if the server closes, there
/// is nothing left to forward to stdout; if stdin reaches EOF, the
/// remote peer is gone and holding the socket open would only pin a
/// dead consumer on the server. The other direction's copy is dropped
/// (not drained) — the transport is gone either way, and the dialer's
/// reconnect logic owns recovery (hub link supervisor backoff, or the
/// client attach loop).
async fn bridge(stream: tokio::net::UnixStream, origin: Option<SshOrigin>) -> ExitCode {
    let (mut from_server, mut to_server) = stream.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let inbound = async {
        forward_hello(&mut stdin, &mut to_server, origin).await?;
        tokio::io::copy(&mut stdin, &mut to_server).await
    };
    let result = tokio::select! {
        inbound = inbound => inbound,
        outbound = tokio::io::copy(&mut from_server, &mut stdout) => outbound,
    };
    // Flush what the winning (or losing) copy already buffered toward
    // the remote peer before exiting; stdout may be a pipe with bytes
    // in flight.
    let _ = stdout.flush().await;
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
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("phux stdio-bridge: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Forward the client's opening frames to the server, stamping its HELLO.
///
/// Before HELLO a client may send only `PING`, so the bridge reads whole
/// frames, forwards each `PING` unchanged, and sends the first other frame
/// through [`restamp_hello`]. A HELLO leaves carrying exactly the bridge's
/// own `ssh_origin`, or none, whatever the remote client put in it. Any
/// other frame, and any header that is not a valid frame length, is
/// forwarded unchanged for the server to refuse. The caller then splices the
/// rest byte for byte.
async fn forward_hello<R, W>(
    input: &mut R,
    output: &mut W,
    origin: Option<SshOrigin>,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let frame = match read_opening_frame(input).await? {
            Opening::Closed => return Ok(()),
            Opening::Unframed(bytes) => return output.write_all(&bytes).await,
            Opening::Frame(frame) => frame,
        };
        if frame.get(LENGTH_PREFIX_LEN) == Some(&TYPE_PING) {
            output.write_all(&frame).await?;
            continue;
        }
        let stamped = restamp_hello(&frame, origin);
        return output.write_all(stamped.as_deref().unwrap_or(&frame)).await;
    }
}

/// One read from the client before HELLO.
enum Opening {
    /// A whole frame: length prefix, type byte, and body.
    Frame(Vec<u8>),
    /// A header whose length §5 forbids, to pass through as-is.
    Unframed(Vec<u8>),
    /// The client closed its side before a whole frame arrived.
    Closed,
}

async fn read_opening_frame<R>(input: &mut R) -> io::Result<Opening>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0u8; LENGTH_PREFIX_LEN];
    if !read_fully(input, &mut header).await? {
        return Ok(Opening::Closed);
    }
    let Ok(body_len) = decode_length(header) else {
        return Ok(Opening::Unframed(header.to_vec()));
    };
    let mut frame = vec![0u8; LENGTH_PREFIX_LEN + body_len];
    let (prefix, body) = frame.split_at_mut(LENGTH_PREFIX_LEN);
    prefix.copy_from_slice(&header);
    if !read_fully(input, body).await? {
        return Ok(Opening::Closed);
    }
    Ok(Opening::Frame(frame))
}

/// `read_exact`, reporting end of input before `buf` fills as `false`.
async fn read_fully<R>(input: &mut R, buf: &mut [u8]) -> io::Result<bool>
where
    R: AsyncRead + Unpin,
{
    match input.read_exact(buf).await {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "tests")]
    #![allow(clippy::panic, reason = "tests")]

    use bytes::BytesMut;
    use phux_protocol::caps::ClientCapabilities;
    use phux_protocol::wire::decode::Decoder;
    use phux_protocol::wire::frame::FrameKind;
    use phux_protocol::wire::ssh_origin::SshOrigin;

    use super::{forward_hello, ssh_origin_from_env};

    fn origin(client: &str, server: Option<&str>) -> SshOrigin {
        SshOrigin {
            client: client.parse().expect("client endpoint"),
            server: server.map(|server| server.parse().expect("server endpoint")),
        }
    }

    fn encoded(frame: &FrameKind) -> Vec<u8> {
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        buf.to_vec()
    }

    fn hello(caps: ClientCapabilities) -> Vec<u8> {
        encoded(&FrameKind::Hello {
            client_name: "hub".to_owned(),
            protocol_major: 0,
            protocol_minor: 9,
            protocol_patch: 0,
            client_caps: caps,
        })
    }

    /// Decode every frame in `bytes`.
    fn frames(bytes: &[u8]) -> Vec<FrameKind> {
        let mut rest = bytes;
        let mut out = Vec::new();
        while !rest.is_empty() {
            let (frame, tail) = Decoder::new(rest).read_frame().expect("a whole frame");
            out.push(frame);
            rest = tail;
        }
        out
    }

    fn hello_origin(frame: &FrameKind) -> Option<SshOrigin> {
        let FrameKind::Hello { client_caps, .. } = frame else {
            panic!("expected HELLO, got {frame:?}");
        };
        client_caps.ssh_origin
    }

    #[test]
    fn the_origin_comes_from_ssh_connection() {
        assert_eq!(
            ssh_origin_from_env(Some("203.0.113.5 52144 198.51.100.7 22"), None),
            Some(origin("203.0.113.5:52144", Some("198.51.100.7:22")))
        );
        assert_eq!(
            ssh_origin_from_env(Some("2001:db8::1 40000 2001:db8::2 2222"), None),
            Some(origin("[2001:db8::1]:40000", Some("[2001:db8::2]:2222")))
        );
        assert_eq!(
            ssh_origin_from_env(Some("fe80::1%en0 40000 fe80::2%en0 22"), None),
            Some(origin("[fe80::1]:40000", Some("[fe80::2]:22"))),
            "a zone suffix is dropped"
        );
    }

    #[test]
    fn ssh_connection_wins_and_ssh_client_is_the_fallback() {
        let both = ssh_origin_from_env(
            Some("203.0.113.5 52144 198.51.100.7 22"),
            Some("10.0.0.9 1 22"),
        );
        assert_eq!(
            both,
            Some(origin("203.0.113.5:52144", Some("198.51.100.7:22")))
        );
        assert_eq!(
            ssh_origin_from_env(None, Some("203.0.113.5 52144 22")),
            Some(origin("203.0.113.5:52144", None)),
            "SSH_CLIENT names no server address"
        );
        assert_eq!(
            ssh_origin_from_env(Some("garbage"), Some("203.0.113.5 52144 22")),
            Some(origin("203.0.113.5:52144", None))
        );
    }

    #[test]
    fn outside_ssh_or_on_a_bad_value_nothing_is_announced() {
        assert_eq!(ssh_origin_from_env(None, None), None);
        for bad in [
            "",
            "203.0.113.5",
            "host 1 2 3",
            "203.0.113.5 99999 1.2.3.4 22",
        ] {
            assert_eq!(ssh_origin_from_env(Some(bad), Some(bad)), None, "{bad:?}");
        }
    }

    #[tokio::test]
    async fn the_hello_is_stamped_and_the_rest_left_for_the_splice() {
        let want = origin("203.0.113.5:52144", Some("198.51.100.7:22"));
        let mut input_bytes = hello(ClientCapabilities::new());
        input_bytes.extend_from_slice(b"after hello");
        let mut input = input_bytes.as_slice();
        let mut output = Vec::new();
        forward_hello(&mut input, &mut output, Some(want))
            .await
            .expect("forwards");
        let sent = frames(&output);
        assert_eq!(sent.len(), 1);
        assert_eq!(hello_origin(&sent[0]), Some(want));
        assert_eq!(input, b"after hello", "nothing past HELLO is consumed");
    }

    #[tokio::test]
    async fn pings_before_hello_pass_unchanged() {
        let want = origin("203.0.113.5:1", None);
        let ping = encoded(&FrameKind::Ping { nonce: 9 });
        let mut input_bytes = ping.clone();
        input_bytes.extend(hello(ClientCapabilities::new()));
        let mut input = input_bytes.as_slice();
        let mut output = Vec::new();
        forward_hello(&mut input, &mut output, Some(want))
            .await
            .expect("forwards");
        assert!(
            output.starts_with(&ping),
            "the PING is forwarded first, as sent"
        );
        let sent = frames(&output);
        assert_eq!(sent.len(), 2);
        assert_eq!(hello_origin(&sent[1]), Some(want));
    }

    /// Security regression: a HELLO padded to the 16 MiB cap, so that the
    /// bridge's own stamp no longer fits, must still leave without the remote
    /// client's forged origin. The bridge never forwards the original frame.
    #[tokio::test]
    async fn an_oversized_hello_loses_its_forged_origin() {
        let cap = usize::try_from(phux_protocol::wire::frame::MAX_FRAME_LEN).expect("fits");
        let mut frame = hello(ClientCapabilities::new().with_ssh_origin(origin("1.2.3.4:1", None)));
        // Pad the body to exactly the cap with an unknown field: id 42 (1
        // byte), wire type (1 byte), and a 4-byte varint length.
        let pad = cap - (frame.len() - 4) - 6;
        let mut extra = BytesMut::new();
        phux_protocol::wire::encode::Encoder::new(&mut extra).write_field(42, &vec![0; pad]);
        frame.extend_from_slice(&extra);
        let length = u32::try_from(frame.len() - 4).expect("fits");
        frame[..4].copy_from_slice(&length.to_be_bytes());
        assert_eq!(frame.len() - 4, cap, "padded to exactly the cap");

        let bridge = origin("[2001:db8::1]:40000", Some("[2001:db8::2]:2222"));
        let mut input = frame.as_slice();
        let mut output = Vec::new();
        forward_hello(&mut input, &mut output, Some(bridge))
            .await
            .expect("forwards");
        let sent = frames(&output);
        assert_eq!(sent.len(), 1);
        assert_eq!(
            hello_origin(&sent[0]),
            None,
            "the forged origin is gone and the stamp did not fit"
        );
    }

    /// Without an ssh environment the bridge still removes a remote client's
    /// own claim from the HELLO it relays. That protects the recorded value
    /// only when ssh forces the bridge command; otherwise the client can set
    /// the bridge's environment itself, which is why the value is a label.
    #[tokio::test]
    async fn a_remote_clients_own_origin_is_removed() {
        let forged = hello(ClientCapabilities::new().with_ssh_origin(origin("10.0.0.1:1", None)));
        let mut input = forged.as_slice();
        let mut output = Vec::new();
        forward_hello(&mut input, &mut output, None)
            .await
            .expect("forwards");
        assert_eq!(output, hello(ClientCapabilities::new()));
    }

    #[tokio::test]
    async fn bytes_that_are_not_a_frame_pass_unchanged() {
        let bytes = [0u8, 0, 0, 0, 0x01, 0xff];
        let mut input = &bytes[..];
        let mut output = Vec::new();
        forward_hello(&mut input, &mut output, Some(origin("203.0.113.5:1", None)))
            .await
            .expect("forwards");
        assert_eq!(output, [0, 0, 0, 0], "the zero-length header goes through");
        assert_eq!(input, [0x01, 0xff], "and the splice carries the rest");
    }
}
