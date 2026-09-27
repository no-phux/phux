//! `phux stdio-bridge` — splice stdin/stdout to the local server socket: the
//! remote end of the SSH-stdio transport (ADR-0007), run as
//! `ssh HOST phux stdio-bridge`.
//!
//! Byte-transparent except for the opening HELLO, which it stamps with the
//! `SSH_CONNECTION` endpoints as `ssh_origin` (always replacing the client's
//! own), so the server reports the connection as `ssh-stdio`
//! (`docs/spec/L3.md` §3.9). The stamp is a label, not a verified fact, and
//! grants nothing. Trust comes from the UDS's owner-only permissions; ssh
//! supplies authentication, so no bearer preamble is consumed. stdout carries
//! protocol bytes only.

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

/// Run the bridge until either side closes: exit 0 on a clean close, 1 when
/// the socket cannot be connected or the splice fails.
pub(crate) fn run_stdio_bridge(socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    if let Err(refusal) = phux_config::socket::refuse_dev_on_production(&socket_path) {
        eprintln!("phux stdio-bridge: {refusal}");
        return ExitCode::FAILURE;
    }
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
    // A plain runtime drop would wait on the blocking-pool stdin read until
    // the peer typed again; abandon it instead.
    runtime.shutdown_background();
    code
}

/// The ssh endpoints from `SSH_CONNECTION`, or the older `SSH_CLIENT` (no
/// server address). `None` outside ssh or when neither parses.
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

/// Splice both ways until one direction finishes. Client-to-server first
/// goes through [`forward_hello`]; server-to-client runs from the start so a
/// `PONG` is never held back. The other direction is dropped, not drained; the
/// dialer owns reconnection.
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
        // A closed stdout is the normal end (the ssh client hung up), so it exits
        // 0 like stdin EOF.
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("phux stdio-bridge: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Forward the client's opening frames, stamping its HELLO: each pre-HELLO
/// `PING` passes unchanged, the first other frame goes through
/// [`restamp_hello`] (leaving with exactly the bridge's `ssh_origin` or none),
/// and anything invalid is forwarded for the server to refuse.
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

    /// Without an ssh environment the bridge still strips a client's own claim.
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
