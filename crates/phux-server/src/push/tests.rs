use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

fn value(gateway: &str) -> Vec<u8> {
    format!(r#"{{"gateway":"{gateway}","grant":"0123456789abcdef","host":"mini"}}"#).into_bytes()
}

#[test]
fn a_grant_is_bounded_and_its_gateway_is_https_or_loopback() {
    assert!(parse_grant(&value("https://push.example/v1/push")).is_ok());
    assert!(parse_grant(&value("http://127.0.0.1:8080/push")).is_ok());
    assert!(parse_grant(&value("http://[::1]:8080/push")).is_ok());
    assert!(parse_grant(&value("http://push.example/push")).is_err());
    assert!(parse_grant(&value("ftp://push.example")).is_err());
    assert!(parse_grant(&value("https://user@push.example")).is_err());
    assert!(parse_grant(br#"{"gateway":"https://x","grant":"short","host":"m"}"#).is_err());
    assert!(
        parse_grant(br#"{"gateway":"https://x","grant":"0123456789abcdef","host":""}"#).is_err()
    );
    assert!(parse_grant(b"not json").is_err());
    let gateway = Gateway::parse("https://push.example/v1/push").expect("parses");
    assert_eq!((gateway.port, gateway.path.as_str()), (443, "/v1/push"));
}

/// A connected phone projects the ask itself; an absent one is pushed,
/// once per ask id, and a new question pushes again.
#[test]
fn only_absent_devices_are_pushed_and_only_once_per_ask() {
    let mut s = ServerState::new();
    let key = format!("{PUSH_GRANT_KEY_PREFIX}phone-1");
    s.metadata_set(&Scope::Global, &key, value("https://push.example/v1/push"));
    assert!(s.push_grants_mut().register(&key, ClientId(7)));
    s.set_client_connection_cancellation(ClientId(7), tokio_util::sync::CancellationToken::new());
    assert!(
        targets(&mut s, Some("q1")).is_empty(),
        "a live setter is not pushed"
    );

    s.forget_connection(ClientId(7));
    assert_eq!(
        targets(&mut s, Some("q1")).len(),
        1,
        "an absent setter is pushed"
    );
    assert!(
        targets(&mut s, Some("q1")).is_empty(),
        "the same ask is not pushed twice"
    );
    assert_eq!(
        targets(&mut s, Some("q2")).len(),
        1,
        "a new ask pushes again"
    );

    s.push_grants_mut().forget(&key);
    s.metadata_delete(&Scope::Global, &key);
    assert!(targets(&mut s, Some("q3")).is_empty());
}

#[test]
fn the_registry_is_capped_but_rewrites_always_land() {
    let mut grants = PushGrants::default();
    for n in 0..MAX_GRANTS {
        assert!(grants.register(&format!("k{n}"), ClientId(1)));
    }
    assert!(!grants.register("one-too-many", ClientId(1)));
    assert!(grants.register("k0", ClientId(2)));
}

/// The exchange a gateway sees: bearer grant, JSON body with the phone's
/// own host id, the pane in the binding's spelling, and the ask id.
#[tokio::test(flavor = "current_thread")]
async fn a_send_posts_the_content_free_notice_with_the_grant_as_bearer() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let captured = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        let mut raw = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            let n = conn.read(&mut buf).expect("read");
            raw.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&raw);
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("Content-Length: "))
                    .and_then(|v| v.parse().ok())
                    .expect("content length");
                if body.len() >= len || n == 0 {
                    break;
                }
            }
        }
        conn.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .expect("reply");
        String::from_utf8_lossy(&raw).into_owned()
    });
    let target = GrantValue {
        gateway: format!("http://127.0.0.1:{port}/v1/push"),
        grant: "0123456789abcdef".to_owned(),
        host: "mini".to_owned(),
        finished: false,
    };
    let status = send(&target, "local:7", Some("q1")).await.expect("sent");
    assert_eq!(status, 202);
    let request = captured.join().expect("gateway thread");
    assert!(
        request.starts_with("POST /v1/push HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(request.contains("Authorization: Bearer 0123456789abcdef\r\n"));
    let body = request.split("\r\n\r\n").nth(1).expect("body");
    let json: serde_json::Value = serde_json::from_str(body).expect("json body");
    assert_eq!(json["host"], "mini");
    assert_eq!(json["pane"], "local:7");
    assert_eq!(json["question"], "q1");
    assert_eq!(json["action"], "reply");
    assert!(
        json.get("text").is_none(),
        "no question text leaves the host"
    );
}

#[test]
fn panes_are_spelled_as_the_binding_spells_them() {
    assert_eq!(encode_pane(&WireResourceId::local(7)), "local:7");
    assert_eq!(
        encode_pane(&WireResourceId::satellite("box", 3)),
        "satellite:box:3"
    );
}

/// A grant written before `finished` existed still parses, as off; the
/// flag turns it on; a field this server does not know is ignored.
#[test]
fn a_grant_opts_in_to_finished_and_an_older_grant_still_parses() {
    let old = parse_grant(&value("https://push.example/v1/push")).expect("old grant");
    assert!(!old.finished);
    let flagged = br#"{"gateway":"https://x","grant":"0123456789abcdef","host":"m","finished":true,"later":1}"#;
    assert!(parse_grant(flagged).expect("flagged grant").finished);
}

fn record(state: &str) -> Vec<u8> {
    format!(r#"{{"name":"rev","kind":"claude","state":"{state}"}}"#).into_bytes()
}

/// A loopback gateway that answers 202 and keeps every body it was posted.
async fn gateway() -> (
    String,
    std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!(
        "http://127.0.0.1:{}/v1/push",
        listener.local_addr().expect("addr").port()
    );
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let bodies = std::sync::Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            let mut raw = vec![0u8; 4096];
            let n = conn.read(&mut raw).await.expect("read");
            let text = String::from_utf8_lossy(&raw[..n]).into_owned();
            let body = text.split("\r\n\r\n").nth(1).expect("body");
            bodies
                .lock()
                .expect("lock")
                .push(serde_json::from_str(body).expect("json body"));
            conn.write_all(b"HTTP/1.1 202 Accepted\r\nConnection: close\r\n\r\n")
                .await
                .expect("reply");
        }
    });
    (url, seen)
}

