use std::net::SocketAddr;
use std::time::Duration;

use phux_protocol::forward::{ForwardChunk, MAX_FORWARD_CHUNK_BYTES};
use phux_protocol::wire::frame::{CommandResult, CommandValue, ErrorCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;

use super::slots::{Forwards, IDLE_REAP, MAX_FORWARDS, UNUSED_REAP};
use super::{DIAL_DEADLINE, Dial, Target, admit_open, admit_read, connect, read, target, write};
use crate::runtime::commands::CommandContext;
use crate::state::{ClientId, SharedState};

const IDLE: Duration = Duration::from_millis(50);
const CHUNK: u32 = 256 * 1024;

fn ctx() -> CommandContext {
    let (out_tx, _out_rx) = tokio::sync::mpsc::channel(1);
    CommandContext::for_test(&SharedState::new(), ClientId(7), &out_tx)
}

/// Dial an admitted open as the dispatcher does.
async fn open(ctx: &CommandContext, dial: Dial, deadline: Duration) -> CommandResult {
    let connecting = connect(dial.target);
    super::open(ctx, dial.reservation, dial.label, connecting, deadline).await
}

async fn listener() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

/// A target that echoes until its peer finishes, then finishes too.
async fn echo() -> u16 {
    let (listener, port) = listener().await;
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

async fn open_at(ctx: &CommandContext, forwards: &Forwards, addr: &str, port: u16) -> u64 {
    let dial = admit_open(forwards, addr, port, &[]).unwrap();
    match open(ctx, dial, DIAL_DEADLINE).await {
        CommandResult::OkWith(CommandValue::ForwardOpened(id)) => id,
        other => panic!("open failed: {other:?}"),
    }
}

/// A forward and the target's end of it.
async fn pair(ctx: &CommandContext, forwards: &Forwards) -> (u64, TcpStream) {
    let (listener, port) = listener().await;
    let id = open_at(ctx, forwards, "127.0.0.1", port).await;
    (id, listener.accept().await.unwrap().0)
}

async fn read_chunk(ctx: &CommandContext, forwards: &Forwards, id: u64) -> ForwardChunk {
    let lease = forwards.lease_read(id).unwrap();
    match read(ctx, lease, CHUNK, IDLE).await {
        CommandResult::OkWith(CommandValue::ForwardChunk(chunk)) => chunk,
        other => panic!("read failed: {other:?}"),
    }
}

/// Read until `len` bytes or the end, whichever comes first.
async fn read_exactly(ctx: &CommandContext, forwards: &Forwards, id: u64, len: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    while bytes.len() < len {
        let chunk = read_chunk(ctx, forwards, id).await;
        bytes.extend(chunk.bytes);
        if chunk.eof {
            break;
        }
    }
    bytes
}

async fn write_ok(ctx: &CommandContext, forwards: &Forwards, id: u64, bytes: &[u8], fin: bool) {
    let lease = forwards.lease_write(id).unwrap();
    assert_eq!(write(ctx, lease, bytes, fin).await, CommandResult::Ok);
}

fn code(result: &CommandResult) -> ErrorCode {
    match result {
        CommandResult::Error { code, .. } => *code,
        other => panic!("expected an error, got {other:?}"),
    }
}

fn message(result: CommandResult) -> String {
    match result {
        CommandResult::Error { message, .. } => message,
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn a_target_is_loopback_or_exactly_what_the_allow_list_spells() {
    let at = |ip: &str, port: u16| format!("{ip}:{port}").parse::<SocketAddr>().unwrap();
    assert_eq!(
        target("localhost", 3000, &[]).unwrap(),
        Target::Addrs(vec![at("127.0.0.1", 3000), at("[::1]", 3000)])
    );
    assert_eq!(
        target("127.0.0.1", 1, &[]).unwrap(),
        Target::Addrs(vec![at("127.0.0.1", 1)])
    );
    assert_eq!(
        target("::1", 1, &[]).unwrap(),
        Target::Addrs(vec![at("[::1]", 1)])
    );
    // Other spellings of this machine, and everything else, are refused
    // without a lookup: the refusal is decided from the text alone.
    for addr in [
        "example.com",
        "LOCALHOST",
        "localhost.",
        "127.0.0.2",
        "0.0.0.0",
        "::",
        "::ffff:127.0.0.1",
        "10.0.0.5",
        "",
    ] {
        let refused = target(addr, 80, &[]).unwrap_err();
        assert_eq!(code(&refused), ErrorCode::PermissionDenied, "{addr}");
        assert!(message(refused).contains("`[forward] allow`"), "{addr}");
    }
    assert_eq!(
        code(&target("localhost", 0, &[]).unwrap_err()),
        ErrorCode::InvalidCommand
    );

    let allow = [
        "db.internal:5432".to_owned(),
        "10.0.0.5:80".to_owned(),
        "[fd00::1]:22".to_owned(),
    ];
    assert_eq!(
        target("db.internal", 5432, &allow).unwrap(),
        Target::Name("db.internal:5432".to_owned())
    );
    assert_eq!(
        target("10.0.0.5", 80, &allow).unwrap(),
        Target::Addrs(vec![at("10.0.0.5", 80)])
    );
    assert_eq!(
        target("fd00::1", 22, &allow).unwrap(),
        Target::Addrs(vec![at("[fd00::1]", 22)])
    );
    for (addr, port) in [
        ("db.internal", 5433),
        ("DB.internal", 5432),
        ("other.internal", 5432),
        ("10.0.0.5", 81),
        ("10.0.0.6", 80),
        ("fd00::2", 22),
    ] {
        let refused = target(addr, port, &allow).unwrap_err();
        assert_eq!(code(&refused), ErrorCode::PermissionDenied, "{addr}:{port}");
    }
}

#[tokio::test]
async fn bytes_echo_both_ways() {
    let (ctx, forwards) = (ctx(), Forwards::default());
    let id = open_at(&ctx, &forwards, "localhost", echo().await).await;
    write_ok(&ctx, &forwards, id, b"ping", false).await;
    assert_eq!(read_exactly(&ctx, &forwards, id, 4).await, b"ping");
    write_ok(&ctx, &forwards, id, b"pong", false).await;
    assert_eq!(read_exactly(&ctx, &forwards, id, 4).await, b"pong");
    // Nothing more: the read waits out its idle deadline and says so.
    let quiet = read_chunk(&ctx, &forwards, id).await;
    assert!(quiet.bytes.is_empty() && !quiet.eof);
}

#[tokio::test]
async fn several_mebibytes_arrive_intact_each_way_and_the_last_write_ends_the_stream() {
    let (ctx, forwards) = (ctx(), Forwards::default());
    let id = open_at(&ctx, &forwards, "127.0.0.1", echo().await).await;
    let sent: Vec<u8> = (0..6 * 1024 * 1024_u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    let upload = async {
        let mut chunks = sent.chunks(MAX_FORWARD_CHUNK_BYTES).peekable();
        while let Some(chunk) = chunks.next() {
            write_ok(&ctx, &forwards, id, chunk, chunks.peek().is_none()).await;
        }
    };
    // The echo ends its side only after it reads ours end, so reading to
    // the end proves `fin` reached the target and its end reached us.
    let download = read_exactly(&ctx, &forwards, id, usize::MAX);
    let ((), received) = tokio::join!(upload, download);
    assert_eq!(received.len(), sent.len());
    assert!(received == sent, "the bytes differ");
}

#[tokio::test]
async fn each_side_can_finish_while_the_other_keeps_sending() {
    let (ctx, forwards) = (ctx(), Forwards::default());

    // The client finishes first: the target reads the end, then answers.
    let (id, mut theirs) = pair(&ctx, &forwards).await;
    write_ok(&ctx, &forwards, id, b"request", true).await;
    let mut request = Vec::new();
    theirs.read_to_end(&mut request).await.unwrap();
    assert_eq!(request, b"request");
    theirs.write_all(b"answer").await.unwrap();
    assert_eq!(read_exactly(&ctx, &forwards, id, 6).await, b"answer");
    let refused = forwards.lease_write(id).unwrap_err();
    assert_eq!(code(&refused), ErrorCode::InvalidCommand);
    drop(theirs);
    assert!(read_chunk(&ctx, &forwards, id).await.eof);

    // The target finishes first: the client reads the end, then still writes.
    let (id, mut theirs) = pair(&ctx, &forwards).await;
    theirs.write_all(b"bye").await.unwrap();
    theirs.shutdown().await.unwrap();
    let last = read_exactly(&ctx, &forwards, id, usize::MAX).await;
    assert_eq!(last, b"bye");
    assert!(read_chunk(&ctx, &forwards, id).await.eof, "the end repeats");
    write_ok(&ctx, &forwards, id, b"late", true).await;
    let mut late = Vec::new();
    theirs.read_to_end(&mut late).await.unwrap();
    assert_eq!(late, b"late");
}

#[tokio::test]
async fn localhost_reaches_a_listener_on_either_loopback_family() {
    let Ok(v6) = TcpListener::bind("[::1]:0").await else {
        return; // no IPv6 loopback on this machine
    };
    let (ctx, forwards) = (ctx(), Forwards::default());
    let port = v6.local_addr().unwrap().port();
    let id = open_at(&ctx, &forwards, "localhost", port).await;
    let (mut theirs, _) = v6.accept().await.unwrap();
    theirs.write_all(b"v6").await.unwrap();
    assert_eq!(read_exactly(&ctx, &forwards, id, 2).await, b"v6");
}

#[tokio::test]
async fn a_refused_dial_says_why_and_frees_the_slot() {
    let (ctx, forwards) = (ctx(), Forwards::default());
    let (gone, port) = listener().await;
    drop(gone);
    let dial = admit_open(&forwards, "127.0.0.1", port, &[]).unwrap();
    let refused = open(&ctx, dial, DIAL_DEADLINE).await;
    assert_eq!(code(&refused), ErrorCode::Unavailable);
    let why = message(refused);
    assert!(
        why.starts_with(&format!("could not connect to 127.0.0.1:{port}")),
        "{why}"
    );

    assert_eq!(forwards.len(), 0, "a failed dial frees its slot");
}

#[tokio::test(start_paused = true)]
async fn a_dial_nothing_answers_is_refused_at_the_deadline_and_frees_the_slot() {
    let (ctx, forwards) = (ctx(), Forwards::default());
    let dial = admit_open(&forwards, "127.0.0.1", 9, &[]).unwrap();
    assert_eq!(forwards.len(), 1, "a dialing forward holds a slot");
    let started = Instant::now();
    let silent = std::future::pending();
    let late = super::open(&ctx, dial.reservation, dial.label, silent, DIAL_DEADLINE).await;
    assert_eq!(started.elapsed(), Duration::from_secs(10));
    assert_eq!(code(&late), ErrorCode::Unavailable);
    assert_eq!(message(late), "no answer from 127.0.0.1:9 within 10 s");
    assert_eq!(forwards.len(), 0, "a dial that timed out frees its slot");
}

#[tokio::test]
async fn forwards_are_capped_and_a_close_frees_a_slot() {
    let (ctx, forwards) = (ctx(), Forwards::default());
    let (listener, port) = listener().await;
    let mut ids = Vec::new();
    for _ in 0..MAX_FORWARDS {
        ids.push(open_at(&ctx, &forwards, "127.0.0.1", port).await);
    }
    let Err(over) = admit_open(&forwards, "127.0.0.1", port, &[]) else {
        panic!("the 33rd forward was admitted");
    };
    assert_eq!(code(&over), ErrorCode::ResourceExhausted);

    assert!(forwards.close(ids[0]).is_some());
    assert!(forwards.close(ids[0]).is_none(), "closing again is a no-op");
    let fresh = open_at(&ctx, &forwards, "127.0.0.1", port).await;
    assert!(ids.iter().all(|id| *id < fresh), "ids are never reused");
    drop(listener);
}

#[tokio::test]
async fn one_read_and_one_write_are_in_flight_at_a_time() {
    let (ctx, forwards) = (ctx(), Forwards::default());
    let (id, _theirs) = pair(&ctx, &forwards).await;
    let reading = forwards.lease_read(id).unwrap();
    assert_eq!(
        code(&forwards.lease_read(id).unwrap_err()),
        ErrorCode::ResourceExhausted
    );
    // A write beside a read is the point; a second write would interleave.
    let writing = forwards.lease_write(id).unwrap();
    assert_eq!(
        code(&forwards.lease_write(id).unwrap_err()),
        ErrorCode::ResourceExhausted
    );
    reading.finish(0);
    writing.finish(0, false);
    assert!(forwards.lease_read(id).is_ok());
    assert!(forwards.lease_write(id).is_ok());

    assert_eq!(
        code(&admit_read(&forwards, id, 0).unwrap_err()),
        ErrorCode::InvalidCommand
    );
    assert_eq!(
        code(&forwards.lease_read(id + 1000).unwrap_err()),
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn a_close_ends_a_read_in_flight_and_the_target_sees_the_end() {
    crate::runtime::test_support::local(async {
        let (ctx, forwards) = (ctx(), Forwards::default());
        let (id, mut theirs) = pair(&ctx, &forwards).await;
        let lease = forwards.lease_read(id).unwrap();
        let parked = tokio::task::spawn_local(async move {
            read(&ctx, lease, CHUNK, Duration::from_secs(60)).await
        });
        tokio::task::yield_now().await;
        assert!(!parked.is_finished(), "the read waits for bytes");

        assert!(forwards.close(id).is_some());
        assert_eq!(code(&parked.await.unwrap()), ErrorCode::NotFound);
        assert_eq!(forwards.len(), 0);
        let mut rest = Vec::new();
        theirs.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty(), "the target's connection ended");
    })
    .await;
}

/// The two orders a close and a command's end can land in: neither brings
/// a closed forward back.
#[tokio::test]
async fn a_command_or_a_dial_that_ends_after_the_close_leaves_nothing_behind() {
    let (ctx, forwards) = (ctx(), Forwards::default());
    let (id, mut theirs) = pair(&ctx, &forwards).await;
    let (reading, writing) = (
        forwards.lease_read(id).unwrap(),
        forwards.lease_write(id).unwrap(),
    );
    assert!(forwards.close(id).is_some());
    reading.finish(3);
    writing.finish(3, false);
    assert_eq!(forwards.len(), 0);
    assert_eq!(
        theirs.read(&mut [0; 1]).await.unwrap(),
        0,
        "both halves went"
    );

    let (listener, port) = listener().await;
    let dial = admit_open(&forwards, "127.0.0.1", port, &[]).unwrap();
    assert!(
        forwards.close_all().is_empty(),
        "a dialing forward was never open"
    );
    assert_eq!(
        code(&open(&ctx, dial, DIAL_DEADLINE).await),
        ErrorCode::NotFound
    );
    assert_eq!(forwards.len(), 0);
    let (mut theirs, _) = listener.accept().await.unwrap();
    assert_eq!(
        theirs.read(&mut [0; 1]).await.unwrap(),
        0,
        "the dial was dropped"
    );
}

#[tokio::test]
async fn an_ending_connection_closes_every_forward_it_opened() {
    let (ctx, forwards) = (ctx(), Forwards::default());
    let (first, mut theirs_first) = pair(&ctx, &forwards).await;
    let (_second, mut theirs_second) = pair(&ctx, &forwards).await;
    write_ok(&ctx, &forwards, first, b"12345", false).await;

    super::close_all(&ctx.state, ctx.client_id, &forwards);
    assert_eq!(forwards.len(), 0);
    let mut seen = Vec::new();
    theirs_first.read_to_end(&mut seen).await.unwrap();
    assert_eq!(seen, b"12345");
    assert_eq!(theirs_second.read(&mut [0; 1]).await.unwrap(), 0);
}

#[tokio::test]
async fn a_target_that_resets_ends_the_forward() {
    let (ctx, forwards) = (ctx(), Forwards::default());
    let (id, theirs) = pair(&ctx, &forwards).await;
    theirs.set_zero_linger().unwrap();
    drop(theirs);
    let lease = forwards.lease_read(id).unwrap();
    let ended = read(&ctx, lease, CHUNK, Duration::from_secs(5)).await;
    assert_eq!(code(&ended), ErrorCode::Unavailable);
    assert!(message(ended).starts_with(&format!("forward {id} ended")));
    assert_eq!(forwards.len(), 0);
}

#[tokio::test]
async fn unused_and_idle_forwards_are_reaped() {
    let (ctx, forwards) = (ctx(), Forwards::default());
    let (abandoned, _theirs) = pair(&ctx, &forwards).await;
    let now = Instant::now();
    assert!(forwards.reap(now).is_empty(), "nothing is abandoned yet");
    let reaped = forwards.reap(now + UNUSED_REAP);
    assert_eq!(reaped.len(), 1, "never read or written within a minute");
    assert_eq!(reaped[0].id, abandoned);
    assert_eq!(
        code(&forwards.lease_read(abandoned).unwrap_err()),
        ErrorCode::NotFound
    );

    let (used, _theirs) = pair(&ctx, &forwards).await;
    write_ok(&ctx, &forwards, used, b"x", false).await;
    let now = Instant::now();
    assert!(forwards.reap(now + UNUSED_REAP).is_empty(), "it was used");
    assert_eq!(forwards.reap(now + IDLE_REAP).len(), 1);
    assert!(used > abandoned, "a reaped id is not handed out again");
    assert_eq!(forwards.len(), 0);
}

#[tokio::test]
async fn the_reaper_closes_abandoned_forwards_and_then_stops() {
    crate::runtime::test_support::local(async {
        let (ctx, forwards) = (ctx(), Forwards::default());
        let (_id, mut theirs) = pair(&ctx, &forwards).await;
        // Paused only once the sockets exist: a paused clock would run
        // the dial's deadline out while the real connect is pending.
        tokio::time::pause();
        assert!(forwards.claim_reaper());
        assert!(!forwards.claim_reaper(), "one reaper per connection");
        super::spawn_reaper(
            ctx.state.clone(),
            ctx.client_id,
            ctx.token.clone(),
            forwards.clone(),
        );

        tokio::time::sleep(UNUSED_REAP + super::REAP_INTERVAL * 2).await;
        assert_eq!(forwards.len(), 0, "the unused forward was reaped");
        assert!(forwards.claim_reaper(), "an empty table's reaper ended");
        assert_eq!(theirs.read(&mut [0; 1]).await.unwrap(), 0);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_forward_is_audited_once_at_each_end_without_its_bytes() {
    const SECRET: &[u8] = b"hunter2-never-logged";
    let dir = tempfile::tempdir().unwrap();
    crate::audit::capture_for_test(dir.path(), crate::audit::ROTATE_BYTES);
    let (ctx, forwards) = (ctx(), Forwards::default());
    let port = echo().await;
    let id = open_at(&ctx, &forwards, "localhost", port).await;
    for _ in 0..3 {
        write_ok(&ctx, &forwards, id, SECRET, false).await;
        read_exactly(&ctx, &forwards, id, SECRET.len()).await;
    }
    let closed = forwards.close(id).unwrap();
    super::audit_close(&ctx.state, ctx.client_id, &closed, "closed");
    crate::audit::release_test_capture();

    let raw = std::fs::read(dir.path().join(crate::audit::LOG_FILE)).unwrap();
    assert!(
        !raw.windows(SECRET.len()).any(|window| window == SECRET),
        "a payload reached the audit log"
    );
    let records: Vec<serde_json::Value> = String::from_utf8(raw)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let kinds: Vec<&str> = records
        .iter()
        .map(|record| record["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["forward.open", "forward.close"], "never per chunk");
    let target = format!("localhost:{port}");
    assert_eq!(records[0]["target"], target.as_str());
    assert_eq!(records[0]["detail"]["client"], 7);
    assert_eq!(records[0]["detail"]["forward"], id);
    assert_eq!(records[1]["target"], target.as_str());
    let bytes = 3 * SECRET.len();
    assert_eq!(records[1]["detail"]["sent"], bytes);
    assert_eq!(records[1]["detail"]["received"], bytes);
    assert_eq!(records[1]["detail"]["reason"], "closed");
}
