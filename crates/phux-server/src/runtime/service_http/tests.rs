use std::time::{Duration, Instant};

use base64::Engine as _;

use phux_protocol::service_http::{ServiceHttpChunk, ServiceHttpHead, ServiceHttpRequest};
use phux_protocol::wire::frame::{CommandResult, CommandValue, ErrorCode};

use super::exchanges::{Exchanges, MAX_EXCHANGES};
use super::exchanges::{IDLE_REAP, MAX_RETAINED_BODY_BYTES, UNREAD_REAP};
use super::opencode::{OpenCode, parse_status};
use super::upstream::Prepared;
use super::{Target, open, open_local, open_target, read};
use crate::state::SharedState;

mod fake;
use fake::Fake;

const IDLE: Duration = Duration::from_millis(50);

fn get(target: &str) -> ServiceHttpRequest {
    ServiceHttpRequest {
        host: None,
        service: "opencode".to_owned(),
        method: "GET".to_owned(),
        target: target.to_owned(),
        headers: Vec::new(),
        body: Vec::new(),
    }
}

fn local(provider: &'static OpenCode, request: ServiceHttpRequest) -> Target {
    let prepared = Prepared::new(
        &request.method,
        &request.target,
        &request.headers,
        request.body,
    )
    .unwrap();
    Target::Local { provider, prepared }
}

async fn open_ok(
    exchanges: &Exchanges,
    provider: &'static OpenCode,
    request: ServiceHttpRequest,
) -> ServiceHttpHead {
    let reservation = exchanges.reserve(0).unwrap();
    match open(&SharedState::new(), reservation, local(provider, request)).await {
        CommandResult::OkWith(CommandValue::ServiceHttpHead(head)) => head,
        other => panic!("open failed: {other:?}"),
    }
}

async fn read_chunk(exchanges: &Exchanges, exchange: u64, max: u32) -> ServiceHttpChunk {
    let lease = exchanges.lease(exchange).unwrap();
    match read(&SharedState::new(), lease, max, IDLE).await {
        CommandResult::OkWith(CommandValue::ServiceHttpChunk(chunk)) => chunk,
        other => panic!("read failed: {other:?}"),
    }
}

/// Read to the end, returning the body and the chunks' sizes.
async fn read_all(exchanges: &Exchanges, exchange: u64, max: u32) -> (String, Vec<usize>) {
    let mut body = Vec::new();
    let mut sizes = Vec::new();
    loop {
        let chunk = read_chunk(exchanges, exchange, max).await;
        sizes.push(chunk.bytes.len());
        body.extend(chunk.bytes);
        if chunk.end {
            return (String::from_utf8(body).unwrap(), sizes);
        }
    }
}

fn error_code(result: &CommandResult) -> ErrorCode {
    match result {
        CommandResult::Error { code, .. } => *code,
        other => panic!("expected an error, got {other:?}"),
    }
}

fn header<'a>(head: &'a ServiceHttpHead, name: &str) -> Option<&'a str> {
    head.headers
        .iter()
        .find(|(header, _)| header == name)
        .map(|(_, value)| value.as_str())
}

#[test]
fn only_a_single_loopback_origin_line_is_an_origin() {
    for good in [
        "http://127.0.0.1:49374\n",
        "http://[::1]:8080",
        " http://127.0.0.1:1 \n",
    ] {
        assert!(parse_status(good.as_bytes()).unwrap().is_some(), "{good:?}");
    }
    for stopped in ["stopped\n", "", "not running"] {
        assert!(
            parse_status(stopped.as_bytes()).unwrap().is_none(),
            "{stopped:?}"
        );
    }
    for bad in [
        "https://127.0.0.1:49374",
        "http://10.0.0.1:49374",
        "http://127.0.0.2:49374",
        "http://localhost:49374",
        "http://127.0.0.1:0",
        "http://127.0.0.1:49374/",
        "http://127.0.0.1:49374\nhttp://127.0.0.1:49375",
    ] {
        let refusal = parse_status(bad.as_bytes()).unwrap_err();
        assert_eq!(refusal.code, ErrorCode::Unavailable, "{bad:?}");
        assert!(
            refusal.message.starts_with("non-loopback origin refused"),
            "{bad:?}"
        );
    }
}

