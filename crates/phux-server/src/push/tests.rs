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
        targets(&mut s, "q1").is_empty(),
        "a live setter is not pushed"
    );

    s.forget_connection(ClientId(7));
    assert_eq!(targets(&mut s, "q1").len(), 1, "an absent setter is pushed");
    assert!(
        targets(&mut s, "q1").is_empty(),
        "the same ask is not pushed twice"
    );
    assert_eq!(targets(&mut s, "q2").len(), 1, "a new ask pushes again");

    s.push_grants_mut().forget(&key);
    s.metadata_delete(&Scope::Global, &key);
    assert!(targets(&mut s, "q3").is_empty());
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
    };
    let status = send(&target, &Notice::ask("local:7".to_owned(), "q1"))
        .await
        .expect("sent");
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

/// An opted-in reply carries its bounded text and title and says it is a
/// reply; an ask's body stays content-free.
#[test]
fn a_reply_notice_carries_bounded_text_and_an_ask_carries_none() {
    let long = "é".repeat(MAX_REPLY_TEXT_BYTES);
    let reply = crate::resource::agent_session::StreamReply {
        turn_id: "t9".to_owned(),
        text: format!("  {long}  "),
        title: Some("Chief".to_owned()),
    };
    let body = Notice::reply("local:3".to_owned(), "t9".to_owned(), &reply).body("mini");
    assert_eq!(body["kind"], "reply");
    assert_eq!(body["question"], "t9");
    assert_eq!(body["pane"], "local:3");
    assert_eq!(body["title"], "Chief");
    let text = body["text"].as_str().expect("text");
    assert!(text.len() <= MAX_REPLY_TEXT_BYTES, "{}", text.len());
    assert!(text.ends_with('…') && text.starts_with('é'));

    let ask = Notice::ask("local:3".to_owned(), "q1").body("mini");
    assert!(ask.get("text").is_none() && ask.get("kind").is_none());
    assert_eq!(bounded(" short ", 16), "short");
}

#[test]
fn panes_are_spelled_as_the_binding_spells_them() {
    assert_eq!(encode_pane(&WireResourceId::local(7)), "local:7");
    assert_eq!(
        encode_pane(&WireResourceId::satellite("box", 3)),
        "satellite:box:3"
    );
}