/// Every write of the record goes through `metadata_set`, so that is what
/// this drives: one notice for `working` to `done`, to the absent device
/// that opted in and nobody else, and none for a repeat, a flap without
/// work in between, `working` to `blocked`, or a record that is cleared.
#[tokio::test(flavor = "current_thread")]
async fn a_finished_agent_is_pushed_once_per_run_to_absent_devices_that_opted_in() {
    let (url, seen) = gateway().await;
    let grant = |finished: &str| {
        format!(r#"{{"gateway":"{url}","grant":"0123456789abcdef","host":"mini"{finished}}}"#)
            .into_bytes()
    };
    let mut s = ServerState::new();
    let opted = format!("{PUSH_GRANT_KEY_PREFIX}opted");
    let live = format!("{PUSH_GRANT_KEY_PREFIX}live");
    s.metadata_set(&Scope::Global, &opted, grant(r#","finished":true"#));
    s.metadata_set(&Scope::Global, &live, grant(r#","finished":true"#));
    s.metadata_set(
        &Scope::Global,
        &format!("{PUSH_GRANT_KEY_PREFIX}plain"),
        grant(""),
    );
    assert!(s.push_grants_mut().register(&live, ClientId(7)));
    s.set_client_connection_cancellation(ClientId(7), tokio_util::sync::CancellationToken::new());

    let pane = Scope::Resource(WireResourceId::local(7));
    let key = phux_protocol::wire::frame::RESOURCE_AGENT_KEY;
    for state in ["working", "blocked", "working", "done"] {
        s.metadata_set(&pane, key, record(state));
    }
    // A repeat with other bytes, a flap with no work between, and a clear.
    s.metadata_set(&pane, key, br#"{"name":"rev","state":"done"}"#.to_vec());
    for state in ["idle", "done", "unknown"] {
        s.metadata_set(&pane, key, record(state));
    }
    s.metadata_delete(&pane, key);
    assert_eq!(targets(&mut s, Some("q1")).len(), 2, "asks are untouched");

    tokio::time::sleep(Duration::from_millis(300)).await;
    let first = seen.lock().expect("lock").clone();
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0]["action"], "finished");
    assert_eq!(first[0]["host"], "mini");
    assert_eq!(first[0]["pane"], "local:7");
    assert!(first[0].get("question").is_none(), "{first:?}");

    // The next run finishes too, and by then the other phone has left.
    s.forget_connection(ClientId(7));
    s.metadata_set(&pane, key, record("working"));
    s.metadata_set(&pane, key, record("done"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(seen.lock().expect("lock").len(), 3);
}