#[test]
fn requests_are_validated_before_anything_is_reserved() {
    let invalid = |method: &str, target: &str, headers: &[(&str, &str)]| {
        let headers: Vec<_> = headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let refusal = Prepared::new(method, target, &headers, Vec::new()).unwrap_err();
        assert_eq!(refusal.code, ErrorCode::InvalidCommand, "{method} {target}");
    };
    invalid("CONNECT", "/", &[]);
    invalid("GE T", "/", &[]);
    invalid("GET", "api/info", &[]);
    invalid("GET", "//evil/x", &[]);
    invalid("GET", "/a#b", &[]);
    invalid("GET", "http://127.0.0.1/", &[]);
    invalid("GET", "/", &[("bad name", "x")]);
    invalid("GET", "/", &[("x-ok", "a\r\nInjected: 1")]);

    let mut unknown = get("/");
    unknown.service = "nginx".to_owned();
    assert_eq!(open_target(unknown).unwrap_err().code, ErrorCode::NotFound);
    for pairing in ["/api/pair", "/api/pair?x=1", "/auth/connect/abc", "/auth"] {
        let refusal = open_target(get(pairing)).unwrap_err();
        assert_eq!(refusal.code, ErrorCode::InvalidCommand, "{pairing}");
    }
    assert!(open_target(get("/api/pairing-notes")).is_ok());
}

