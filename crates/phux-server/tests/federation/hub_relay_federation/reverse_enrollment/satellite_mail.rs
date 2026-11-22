//! A satellite's mail and claims on its hub (`docs/agents.md#mail-across-a-hub`):
//! with the credential it joined with, a satellite dials `phux-coord/1` and
//! each stream is one work-store call against the hub's store, in the
//! satellite's own name. An abandoned wait takes nothing, a revoked
//! credential is cut off, and a hub with no work store refuses the ALPN.

use std::net::SocketAddr;

use phux_dial::{CertTrust, DialError, QuicDial, TlsClientIdentity};
use phux_server::uplink::join;
use phux_server::workload::WorkloadRegistry;
use phux_work::{Client, Endpoint, OperationId, ReadMail, SendMail, run_keeper};
use serde_json::{Value, json};

use super::*;
use super::{PairedHub, code, hub_paths, mint, private_dir};

/// The hub's work store, its keeper on a thread of this test.
fn work_store(dir: &std::path::Path) -> Client {
    let endpoint = Endpoint::new(&dir.join("work-state"), &dir.join("work-run"));
    let serving = endpoint.clone();
    std::thread::spawn(move || run_keeper(&serving, Duration::from_secs(60)));
    let deadline = Instant::now() + STEP_DEADLINE;
    while std::os::unix::net::UnixStream::connect(&endpoint.socket).is_err() {
        assert!(Instant::now() < deadline, "keeper never bound its socket");
        std::thread::sleep(Duration::from_millis(5));
    }
    Client::new(endpoint, None)
}

async fn dial_coord(
    quic: SocketAddr,
    ca: &str,
    entry: &phux_config::UplinkConfigEntry,
) -> Result<(quinn::Endpoint, quinn::Connection), DialError> {
    let plan = QuicDial {
        addr: quic,
        server_name: "127.0.0.1".to_owned(),
        token: None,
        trust: CertTrust::Authority { ca: ca.to_owned() },
        identity: Some(TlsClientIdentity::PemFiles {
            certificate: entry.client_cert.clone(),
            private_key: entry.client_key.clone(),
        }),
        inner: None,
    };
    phux_dial::quic::connect_with_alpn(&plan, phux_protocol::policy::COORD_ALPN).await
}

/// One call: a request line out, the reply line back.
async fn call(conn: &quinn::Connection, request: Value) -> Value {
    let line = json!({"build": "test", "request": request}).to_string() + "\n";
    let (mut send, mut recv) = conn.open_bi().await.expect("open a call stream");
    send.write_all(line.as_bytes()).await.unwrap();
    send.finish().unwrap();
    let reply = tokio::time::timeout(STEP_DEADLINE, recv.read_to_end(1 << 20))
        .await
        .expect("the hub answers")
        .expect("a reply");
    serde_json::from_slice(&reply).expect("a JSON reply line")
}

fn send_request(from: &str, to: &str, body: &str) -> Value {
    json!({"op": "send_mail", "thread": "t", "from": from, "to": [to], "body": body,
        "operation_id": OperationId::generate().unwrap().to_hex()})
}

/// What the hub's own agents read at `reader`, off the test's thread.
async fn hub_mail(store: &Client, reader: &str) -> Vec<phux_work::Mail> {
    let store = store.clone();
    let reader = reader.to_owned();
    tokio::task::spawn_blocking(move || {
        store.read_mail(ReadMail {
            reader,
            thread: None,
            all: true,
            mark_read: false,
            limit: 0,
        })
    })
    .await
    .unwrap()
    .unwrap()
}

