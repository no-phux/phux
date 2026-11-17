use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

use super::{Consent, DEADLINE, Serving, comment_of, fingerprint, printable};

const KEY: &[u8] = b"the-key-blob";
const FAILURE: &[u8] = &[5];
const SIGNATURE: &[u8] = &[14, 0, 0, 0, 1, b's'];

fn string(value: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(value.len()).unwrap().to_be_bytes().to_vec();
    out.extend(value);
    out
}

/// An identities answer holding `KEY` under the comment `me@here`.
fn identities() -> Vec<u8> {
    let mut out = vec![12, 0, 0, 0, 2];
    out.extend(string(b"another-key"));
    out.extend(string(b"other"));
    out.extend(string(KEY));
    out.extend(string(b"me@here"));
    out
}

fn sign(key: &[u8]) -> Vec<u8> {
    let mut out = vec![13];
    out.extend(string(key));
    out.extend(string(b"data"));
    out.extend([0, 0, 0, 0]);
    out
}

/// A local agent that lists [`identities`], signs with [`SIGNATURE`], and
/// records the type of every request it is sent.
fn agent(dir: &tempfile::TempDir) -> (PathBuf, Arc<Mutex<Vec<u8>>>) {
    let path = dir.path().join("agent.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let len = stream.read_u32().await.unwrap();
            let mut message = vec![0; usize::try_from(len).unwrap()];
            stream.read_exact(&mut message).await.unwrap();
            record.lock().unwrap().push(message[0]);
            let answer = match message[0] {
                11 => identities(),
                13 => SIGNATURE.to_vec(),
                _ => FAILURE.to_vec(),
            };
            stream.write_all(&string(&answer)).await.unwrap();
        }
    });
    (path, seen)
}

fn serving(agent: PathBuf, consent: Consent) -> Serving {
    Serving {
        agent,
        host: "mini".to_owned(),
        consent,
        asking: tokio::sync::Mutex::new(()),
    }
}

/// A consent that answers `yes` and records each question.
fn asked(yes: bool) -> (Consent, Arc<Mutex<Vec<String>>>) {
    let questions = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&questions);
    let consent = Consent::Ask(Arc::new(move |question| {
        record.lock().unwrap().push(question.to_owned());
        yes
    }));
    (consent, questions)
}

fn sent(seen: &Mutex<Vec<u8>>) -> Vec<u8> {
    std::mem::take(&mut *seen.lock().unwrap())
}

#[tokio::test]
async fn a_listing_passes_without_a_question() {
    let dir = tempfile::tempdir().unwrap();
    let (path, seen) = agent(&dir);
    for consent in [Consent::Never, asked(false).0] {
        let serving = serving(path.clone(), consent);
        assert_eq!(serving.answer(&[11], Instant::now()).await, identities());
    }
    assert_eq!(sent(&seen), [11, 11]);
}

/// The question names the host, the key's fingerprint and its comment; a
/// yes signs, a no (or anything else) does not reach the agent's signer.
#[tokio::test]
async fn a_signature_is_asked_and_only_a_yes_signs() {
    let dir = tempfile::tempdir().unwrap();
    let (path, seen) = agent(&dir);

    let (consent, questions) = asked(true);
    let serving_yes = serving(path.clone(), consent);
    let answer = serving_yes.answer(&sign(KEY), Instant::now()).await;
    assert_eq!(answer, SIGNATURE);
    assert_eq!(sent(&seen), [11, 13], "the listing finds the comment");
    let question = questions.lock().unwrap().join("");
    assert_eq!(
        question,
        format!(
            "phux: let mini sign with {} (me@here)? [y/N] ",
            fingerprint(KEY)
        )
    );

    let (consent, questions) = asked(false);
    let serving_no = serving(path, consent);
    assert_eq!(serving_no.answer(&sign(KEY), Instant::now()).await, FAILURE);
    assert_eq!(questions.lock().unwrap().len(), 1);
    assert_eq!(sent(&seen), [11], "nothing was signed");
}

/// With nobody to ask and no `--yes`, a signature is refused unasked;
/// with `--yes` it is signed unasked.
#[tokio::test]
async fn without_a_terminal_a_signature_is_refused_and_yes_signs_unasked() {
    let dir = tempfile::tempdir().unwrap();
    let (path, seen) = agent(&dir);
    let never = serving(path.clone(), Consent::Never);
    assert_eq!(never.answer(&sign(KEY), Instant::now()).await, FAILURE);
    assert_eq!(sent(&seen), [11]);
    let always = serving(path, Consent::Always);
    assert_eq!(always.answer(&sign(KEY), Instant::now()).await, SIGNATURE);
    assert_eq!(sent(&seen), [11, 13]);
}

