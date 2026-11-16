//! The relay is blind (ADR-0180): a relayed attach works end to end while
//! the production relay, tapped for every byte it forwards, sees only the
//! consumer's end-to-end TLS session with the server. A relay operator that
//! swaps in its own server behind the route cannot finish that session, and
//! the consumer's bearer never crosses.
//!
//! ```text
//! consumer --QUIC(relay pin)--> relay [tap] <--"phux-relay/1"-- connector (in server)
//!          \__________ TLS 1.3 (server CA pin), terminated by the server __________/
//! ```

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use phux_dial::quic::{DialedStream, InnerStream};
use phux_dial::{DialError, QuicDial, TlsClientIdentity};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::FrameKind;
use phux_server::LaunchCommand as CommandBuilder;
use phux_server_testkit::relay::RelayHarness;
use phux_server_testkit::{WIRE_RECV_TIMEOUT, attach_by_name};
use tempfile::TempDir;
use tokio::time::{sleep, timeout};

use crate::relay_common::{
    RelayedServer, embedder_seeded, hello, inner, loopback, plan, read_frame, relay_config,
    relay_pin, write_frame,
};

const ROUTE: &str = "blind-route";
const TUNNEL_TOKEN: [u8; 32] = [0x51; 32];
/// Typed into the pane and echoed back by `cat`: it crosses the relay once
/// in each direction, as input and as terminal output.
const SECRET: &str = "zebrafish-keystrokes-the-relay-must-not-see";
/// The consumer's HELLO names itself with this.
const CLIENT_NAME: &str = "blind-relay-consumer-hello";

/// One server behind the route, seeded with `cat` when `seed`. The relay's
/// view does not depend on who may act; `relay_inner_tls` covers `paired`.
fn server(dir: &Path, relay: &Relay, seed: bool) -> RelayedServer {
    RelayedServer::start(
        dir,
        relay.harness.addr,
        &relay.pin,
        &TUNNEL_TOKEN,
        |cfg, _| embedder_seeded(cfg, seed.then(|| CommandBuilder::new("/bin/cat"))),
    )
}

/// A tapped relay: everything it forwards for a consumer lands in `tap`.
struct Relay {
    harness: RelayHarness,
    pin: String,
    tap: Arc<Mutex<Vec<u8>>>,
}

impl Relay {
    fn start(dir: &Path) -> Self {
        let tap = Arc::new(Mutex::new(Vec::new()));
        let config = relay_config(dir, loopback(), ROUTE, &TUNNEL_TOKEN);
        let harness = RelayHarness::start_tapped(config, Arc::clone(&tap));
        let pin = relay_pin(dir);
        Self { harness, pin, tap }
    }

    fn forwarded(&self) -> Vec<u8> {
        self.tap.lock().expect("tap").clone()
    }

    /// A consumer plan through this relay to the server whose CA is `ca`.
    fn plan(&self, ca: &str, bearer: &[u8]) -> QuicDial {
        let session = inner(ca, TlsClientIdentity::None, ROUTE);
        plan(
            self.harness.addr,
            &self.pin,
            ROUTE,
            Some(bearer.to_vec()),
            Some(session),
        )
    }
}

/// The end-to-end session, retried while the connector has not yet claimed
/// the route.
async fn session(plan: &QuicDial) -> Result<(quinn::Endpoint, InnerStream), DialError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match phux_dial::quic::dial_stream(plan).await {
            Ok((endpoint, _conn, DialedStream::Inner(stream))) => return Ok((endpoint, *stream)),
            Ok((_, _, DialedStream::Plain { .. })) => panic!("a relayed dial is never plain"),
            Err(DialError::Connect(detail))
                if detail.contains("route offline") && Instant::now() < deadline =>
            {
                sleep(Duration::from_millis(100)).await;
            }
            Err(err) => return Err(err),
        }
    }
}

async fn recv(stream: &mut InnerStream) -> FrameKind {
    timeout(WIRE_RECV_TIMEOUT, read_frame(stream))
        .await
        .expect("frame within the deadline")
        .expect("frame")
}

async fn send(stream: &mut InnerStream, frame: &FrameKind) {
    write_frame(stream, frame).await.expect("send");
}

use phux_server_testkit::contains;