#[test]
fn a_joined_satellite_mails_the_hub_in_its_own_name_until_revoked() {
    phux_server_testkit::run_local(async {
        let dir = private_dir();
        let d = dir.path();
        let store = work_store(d);
        let hub = PairedHub::boot_with(d, 0, Some(Box::new(store.clone()))).await;
        let join_code = code(&hub, d, &mint(d, 600));
        let joined = join(&join_code, &d.join("sat-state")).await.expect("joins");
        let (_endpoint, conn) = dial_coord(hub.quic, &join_code.ca, &joined.entry)
            .await
            .expect("the satellite credential opens the mail ALPN");

        // The box writes as itself; the hub reads it as `sat/@3`.
        let sent = call(&conn, send_request("pi@3", "hub/@749", "tests pass")).await;
        assert_eq!(sent["result"], "sent", "{sent}");
        assert_eq!(sent["value"]["mail"]["from"], "pi@3");
        assert_eq!(sent["value"]["mail"]["to"], json!(["hub/@749"]));
        let mail = hub_mail(&store, "@749").await;
        assert_eq!(mail[0].from, "sat/pi@3");

        // It cannot read the hub's mail or speak as the hub.
        let read = call(
            &conn,
            json!({"op": "read_mail", "reader": "hub/@749", "all": true, "mark_read": false, "limit": 0}),
        )
        .await;
        assert_eq!(read["value"]["kind"], "invalid_text", "{read}");
        let forged = call(&conn, send_request("hub/@749", "@749", "forged")).await;
        assert_eq!(forged["value"]["kind"], "invalid_text", "{forged}");
        let listed = call(&conn, json!({"op": "list", "archived": false})).await;
        assert_eq!(listed["value"]["kind"], "invalid_text", "{listed}");
        assert_eq!(hub_mail(&store, "@749").await.len(), 1, "nothing forged");

        // A wait the satellite abandons takes nothing: the next one gets it.
        let (mut request, recv) = conn.open_bi().await.unwrap();
        let wait = json!({"build": "test", "request":
            {"op": "wait_mail", "reader": "@3", "timeout_ms": 30_000, "limit": 0}});
        request
            .write_all((wait.to_string() + "\n").as_bytes())
            .await
            .unwrap();
        request.finish().unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(recv);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let reply = {
            let store = store.clone();
            tokio::task::spawn_blocking(move || {
                store.send_mail(SendMail {
                    thread: "t".into(),
                    from: "@749".into(),
                    to: vec!["sat/@3".into()],
                    body: "merge it".into(),
                    operation_id: OperationId::generate().unwrap(),
                })
            })
            .await
            .unwrap()
            .unwrap()
        };
        assert_eq!(reply.mail.to, ["sat/@3"]);
        let taken = call(
            &conn,
            json!({"op": "wait_mail", "reader": "@3", "timeout_ms": 0, "limit": 0}),
        )
        .await;
        assert_eq!(taken["result"], "mail", "{taken}");
        assert_eq!(taken["value"][0]["body"], "merge it");
        assert_eq!(taken["value"][0]["from"], "hub/@749");

        // Revoking the credential closes the connection and refuses redials.
        WorkloadRegistry::revoke(&hub_paths(d).registry, &joined.credential).unwrap();
        let closed = tokio::time::timeout(STEP_DEADLINE, conn.closed())
            .await
            .expect("revocation closes the mail link");
        assert!(
            matches!(
                phux_dial::quic::close_error(&closed),
                DialError::AuthRefused(_)
            ),
            "{closed:?}"
        );
        let redial = dial_coord(hub.quic, &join_code.ca, &joined.entry).await;
        let refused = match redial {
            Err(err) => err,
            Ok((_endpoint, conn)) => {
                let closed = tokio::time::timeout(STEP_DEADLINE, conn.closed())
                    .await
                    .expect("a revoked credential is closed");
                phux_dial::quic::close_error(&closed)
            }
        };
        assert!(matches!(refused, DialError::AuthRefused(_)), "{refused:?}");

        hub.stop().await;
    });
}

#[test]
fn a_hub_with_no_work_store_refuses_satellite_mail() {
    phux_server_testkit::run_local(async {
        let dir = private_dir();
        let d = dir.path();
        let hub = PairedHub::boot_with(d, 0, None).await;
        let join_code = code(&hub, d, &mint(d, 600));
        let joined = join(&join_code, &d.join("sat-state")).await.expect("joins");
        let (_endpoint, conn) = dial_coord(hub.quic, &join_code.ca, &joined.entry)
            .await
            .expect("the handshake itself completes");
        let closed = tokio::time::timeout(STEP_DEADLINE, conn.closed())
            .await
            .expect("the hub closes it");
        assert!(
            matches!(
                phux_dial::quic::close_error(&closed),
                DialError::AuthRefused(_)
            ),
            "{closed:?}"
        );
        hub.stop().await;
    });
}
