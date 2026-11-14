//! `forward::relay` against a real server and real TCP sockets: bytes and
//! half-closes cross both ways, a dropped phux connection ends the local
//! connection, and a reconnect serves new ones.

#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::unwrap_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use phux_client_runtime::control::forward::{self, ForwardError};
use phux_client_runtime::control::{ControlOptions, Status};
use phux_client_runtime::reconnect::Ladder;
use phux_client_runtime::{Client, ClientOptions, ConnectOptions, Runtime, Target};
use phux_server_testkit::{run_local, spawn_server};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A connection that attaches to nothing, as `phux forward` makes.
fn browsing() -> ClientOptions {
    ClientOptions {
        control: ControlOptions {
            client_name: "phux-forward-e2e".to_owned(),
            attach: None,
            viewport: (0, 0),
            ..ControlOptions::default()
        },
        connect: ConnectOptions {
            // A local socket comes back in well under a second.
            ladder: Ladder::AGENT_VERB,
            ..ConnectOptions::default()
        },
    }
}

async fn wait_for_status(client: &Client, status: Status) {
    let start = Instant::now();
    while client.status() != status {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for status {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A target that echoes each connection until its peer finishes.
async fn echo() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut rd, mut wr) = stream.split();
                let _ = tokio::io::copy(&mut rd, &mut wr).await;
            });
        }
    });
    port
}

/// A connected local pair: the application's end and the forward's end.
async fn local_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let app = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    (app, listener.accept().await.unwrap().0)
}

#[test]
fn a_local_connection_is_carried_both_ways_and_ends_when_both_sides_have() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        let client = Arc::new(Runtime::connect(Target::uds(&socket), browsing()).unwrap());
        wait_for_status(&client, Status::Attached).await;

        let refused = forward::open(&client, "example.com", 80).await;
        assert!(
            matches!(&refused, Err(ForwardError::Refused { message, .. })
                if message.starts_with("example.com:80 is not on this server's loopback")),
            "{refused:?}"
        );

        let id = forward::open(&client, "localhost", echo().await)
            .await
            .unwrap();
        let (mut app, local) = local_pair().await;
        let relay = {
            let client = Arc::clone(&client);
            tokio::spawn(async move { forward::relay(&client, id, local).await })
        };
        // Several chunks each way, written while the echo comes back. The
        // echo ends only after it reads our end, and the relay only after
        // both, so reading to the end proves each half-close crossed.
        let sent: Vec<u8> = (0..3 * 1024 * 1024_u32).map(|i| (i % 251) as u8).collect();
        let (mut rd, mut wr) = app.split();
        let upload = async {
            wr.write_all(&sent).await.unwrap();
            wr.shutdown().await.unwrap();
        };
        let download = async {
            let mut received = Vec::new();
            rd.read_to_end(&mut received).await.unwrap();
            received
        };
        let ((), received) = tokio::join!(upload, download);
        assert_eq!(received.len(), sent.len());
        assert!(received == sent, "the bytes differ");
        assert_eq!(relay.await.unwrap(), Ok(()));
        // The relay closed the forward when it ended.
        let gone = client
            .with_control(|control| control.forward_read(id, 1))
            .unwrap()
            .await;
        assert!(
            matches!(gone, Err(ForwardError::Refused { .. })),
            "{gone:?}"
        );

        client.close();
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
}

#[test]
fn a_dropped_connection_ends_the_local_side_and_a_reconnect_serves_new_ones() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        let client = Arc::new(Runtime::connect(Target::uds(&socket), browsing()).unwrap());
        wait_for_status(&client, Status::Attached).await;
        let port = echo().await;

        let id = forward::open(&client, "127.0.0.1", port).await.unwrap();
        let (mut app, local) = local_pair().await;
        let relay = {
            let client = Arc::clone(&client);
            tokio::spawn(async move { forward::relay(&client, id, local).await })
        };
        app.write_all(b"ping").await.unwrap();
        let mut pong = [0; 4];
        app.read_exact(&mut pong).await.unwrap();

        // The server goes away mid-connection.
        drop(shutdown);
        server.await.unwrap().unwrap();
        // A server that shuts down in order may still answer the parked
        // read `NOT_FOUND` before the socket closes; either way it failed.
        let ended = relay.await.unwrap();
        assert!(ended.is_err(), "{ended:?}");
        // The application is not left waiting: its connection ends.
        let cut = tokio::time::timeout(Duration::from_secs(10), app.read(&mut pong)).await;
        assert!(
            matches!(cut, Ok(Ok(0) | Err(_))),
            "the local side must end: {cut:?}"
        );
        wait_for_status(&client, Status::Connecting).await;
        let while_down = forward::open(&client, "127.0.0.1", port).await;
        assert_eq!(while_down, Err(ForwardError::NotConnected));

        // The same client serves a new connection once the server is back.
        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        wait_for_status(&client, Status::Attached).await;
        let id = forward::open(&client, "127.0.0.1", port).await.unwrap();
        let (mut app, local) = local_pair().await;
        let relay = {
            let client = Arc::clone(&client);
            tokio::spawn(async move { forward::relay(&client, id, local).await })
        };
        app.write_all(b"back").await.unwrap();
        app.shutdown().await.unwrap();
        let mut back = Vec::new();
        app.read_to_end(&mut back).await.unwrap();
        assert_eq!(back, b"back");
        assert_eq!(relay.await.unwrap(), Ok(()));

        client.close();
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
}
