use super::*;
use std::sync::Arc;

use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use phux_protocol::policy::TransportType;
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::server::Request;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

use phux_protocol::wire::framing::{self, LENGTH_PREFIX_LEN as LENGTH_PREFIX};

use phux_protocol::caps::ClientCapabilities;
use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
use tokio::net::TcpStream;
use tokio::task::LocalSet;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_util::sync::CancellationToken;

const TEST_TOKEN: [u8; crate::auth::TOKEN_LEN] = [0x11; crate::auth::TOKEN_LEN];
/// One complete framed message: 4-byte length prefix (body = 3) + body.
const FRAME: [u8; 7] = [0, 0, 0, 3, 0xde, 0xad, 0xbe];

/// A plaintext token-gated listener with one known token. The token file
/// is returned because the store re-reads it on every connection.
async fn token_listener() -> (WsListener, SocketAddr, String, tempfile::NamedTempFile) {
    let file = tempfile::NamedTempFile::new().unwrap();
    crate::auth::write_test_credential(file.path(), &TEST_TOKEN);
    let store = crate::auth::ReloadingTokenStore::load(file.path().to_path_buf()).unwrap();
    let listener = WsListener::loopback_with_tokens(Arc::new(store))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr, hex::encode(TEST_TOKEN), file)
}

fn request(addr: SocketAddr) -> Request {
    format!("ws://{addr}/").into_client_request().unwrap()
}

/// A `ws://` upgrade request carrying `Authorization: Bearer <hex>`.
fn bearer_request(addr: SocketAddr, token_hex: &str) -> Request {
    let mut req = request(addr);
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {token_hex}").parse().unwrap(),
    );
    req
}

fn browser_request(addr: SocketAddr, protocols: &str) -> Request {
    let mut req = request(addr);
    req.headers_mut()
        .insert("sec-websocket-protocol", protocols.parse().unwrap());
    req
}

/// Upgrade with `request`, send `messages`, and return what the server
/// read for each, the admitted identity, and the upgrade response.
async fn exchange(
    listener: &WsListener,
    addr: SocketAddr,
    request: Request,
    messages: &[&[u8]],
) -> (
    Vec<io::Result<Option<BytesMut>>>,
    crate::auth::ConnectionIdentity,
    tokio_tungstenite::tungstenite::handshake::client::Response,
) {
    let server = async {
        let (mut reader, _writer, peer) = listener.accept().await.unwrap();
        let mut reads = Vec::new();
        for _ in messages {
            reads.push(reader.read_frame().await);
        }
        (reads, peer)
    };
    let client = async {
        let tcp = TcpStream::connect(addr).await.unwrap();
        let (mut ws, response) = tokio_tungstenite::client_async(request, tcp)
            .await
            .expect("upgrade");
        for message in messages {
            ws.send(Message::Binary(message.to_vec().into()))
                .await
                .unwrap();
        }
        // Hold the connection open until the server has read.
        tokio::time::sleep(Duration::from_millis(50)).await;
        response
    };
    let ((reads, peer), response) = tokio::join!(server, client);
    (reads, peer, response)
}

async fn refused_handshake(
    listener: &WsListener,
    addr: SocketAddr,
    request: Request,
) -> (String, WebSocketError) {
    let client = async {
        let tcp = TcpStream::connect(addr).await.unwrap();
        tokio_tungstenite::client_async(request, tcp).await
    };
    let (server_result, client_result) = tokio::join!(listener.accept(), client);
    let Err(server_error) = server_result else {
        panic!("server unexpectedly admitted rejected handshake");
    };
    let Err(client_error) = client_result else {
        panic!("client unexpectedly completed rejected handshake");
    };
    (server_error.to_string(), client_error)
}

fn assert_generic_unauthorized(error: WebSocketError) {
    let WebSocketError::Http(response) = error else {
        panic!("expected HTTP rejection");
    };
    assert_eq!(
        response.status(),
        tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        response.body().as_deref(),
        Some(b"missing or invalid pairing token".as_slice())
    );
}

mod admission;
mod anonymous;
mod auth;
mod mtls;
mod revocation;
mod uds;
