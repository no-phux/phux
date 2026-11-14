use std::time::Duration;

use phux_protocol::forward::{ForwardOp, MAX_AGENT_MESSAGE_BYTES};
use phux_protocol::wire::frame::{AgentEvent, CommandResult, ErrorCode, FrameKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use super::{MAX_PENDING, admit, ask, serve, withdraw};
use crate::runtime::commands::CommandContext;
use crate::state::{ClientId, Outbound, SharedState};

const FAILURE: &[u8] = &[5];
const LIST: &[u8] = &[11];
const LONG: Duration = Duration::from_secs(600);

/// A sign request for key blob `key`.
fn sign(key: &[u8]) -> Vec<u8> {
    let mut message = vec![13];
    message.extend(u32::try_from(key.len()).unwrap().to_be_bytes());
    message.extend(key);
    message.extend([0, 0, 0, 4, b'd', b'a', b't', b'a', 0, 0, 0, 0]);
    message
}

/// A server with forwarding on (a socket path is set; nothing listens).
fn state() -> SharedState {
    let state = SharedState::new();
    state.with_mut(|s| s.forward_agent_mut().socket = Some("/nonexistent".into()));
    state
}

/// Client `id` and the mailbox its events land in.
fn client(
    state: &SharedState,
    id: u64,
    capacity: usize,
) -> (CommandContext, mpsc::Receiver<Outbound>) {
    let (out_tx, out_rx) = mpsc::channel(capacity);
    (
        CommandContext::for_test(state, ClientId(id), &out_tx),
        out_rx,
    )
}

fn offer(ctx: &CommandContext) {
    assert_eq!(admit(ctx, ForwardOp::AgentOffer), CommandResult::Ok);
}

fn reply(ctx: &CommandContext, id: u64, message: &[u8]) -> CommandResult {
    let message = message.to_vec();
    admit(ctx, ForwardOp::AgentReply { id, message })
}

/// The next `agent_request` in `mailbox`: its id and message.
async fn request(mailbox: &mut mpsc::Receiver<Outbound>) -> (u64, Vec<u8>) {
    match mailbox.recv().await {
        Some(Outbound::Frame(FrameKind::Event {
            terminal: None,
            event: AgentEvent::SshAgentRequest { id, message },
            stamp: None,
        })) => (id, message),
        other => panic!("expected an agent_request, got {other:?}"),
    }
}

fn pending(state: &SharedState) -> usize {
    state.with(|s| s.forward_agent().pending.len())
}

#[tokio::test]
async fn off_refuses_both_commands() {
    let state = SharedState::new();
    let (ctx, _mailbox) = client(&state, 1, 4);
    for op in [
        ForwardOp::AgentOffer,
        ForwardOp::AgentReply {
            id: 1,
            message: FAILURE.to_vec(),
        },
    ] {
        let CommandResult::Error { code, .. } = admit(&ctx, op) else {
            panic!("admitted with forwarding off");
        };
        assert_eq!(code, ErrorCode::Unsupported);
    }
    assert_eq!(ask(&state, LIST.to_vec(), LONG).await, FAILURE);
}

#[tokio::test]
async fn a_request_with_no_offer_fails_at_once() {
    let state = state();
    assert_eq!(ask(&state, LIST.to_vec(), LONG).await, FAILURE);
    assert_eq!(ask(&state, sign(b"key"), LONG).await, FAILURE);
}

/// Every type but list (11) and sign (13) is answered here and never
/// reaches the offering client: add, remove, lock, extension, the lot.
#[tokio::test]
async fn only_list_and_sign_reach_the_client() {
    let state = state();
    let (ctx, mut mailbox) = client(&state, 1, 4);
    offer(&ctx);
    for kind in (0..=u8::MAX).filter(|kind| !matches!(kind, 11 | 13)) {
        assert_eq!(ask(&state, vec![kind, 0, 0, 0, 0], LONG).await, FAILURE);
    }
    assert_eq!(ask(&state, Vec::new(), LONG).await, FAILURE);
    assert!(
        mailbox.try_recv().is_err(),
        "a refused request was delivered"
    );
}

/// The reply reaches the pane only when it is the answer type the request
/// calls for; anything else the client sends becomes a failure.
#[tokio::test]
async fn a_reply_must_be_the_matching_answer_type() {
    let state = state();
    let (ctx, mut mailbox) = client(&state, 1, 4);
    offer(&ctx);
    for (message, answer, seen) in [
        (LIST.to_vec(), vec![12, 0, 0, 0, 0], vec![12, 0, 0, 0, 0]),
        (sign(b"key"), vec![14, 9], vec![14, 9]),
        (LIST.to_vec(), vec![14, 9], FAILURE.to_vec()),
        (sign(b"key"), vec![12, 0, 0, 0, 0], FAILURE.to_vec()),
        (sign(b"key"), vec![6], FAILURE.to_vec()),
        (sign(b"key"), FAILURE.to_vec(), FAILURE.to_vec()),
        (sign(b"key"), Vec::new(), FAILURE.to_vec()),
    ] {
        let asked = ask(&state, message.clone(), LONG);
        let answered = async {
            let (id, delivered) = request(&mut mailbox).await;
            assert_eq!(delivered, message);
            assert_eq!(reply(&ctx, id, &answer), CommandResult::Ok);
        };
        let (got, ()) = tokio::join!(asked, answered);
        assert_eq!(got, seen);
        assert_eq!(pending(&state), 0);
    }
}

/// The deadline answers the pane and forgets the request, so the client's
/// late reply is refused rather than held for a later request.
#[tokio::test]
async fn an_unanswered_request_fails_at_the_deadline_and_a_late_reply_is_refused() {
    let state = state();
    let (ctx, mut mailbox) = client(&state, 1, 4);
    offer(&ctx);
    let got = ask(&state, sign(b"key"), Duration::from_millis(50)).await;
    assert_eq!(got, FAILURE);
    assert_eq!(pending(&state), 0);
    let (id, _) = request(&mut mailbox).await;
    let CommandResult::Error { code, .. } = reply(&ctx, id, &[14]) else {
        panic!("a late reply was accepted");
    };
    assert_eq!(code, ErrorCode::NotFound);
}

/// The offering client's disconnect (`withdraw`, run as its connection
/// ends) fails what waits on it without waiting for the deadline, and
/// later requests fail at once.
#[tokio::test(start_paused = true)]
async fn a_disconnect_fails_the_waiting_request_now() {
    let state = state();
    let (ctx, mut mailbox) = client(&state, 1, 4);
    offer(&ctx);
    let started = tokio::time::Instant::now();
    let asked = ask(&state, sign(b"key"), LONG);
    let gone = async {
        request(&mut mailbox).await;
        withdraw(&state, ClientId(1));
    };
    let (got, ()) = tokio::join!(asked, gone);
    assert_eq!(got, FAILURE);
    assert_eq!(started.elapsed(), Duration::ZERO, "waited for the deadline");
    assert_eq!(pending(&state), 0);
    assert_eq!(ask(&state, LIST.to_vec(), LONG).await, FAILURE);
}

/// A connection whose writer is gone before `withdraw` runs (the mailbox
/// is closed) or that is not draining (the mailbox is full) fails the
/// request at once instead of parking it.
#[tokio::test]
async fn a_closed_or_full_mailbox_fails_at_once() {
    let state = state();
    let (ctx, mailbox) = client(&state, 1, 1);
    offer(&ctx);
    drop(mailbox);
    assert_eq!(ask(&state, LIST.to_vec(), LONG).await, FAILURE);

    let (ctx, _mailbox) = client(&state, 2, 1);
    offer(&ctx);
    let first = ask(&state, LIST.to_vec(), Duration::from_millis(50));
    let second = async {
        tokio::task::yield_now().await;
        ask(&state, LIST.to_vec(), LONG).await
    };
    let (first, second) = tokio::join!(first, second);
    assert_eq!((first.as_slice(), second.as_slice()), (FAILURE, FAILURE));
    assert_eq!(pending(&state), 0);
}

/// The newest offer wins. The earlier client still answers what it was
/// already sent, cannot answer the newer client's request, and its
/// disconnect leaves the newer offer standing; when the newer client goes
/// there is no falling back.
#[tokio::test]
async fn the_newest_offer_wins_and_there_is_no_fallback() {
    let state = state();
    let (old, mut old_mail) = client(&state, 1, 4);
    let (new, mut new_mail) = client(&state, 2, 4);
    offer(&old);
    let early = ask(&state, LIST.to_vec(), LONG);
    let rest = async {
        let (early_id, _) = request(&mut old_mail).await;
        offer(&new);
        let late = ask(&state, LIST.to_vec(), LONG);
        let answers = async {
            let (late_id, _) = request(&mut new_mail).await;
            let CommandResult::Error { code, .. } = reply(&old, late_id, &[12]) else {
                panic!("the replaced client answered the newer client's request");
            };
            assert_eq!(code, ErrorCode::NotFound);
            assert_eq!(reply(&old, early_id, &[12, 1]), CommandResult::Ok);
            withdraw(&state, ClientId(1));
            assert_eq!(reply(&new, late_id, &[12, 2]), CommandResult::Ok);
        };
        tokio::join!(late, answers).0
    };
    let (early, late) = tokio::join!(early, rest);
    assert_eq!((early, late), (vec![12, 1], vec![12, 2]));
    assert!(
        old_mail.try_recv().is_err(),
        "the replaced client was asked"
    );

    withdraw(&state, ClientId(2));
    assert_eq!(ask(&state, LIST.to_vec(), LONG).await, FAILURE);
}

/// A pane that gives up (its connection's task is dropped) leaves no
/// entry behind, and the table is bounded.
#[tokio::test]
async fn an_abandoned_request_is_forgotten_and_the_table_is_bounded() {
    let state = state();
    let (ctx, mut mailbox) = client(&state, 1, MAX_PENDING + 4);
    offer(&ctx);
    let mut waiting = Vec::new();
    for _ in 0..MAX_PENDING {
        let mut asked = Box::pin(ask(&state, LIST.to_vec(), LONG));
        assert!(futures_util::poll!(asked.as_mut()).is_pending());
        waiting.push(asked);
    }
    assert_eq!(pending(&state), MAX_PENDING);
    assert_eq!(ask(&state, LIST.to_vec(), LONG).await, FAILURE);
    drop(waiting);
    assert_eq!(pending(&state), 0);
    for _ in 0..MAX_PENDING {
        request(&mut mailbox).await;
    }
    assert!(mailbox.try_recv().is_err(), "the refused request was sent");
}

/// The socket framing: answers come back in order on one connection, and
/// a zero or over-long length is answered with a failure and the end.
#[tokio::test]
async fn a_bad_length_is_refused_and_ends_the_connection() {
    let state = state();
    let (ctx, mut mailbox) = client(&state, 1, 4);
    offer(&ctx);
    let over = u32::try_from(MAX_AGENT_MESSAGE_BYTES + 1).unwrap();
    for bad in [0, over, u32::MAX] {
        let (mut pane, server) = UnixStream::pair().unwrap();
        let serving = serve(&state, server, LONG);
        let asking = async {
            pane.write_all(&[0, 0, 0, 1, 11]).await.unwrap();
            let (id, _) = request(&mut mailbox).await;
            assert_eq!(reply(&ctx, id, &[12, 0, 0, 0, 0]), CommandResult::Ok);
            let mut answer = [0; 9];
            pane.read_exact(&mut answer).await.unwrap();
            assert_eq!(answer, [0, 0, 0, 5, 12, 0, 0, 0, 0]);

            pane.write_all(&bad.to_be_bytes()).await.unwrap();
            let mut rest = Vec::new();
            pane.read_to_end(&mut rest).await.unwrap();
            assert_eq!(rest, [0, 0, 0, 1, 5], "length {bad}");
        };
        tokio::join!(serving, asking);
    }
    assert!(mailbox.try_recv().is_err());
}

/// The largest message is carried whole.
#[tokio::test]
async fn the_largest_message_crosses() {
    let state = state();
    let (ctx, mut mailbox) = client(&state, 1, 4);
    offer(&ctx);
    let mut big = sign(b"key");
    big.resize(MAX_AGENT_MESSAGE_BYTES, 7);
    let asked = ask(&state, big.clone(), LONG);
    let answered = async {
        let (id, delivered) = request(&mut mailbox).await;
        assert_eq!(delivered, big);
        assert_eq!(reply(&ctx, id, &[14]), CommandResult::Ok);
    };
    assert_eq!(tokio::join!(asked, answered).0, [14]);
}

/// The offer and each signature are audited, the signature with the key's
/// fingerprint and its outcome; a listing is not, and no payload is.
#[tokio::test]
async fn offers_and_signatures_are_audited_without_their_bytes() {
    const KEY: &[u8] = b"a-public-key-blob";
    let dir = tempfile::tempdir().unwrap();
    crate::audit::capture_for_test(dir.path(), crate::audit::ROTATE_BYTES);
    let state = state();
    assert_eq!(ask(&state, sign(KEY), LONG).await, FAILURE);
    let (ctx, mut mailbox) = client(&state, 7, 4);
    offer(&ctx);
    for (message, answer) in [(LIST.to_vec(), [12]), (sign(KEY), [14]), (sign(KEY), [5])] {
        let asked = ask(&state, message, LONG);
        let answered = async {
            let (id, _) = request(&mut mailbox).await;
            assert_eq!(reply(&ctx, id, &answer), CommandResult::Ok);
        };
        tokio::join!(asked, answered);
    }
    crate::audit::release_test_capture();

    let raw = std::fs::read_to_string(dir.path().join(crate::audit::LOG_FILE)).unwrap();
    assert!(!raw.contains("a-public-key-blob") && !raw.contains("data"));
    let records: Vec<serde_json::Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let seen: Vec<(&str, &str)> = records
        .iter()
        .map(|record| {
            (
                record["kind"].as_str().unwrap(),
                record["outcome"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("forward.agent.sign", "denied"),
            ("forward.agent.offer", "allowed"),
            ("forward.agent.sign", "allowed"),
            ("forward.agent.sign", "denied"),
        ]
    );
    // `ssh-keygen -lf` prints the same for this blob's SHA-256.
    let fingerprint = super::fingerprint(KEY);
    assert!(fingerprint.starts_with("SHA256:") && fingerprint.len() == 50);
    assert_eq!(records[0]["target"], fingerprint.as_str());
    assert_eq!(records[0]["detail"]["reason"], "no client offers an agent");
    assert_eq!(records[2]["target"], fingerprint.as_str());
    assert_eq!(records[2]["detail"]["client"], 7);
    assert_eq!(
        records[3]["detail"]["reason"],
        "the offering client refused"
    );
}

/// The broker socket sits beside the server's, owner-only, and exists
/// only with `[forward] agent = true`.
#[tokio::test]
async fn the_socket_is_owner_only_beside_the_servers_and_only_when_on() {
    use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
    let dir = tempfile::tempdir().unwrap();
    let expected = dir.path().join("phux.sock.ssh-agent");
    for on in [false, true] {
        let state = SharedState::new();
        state.with_mut(|s| {
            s.set_server_socket_path(dir.path().join("phux.sock"));
            s.set_forward(phux_config::ForwardCfg {
                agent: on,
                ..phux_config::ForwardCfg::default()
            });
        });
        let root = tokio_util::sync::CancellationToken::new();
        tokio::task::LocalSet::new()
            .run_until(async {
                // A previous run's leftover is replaced, not an error.
                std::fs::write(&expected, b"stale").unwrap();
                super::start(&state, &root);
                let bound = state.with(|s| s.forward_agent().socket().map(ToOwned::to_owned));
                if on {
                    assert_eq!(bound.as_deref(), Some(expected.as_path()));
                    let meta = std::fs::symlink_metadata(&expected).unwrap();
                    assert!(meta.file_type().is_socket());
                    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
                    root.cancel();
                    for _ in 0..200 {
                        if !expected.exists() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    assert!(!expected.exists(), "the socket outlived the server");
                } else {
                    assert_eq!(bound, None);
                    assert_eq!(std::fs::read(&expected).unwrap(), b"stale");
                }
            })
            .await;
    }
}