#[tokio::test]
async fn a_stopped_service_is_started_and_headers_follow_the_policy() {
    let fake = Fake::stopped().await;
    let exchanges = Exchanges::default();
    let mut request = get("/echo?q=1");
    request.headers = [
        ("Host", "evil.example"),
        ("Authorization", "Bearer stolen"),
        ("Cookie", "a=b"),
        ("Proxy-Authorization", "x"),
        ("Connection", "x-drop"),
        ("X-Drop", "1"),
        ("Content-Length", "999"),
        ("X-Keep", "kept"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect();
    let head = open_ok(&exchanges, fake.provider(), request).await;
    assert_eq!(head.status, 200);
    assert_eq!(header(&head, "x-trace"), Some("kept"));
    for stripped in ["set-cookie", "connection", "x-hop"] {
        assert_eq!(header(&head, stripped), None, "{stripped}");
    }

    let (seen, _) = read_all(&exchanges, head.exchange, 1024).await;
    let basic = base64::engine::general_purpose::STANDARD.encode("opencode:tok-1");
    assert!(
        seen.contains(&format!("authorization: Basic {basic}\n")),
        "{seen}"
    );
    assert!(seen.contains(&format!("host: {}\n", fake.addr)), "{seen}");
    assert!(seen.contains("x-keep: kept\n"), "{seen}");
    for stripped in [
        "cookie",
        "proxy-authorization",
        "x-drop",
        "connection",
        "999",
    ] {
        assert!(
            !seen.contains(stripped),
            "{stripped} reached upstream: {seen}"
        );
    }
    assert_eq!(fake.pairs(), 1);
}

#[tokio::test]
async fn a_chunked_body_arrives_in_order_and_within_each_read_bound() {
    let fake = Fake::stopped().await;
    let exchanges = Exchanges::default();
    let head = open_ok(&exchanges, fake.provider(), get("/chunked")).await;
    let (body, sizes) = read_all(&exchanges, head.exchange, 4).await;
    assert_eq!(body, "onetwothree");
    assert!(sizes.iter().all(|size| *size <= 4), "{sizes:?}");

    assert_eq!(
        exchanges.lease(head.exchange).unwrap_err().code,
        ErrorCode::NotFound,
        "the chunk that ends the body ends the exchange"
    );
    assert!(
        exchanges.close(head.exchange).is_none(),
        "a close after is a no-op"
    );
    assert_eq!(exchanges.len(), 0);
}

#[tokio::test]
async fn an_event_stream_answers_empty_reads_until_the_next_event() {
    let fake = Fake::stopped().await;
    let exchanges = Exchanges::default();
    let head = open_ok(&exchanges, fake.provider(), get("/sse")).await;
    assert_eq!(header(&head, "content-type"), Some("text/event-stream"));

    let first = read_chunk(&exchanges, head.exchange, 1024).await;
    assert_eq!(first.bytes, b"data: 1\n\n");
    let idle = read_chunk(&exchanges, head.exchange, 1024).await;
    assert!(
        idle.bytes.is_empty() && !idle.end,
        "an idle read answers nothing"
    );

    fake.send_event("data: 2\n\n").await;
    let second = read_chunk(&exchanges, head.exchange, 1024).await;
    assert_eq!(second.bytes, b"data: 2\n\n");
    assert!(!second.end);
}

#[tokio::test]
async fn a_401_mints_a_new_token_once_and_resends() {
    let fake = Fake::stopped().await;
    fake.seen.lock().unwrap().revoked.insert("tok-1".to_owned());
    let provider = fake.provider();
    let exchanges = Exchanges::default();
    let head = open_ok(&exchanges, provider, get("/echo")).await;
    assert_eq!(head.status, 200);
    assert_eq!(fake.pairs(), 2);

    // Within the cooldown a refused token is relayed, not re-minted.
    fake.seen.lock().unwrap().revoked.insert("tok-2".to_owned());
    let head = open_ok(&exchanges, provider, get("/echo")).await;
    assert_eq!(head.status, 401);
    assert_eq!(fake.pairs(), 2);
}

#[tokio::test]
async fn a_401_without_the_services_basic_challenge_mints_nothing() {
    let fake = Fake::stopped().await;
    let exchanges = Exchanges::default();
    let head = open_ok(&exchanges, fake.provider(), get("/denied")).await;
    assert_eq!(head.status, 401);
    assert_eq!(fake.pairs(), 1, "only the first mint");
}

#[tokio::test]
async fn one_deadline_bounds_the_whole_open() {
    let fake = Fake::stopped().await;
    let prepared = Prepared::new("GET", "/hang", &[], Vec::new()).unwrap();
    let refusal = open_local(fake.provider(), &prepared, Duration::from_millis(300))
        .await
        .unwrap_err();
    assert_eq!(refusal.code, ErrorCode::Unavailable);
    assert!(
        refusal.message.contains("did not answer within"),
        "{refusal:?}"
    );
}

#[tokio::test]
async fn unread_and_idle_exchanges_are_reaped() {
    let fake = Fake::stopped().await;
    let provider = fake.provider();
    let exchanges = Exchanges::default();
    let unread = open_ok(&exchanges, provider, get("/sse")).await;
    let read = open_ok(&exchanges, provider, get("/chunked")).await;
    let first = read_chunk(&exchanges, read.exchange, 1).await;
    assert!(!first.end);
    let now = Instant::now();
    assert!(exchanges.reap(now).is_empty(), "nothing is abandoned yet");

    assert_eq!(exchanges.reap(now + UNREAD_REAP).len(), 1);
    assert_eq!(
        exchanges.lease(unread.exchange).unwrap_err().code,
        ErrorCode::NotFound,
        "an exchange nobody read is closed after the unread window"
    );
    assert_eq!(exchanges.len(), 1, "a read exchange survives it");
    assert_eq!(exchanges.reap(now + IDLE_REAP).len(), 1);
    assert_eq!(exchanges.len(), 0);
}

#[test]
fn opening_bodies_share_a_byte_budget() {
    let exchanges = Exchanges::default();
    let held = exchanges.reserve(MAX_RETAINED_BODY_BYTES).unwrap();
    assert_eq!(
        exchanges.reserve(1).unwrap_err().code,
        ErrorCode::ResourceExhausted
    );
    drop(held);
    assert!(exchanges.reserve(1).is_ok());
}

#[tokio::test]
async fn a_moved_service_is_rediscovered_after_a_connect_failure() {
    let old = Fake::stopped().await;
    let provider = old.provider();
    let exchanges = Exchanges::default();
    open_ok(&exchanges, provider, get("/echo")).await;

    let new = Fake::stopped().await;
    old.stop();
    old.report_origin(new.addr);
    let head = open_ok(&exchanges, provider, get("/echo")).await;
    let (seen, _) = read_all(&exchanges, head.exchange, 1024).await;
    assert!(seen.contains(&format!("host: {}\n", new.addr)), "{seen}");
}

#[tokio::test]
async fn exchanges_are_capped_and_close_is_idempotent() {
    let exchanges = Exchanges::default();
    let reservations: Vec<_> = (0..MAX_EXCHANGES)
        .map(|_| exchanges.reserve(0).unwrap())
        .collect();
    assert_eq!(
        exchanges.reserve(0).unwrap_err().code,
        ErrorCode::ResourceExhausted
    );
    let opening = reservations[0].exchange();
    assert_eq!(
        exchanges.lease(opening).unwrap_err().code,
        ErrorCode::ResourceExhausted,
        "an opening exchange cannot be read yet"
    );
    drop(reservations);
    assert_eq!(
        exchanges.len(),
        0,
        "unfulfilled reservations free their slots"
    );

    let fake = Fake::stopped().await;
    let head = open_ok(&exchanges, fake.provider(), get("/chunked")).await;
    assert!(exchanges.close(head.exchange).is_some());
    assert!(
        exchanges.close(head.exchange).is_none(),
        "a second close is a no-op"
    );
    assert_eq!(
        exchanges.lease(head.exchange).unwrap_err().code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn a_close_ends_a_read_in_flight_and_a_disconnect_closes_upstream() {
    let fake = Fake::stopped().await;
    let exchanges = Exchanges::default();
    let head = open_ok(&exchanges, fake.provider(), get("/sse")).await;
    read_chunk(&exchanges, head.exchange, 1024).await;

    let lease = exchanges.lease(head.exchange).unwrap();
    let state = SharedState::new();
    let pending = read(&state, lease, 1024, Duration::from_secs(30));
    let closer = async {
        tokio::task::yield_now().await;
        assert!(
            exchanges.close(head.exchange).is_none(),
            "the read holds it"
        );
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(pending, closer)
    })
    .await
    .expect("the close ends the read");
    assert_eq!(error_code(&result), ErrorCode::NotFound);

    let streaming = open_ok(&exchanges, fake.provider(), get("/sse")).await;
    let upstream_closed = fake.closed.notified();
    let before = fake.closed_connections();
    assert_eq!(exchanges.close_all().len(), 1);
    tokio::time::timeout(Duration::from_secs(5), upstream_closed)
        .await
        .expect("dropping the exchange closes its upstream connection");
    assert!(fake.closed_connections() > before);
    assert_eq!(exchanges.len(), 0);
    let _ = streaming;
}

#[tokio::test]
async fn an_unavailable_service_says_why() {
    let missing: &'static OpenCode = Box::leak(Box::new(OpenCode::at(std::path::PathBuf::from(
        "/nonexistent/opencode",
    ))));
    let exchanges = Exchanges::default();
    let message = |result: CommandResult| match result {
        CommandResult::Error {
            code: ErrorCode::Unavailable,
            message,
        } => message,
        other => panic!("expected UNAVAILABLE, got {other:?}"),
    };
    let reservation = exchanges.reserve(0).unwrap();
    let result = open(&SharedState::new(), reservation, local(missing, get("/"))).await;
    assert!(message(result).contains("could not run"));

    let fake = Fake::stopped().await;
    fake.break_start();
    let reservation = exchanges.reserve(0).unwrap();
    let result = open(
        &SharedState::new(),
        reservation,
        local(fake.provider(), get("/")),
    )
    .await;
    assert!(message(result).contains("service start"));
    assert_eq!(exchanges.len(), 0, "a failed open frees its slot");
}

#[tokio::test(start_paused = true)]
async fn the_reaper_closes_abandoned_exchanges_and_then_stops() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let exchanges = Exchanges::default();
            let reservation = exchanges.reserve(0).unwrap();
            let remote = super::exchanges::Upstream::Remote {
                host: phux_protocol::ids::SatelliteHost::new("ghost"),
                exchange: 7,
            };
            reservation.fulfill(remote).unwrap();
            assert!(exchanges.claim_reaper());
            assert!(!exchanges.claim_reaper(), "one reaper per connection");
            super::spawn_reaper(
                SharedState::new(),
                tokio_util::sync::CancellationToken::new(),
                exchanges.clone(),
            );

            tokio::time::sleep(UNREAD_REAP + super::REAP_INTERVAL * 2).await;
            assert_eq!(exchanges.len(), 0, "the unread exchange was reaped");
            assert!(exchanges.claim_reaper(), "an empty table's reaper ended");
        })
        .await;
}