/// The client refuses for itself what the server should already have
/// refused: nothing but a listing or a signature reaches the local agent,
/// even with `--yes`.
#[tokio::test]
async fn nothing_but_list_and_sign_reaches_the_local_agent() {
    let dir = tempfile::tempdir().unwrap();
    let (path, seen) = agent(&dir);
    let serving = serving(path, Consent::Always);
    for kind in (0..=u8::MAX).filter(|kind| !matches!(kind, 11 | 13)) {
        let answer = serving.answer(&[kind, 0, 0, 0, 0], Instant::now()).await;
        assert_eq!(answer, FAILURE, "type {kind}");
    }
    assert_eq!(serving.answer(&[], Instant::now()).await, FAILURE);
    // A sign request too short to name a key, and one for a key the agent
    // does not hold, are refused without asking it to sign.
    assert_eq!(serving.answer(&[13, 0, 0], Instant::now()).await, FAILURE);
    assert_eq!(sent(&seen), [0_u8; 0]);
    let unknown = serving.answer(&sign(b"not-held"), Instant::now()).await;
    assert_eq!(unknown, FAILURE);
    assert_eq!(sent(&seen), [11]);
}

/// A request the server has stopped waiting for is not asked about, and a
/// yes that comes after the deadline signs nothing.
#[tokio::test]
async fn an_expired_request_is_not_signed() {
    let dir = tempfile::tempdir().unwrap();
    let (path, seen) = agent(&dir);
    let stale = Instant::now().checked_sub(DEADLINE).unwrap();

    let (consent, questions) = asked(true);
    let serving_stale = serving(path.clone(), consent);
    assert_eq!(serving_stale.answer(&sign(KEY), stale).await, FAILURE);
    assert!(questions.lock().unwrap().is_empty());

    // Answered a moment too late: the question was asked in time.
    let almost = Instant::now()
        .checked_sub(DEADLINE.checked_sub(Duration::from_millis(300)).unwrap())
        .unwrap();
    let slow = Consent::Ask(Arc::new(|_| {
        std::thread::sleep(Duration::from_millis(400));
        true
    }));
    let serving_slow = serving(path, slow);
    assert_eq!(serving_slow.answer(&sign(KEY), almost).await, FAILURE);
    assert_eq!(sent(&seen), [11, 11], "neither was signed");
}

/// Two signatures at once are asked one after the other.
#[tokio::test]
async fn questions_are_asked_one_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _seen) = agent(&dir);
    let (open, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (open_in, most_in) = (Arc::clone(&open), Arc::clone(&most));
    let consent = Consent::Ask(Arc::new(move |_| {
        let now = open_in.fetch_add(1, Ordering::SeqCst) + 1;
        most_in.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(50));
        open_in.fetch_sub(1, Ordering::SeqCst);
        true
    }));
    let serving = serving(path, consent);
    let request = sign(KEY);
    let (a, b, c) = tokio::join!(
        serving.answer(&request, Instant::now()),
        serving.answer(&request, Instant::now()),
        serving.answer(&request, Instant::now()),
    );
    assert_eq!(
        (a.as_slice(), b.as_slice(), c.as_slice()),
        (SIGNATURE, SIGNATURE, SIGNATURE)
    );
    assert_eq!(most.load(Ordering::SeqCst), 1);
}

/// With no local agent to reach, everything is a failure, not a hang.
#[tokio::test]
async fn an_unreachable_local_agent_is_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let serving = serving(dir.path().join("absent.sock"), Consent::Always);
    assert_eq!(serving.answer(&[11], Instant::now()).await, FAILURE);
    assert_eq!(serving.answer(&sign(KEY), Instant::now()).await, FAILURE);
}

#[test]
fn the_comment_is_found_by_key_and_shown_without_control_bytes() {
    assert_eq!(comment_of(&identities(), KEY).as_deref(), Some("me@here"));
    assert_eq!(comment_of(&identities(), b"missing"), None);
    // Truncated listings and other answers hold no key.
    let whole = identities();
    assert_eq!(comment_of(&whole[..whole.len() - 3], KEY), None);
    assert_eq!(comment_of(FAILURE, KEY), None);
    assert_eq!(comment_of(&[12, 0xff, 0xff, 0xff, 0xff], KEY), None);
    assert_eq!(printable("me\u{1b}[2J@here\n"), "me[2J@here");
    // RFC 4648 base64 of SHA-256("") without padding.
    assert_eq!(
        fingerprint(b""),
        "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU"
    );
}