/// A relayed attach types into a real PTY and reads the echo, end to end,
/// while every byte the relay forwarded is ciphertext: no keystroke, no
/// terminal output, no HELLO, no bearer. What it does see is named: the
/// inner `ClientHello`, which carries the route as its server name.
#[test]
fn a_relayed_attach_works_and_the_relay_forwards_only_ciphertext() {
    let dir = TempDir::new().expect("tempdir");
    phux_server_testkit::run_local(async {
        let relay = Relay::start(dir.path());
        let server = server(&dir.path().join("server"), &relay, true);
        let (authority, bearer) = (server.authority.clone(), server.bearer.clone());

        let (_endpoint, mut stream) = session(&relay.plan(&authority, &bearer))
            .await
            .expect("the end-to-end session to the server");
        send(&mut stream, &hello(CLIENT_NAME)).await;
        send(&mut stream, &attach_by_name("default")).await;
        let mut pane = None;
        let mut ready = false;
        while pane.is_none() || !ready {
            match recv(&mut stream).await {
                FrameKind::Attached { snapshot, .. } => {
                    pane = Some(snapshot.resources[0].id.clone());
                }
                FrameKind::BootstrapReady { .. } => ready = true,
                _ => {}
            }
        }
        send(
            &mut stream,
            &FrameKind::InputPaste {
                terminal_id: pane.expect("one pane"),
                event: PasteEvent {
                    trust: PasteTrust::Trusted,
                    data: format!("{SECRET}\r").into_bytes(),
                },
            },
        )
        .await;
        let mut echoed = Vec::new();
        while !contains(&echoed, SECRET.as_bytes()) {
            if let FrameKind::ResourceOutput { bytes, .. } = recv(&mut stream).await {
                echoed.extend_from_slice(&bytes);
            }
        }

        let forwarded = relay.forwarded();
        assert!(
            forwarded.len() > SECRET.len() * 2,
            "the relay forwarded the session: {} bytes",
            forwarded.len()
        );
        assert_eq!(
            forwarded.first(),
            Some(&0x16),
            "the first byte a consumer sends is a TLS handshake record"
        );
        for (what, needle) in [
            ("the keystrokes and their echo", SECRET.as_bytes()),
            ("the HELLO", CLIENT_NAME.as_bytes()),
            ("the bearer", bearer.as_slice()),
            ("the bearer, hex", hex::encode(&bearer).as_bytes()),
        ] {
            assert!(
                !contains(&forwarded, needle),
                "the relay saw {what} in the clear"
            );
        }
        // Honest metadata: the inner `ClientHello` is plaintext, and its
        // server name is the route the relay already routes on.
        assert!(contains(&forwarded, ROUTE.as_bytes()));

        server.stop().await;
        relay.harness.stop().await;
    });
}

/// A relay operator that puts its own server behind the route (it controls
/// the route table) cannot complete the consumer's session: the consumer
/// pins the real server's CA, refuses the impostor's by name, and never
/// writes its bearer, so neither the relay nor the impostor learns it.
#[test]
fn an_impostor_behind_the_route_fails_closed_and_never_sees_the_bearer() {
    let dir = TempDir::new().expect("tempdir");
    phux_server_testkit::run_local(async {
        let relay = Relay::start(dir.path());
        let real = server(&dir.path().join("real"), &relay, false);
        let (authority, bearer) = (real.authority.clone(), real.bearer.clone());
        // The real server holds the route and answers.
        let (_endpoint, mut stream) = session(&relay.plan(&authority, &bearer))
            .await
            .expect("the real server");
        send(&mut stream, &FrameKind::Ping { nonce: 7 }).await;
        assert!(matches!(
            recv(&mut stream).await,
            FrameKind::Pong { nonce: 7 }
        ));
        drop(stream);
        real.stop().await;

        // The operator's own server claims the same route with the same
        // tunnel token, presenting a certificate its own CA issued.
        let impostor = server(&dir.path().join("impostor"), &relay, false);
        let impostor_ca = impostor.authority.clone();
        assert_ne!(impostor_ca, authority);
        let before = relay.forwarded().len();
        match session(&relay.plan(&authority, &bearer)).await {
            Err(DialError::AuthorityChanged(change)) => {
                assert_eq!(change.presented.as_deref(), Some(impostor_ca.as_str()));
            }
            Err(other) => panic!("the impostor must be refused by name, got {other}"),
            Ok(_) => panic!("a session to a server the consumer did not pin"),
        }
        let after = relay.forwarded();
        assert!(
            after.len() > before,
            "the impostor's handshake crossed the relay"
        );
        assert!(!contains(&after, &bearer), "the bearer crossed the relay");
        assert!(!contains(&after, hex::encode(&bearer).as_bytes()));

        impostor.stop().await;
        relay.harness.stop().await;
    });
}
