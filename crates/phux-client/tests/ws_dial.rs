#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "tests"
)]

use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use phux_client::attach::connection::Connection;
use phux_client::attach::{CertTrust, WsDial};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::ServerCapabilities;
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::FrameKind;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::Request;

const TOKEN_HEX: &str = "1111111111111111111111111111111111111111111111111111111111111111";

async fn read_ws_frame<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>) -> FrameKind
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match ws.next().await.unwrap().unwrap() {
        Message::Binary(data) => FrameKind::decode(&data).unwrap().0,
        other => panic!("expected binary frame, got {other:?}"),
    }
}

async fn write_ws_frame<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>, frame: &FrameKind)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut out = BytesMut::new();
    frame.encode(&mut out);
    ws.send(Message::Binary(out.to_vec())).await.unwrap();
}

async fn accept_hello<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    assert!(matches!(read_ws_frame(ws).await, FrameKind::Hello { .. }));
    write_ws_frame(
        ws,
        &FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new(),
            server_id: Vec::new(),
        },
    )
    .await;
}

const fn ack(seq: u64) -> FrameKind {
    FrameKind::FrameAck {
        terminal_id: TerminalId::Local { id: 1 },
        seq,
    }
}

#[tokio::test]
async fn plaintext_loopback_round_trips_both_directions() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let from_client = ack(101);
    let from_server = ack(202);

    let server = {
        let from_client = from_client.clone();
        let from_server = from_server.clone();
        async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            accept_hello(&mut ws).await;
            assert_eq!(read_ws_frame(&mut ws).await, from_client);
            write_ws_frame(&mut ws, &from_server).await;
        }
    };

    let client = async {
        let dial = WsDial {
            url: format!("ws://{addr}/"),
            token: None,
            trust: CertTrust::SkipVerify,
            tls_server_name: None,
        };
        let mut conn = Connection::connect_ws(&dial).await.expect("dial");
        conn.send(&from_client).await.expect("send");
        conn.recv().await.expect("recv")
    };

    let ((), got) = tokio::join!(server, client);
    assert_eq!(got, from_server);
}

#[tokio::test]
async fn wss_with_pinned_cert_sends_bearer_token() {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    phux_server::transport::tls::ensure_self_signed(&cert, &key).unwrap();
    let fingerprint = phux_server::transport::tls::cert_fingerprint(&cert).unwrap();
    let acceptor = phux_server::transport::tls::acceptor_from_pem(&cert, &key).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let frame = ack(303);

    let server = {
        let frame = frame.clone();
        async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(tcp).await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(tls, |req: &Request, resp| {
                assert_eq!(
                    req.headers()
                        .get("authorization")
                        .unwrap()
                        .to_str()
                        .unwrap(),
                    format!("Bearer {TOKEN_HEX}")
                );
                Ok(resp)
            })
            .await
            .unwrap();
            accept_hello(&mut ws).await;
            assert_eq!(read_ws_frame(&mut ws).await, frame);
        }
    };

    let client = {
        let frame = frame.clone();
        async move {
            let dial = WsDial {
                url: format!("wss://{addr}/"),
                token: Some(TOKEN_HEX.to_owned()),
                trust: CertTrust::Pinned(fingerprint),
                tls_server_name: Some("localhost".to_owned()),
            };
            let mut conn = Connection::connect_ws(&dial).await.expect("dial");
            conn.send(&frame).await.expect("send");
        }
    };

    tokio::join!(server, client);
}
