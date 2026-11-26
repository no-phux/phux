use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use phux_protocol::ResourceId;

use super::{
    AgentServing, Consent, DEADLINE, Question, comment_of, fingerprint, identities, local_agent,
    printable,
};
use crate::control::forward::AgentRequest;

const KEY: &[u8] = b"the-key-blob";
const OTHER: &[u8] = b"another-key";
const FAILURE: &[u8] = &[5];
const SIGNATURE: &[u8] = &[14, 0, 0, 0, 1, b's'];

fn string(value: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(value.len()).unwrap().to_be_bytes().to_vec();
    out.extend(value);
    out
}

/// An identities answer holding `KEY` under the comment `me@here`.
fn listing() -> Vec<u8> {
    let mut out = vec![12, 0, 0, 0, 2];
    out.extend(string(OTHER));
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

/// A local agent that lists [`listing`], signs with [`SIGNATURE`], and
/// records the type of every request it is sent.
fn agent(dir: &tempfile::TempDir) -> (PathBuf, Arc<Mutex<Vec<u8>>>) {
    let path = dir.path().join("agent.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            // A client that hits its deadline drops the socket. That is a
            // closed connection, not a dead agent: keep serving the next one.
            let mut len = [0; 4];
            if stream.read_exact(&mut len).is_err() {
                continue;
            }
            let Ok(n) = usize::try_from(u32::from_be_bytes(len)) else {
                continue;
            };
            let mut message = vec![0; n];
            if message.is_empty() || stream.read_exact(&mut message).is_err() {
                continue;
            }
            record.lock().unwrap().push(message[0]);
            let answer = match message[0] {
                11 => listing(),
                13 => SIGNATURE.to_vec(),
                _ => FAILURE.to_vec(),
            };
            let _ = stream.write_all(&string(&answer));
        }
    });
    (path, seen)
}

fn serving(agent: PathBuf, consent: Consent) -> AgentServing {
    AgentServing::new(agent, "mini".to_owned(), None, consent)
}

fn answer_at(serving: &AgentServing, message: &[u8], started: Instant) -> Vec<u8> {
    let request = AgentRequest {
        id: 1,
        pane: Some(ResourceId::local(3)),
        message: message.to_vec(),
    };
    serving.answer(&request, started).message
}

fn answer(serving: &AgentServing, message: &[u8]) -> Vec<u8> {
    answer_at(serving, message, Instant::now())
}

/// A consent that answers `yes` and records each question.
fn asked(yes: bool) -> (Consent, Arc<Mutex<Vec<String>>>) {
    let questions = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&questions);
    let consent = Consent::Ask(Box::new(move |question: Question| {
        let asker = question.asker(None);
        record
            .lock()
            .unwrap()
            .push(format!("{asker}: {}", question.key));
        question.answer(yes);
    }));
    (consent, questions)
}

fn sent(seen: &Mutex<Vec<u8>>) -> Vec<u8> {
    std::mem::take(&mut *seen.lock().unwrap())
}

#[test]
fn a_listing_passes_without_a_question() {
    let dir = tempfile::tempdir().unwrap();
    let (path, seen) = agent(&dir);
    for consent in [Consent::Never, asked(false).0] {
        assert_eq!(answer(&serving(path.clone(), consent), &[11]), listing());
    }
    assert_eq!(sent(&seen), [11, 11]);
}

/// The question names the pane, the host, the key's fingerprint and its
/// comment; a yes signs; a no, or a question dropped unanswered, does not
/// reach the agent's signer.
#[test]
fn a_signature_is_asked_and_only_a_yes_signs() {
    let dir = tempfile::tempdir().unwrap();
    let (path, seen) = agent(&dir);

    let (consent, questions) = asked(true);
    let serving_yes = serving(path.clone(), consent);
    assert_eq!(answer(&serving_yes, &sign(KEY)), SIGNATURE);
    assert_eq!(sent(&seen), [11, 13], "the listing finds the comment");
    let question = questions.lock().unwrap().join("");
    let print = fingerprint(KEY);
    assert_eq!(question, format!("pane @3 on mini: {print} (me@here)"));

    let (consent, questions) = asked(false);
    for consent in [consent, Consent::Ask(Box::new(drop))] {
        let refusing = serving(path.clone(), consent);
        assert_eq!(answer(&refusing, &sign(KEY)), FAILURE);
        assert_eq!(sent(&seen), [11], "nothing was signed");
    }
    assert_eq!(questions.lock().unwrap().len(), 1);
}

/// With nobody to ask and no `--yes`, a signature is refused unasked;
/// with `--yes` it is signed unasked.
#[test]
fn without_a_person_a_signature_is_refused_and_always_signs_unasked() {
    let dir = tempfile::tempdir().unwrap();
    let (path, seen) = agent(&dir);
    let never = serving(path.clone(), Consent::Never);
    assert_eq!(answer(&never, &sign(KEY)), FAILURE);
    assert_eq!(sent(&seen), [11]);
    let always = serving(path, Consent::Always);
    assert_eq!(answer(&always, &sign(KEY)), SIGNATURE);
    assert_eq!(sent(&seen), [11, 13]);
}

/// A host limited to some keys sees only those, and cannot sign with
/// another even when the person would say yes.
#[test]
fn a_key_filter_hides_and_refuses_every_other_key() {
    let dir = tempfile::tempdir().unwrap();
    let (path, seen) = agent(&dir);
    let limited = |keys: Vec<String>| {
        AgentServing::new(path.clone(), "mini".to_owned(), Some(keys), asked(true).0)
    };
    let only = limited(vec![fingerprint(KEY)]);
    let listed = answer(&only, &[11]);
    assert_eq!(identities(&listed), Some(vec![(KEY, &b"me@here"[..])]));
    assert_eq!(answer(&only, &sign(KEY)), SIGNATURE);
    assert_eq!(sent(&seen), [11, 11, 13]);
    assert_eq!(answer(&only, &sign(OTHER)), FAILURE);
    assert_eq!(sent(&seen), [11], "the other key never reached the signer");
    assert_eq!(answer(&limited(Vec::new()), &[11]), [12, 0, 0, 0, 0]);
}

/// The client refuses for itself what the server should already have
/// refused: nothing but a listing or a signature reaches the local agent,
/// even with `--yes`.
#[test]
fn nothing_but_list_and_sign_reaches_the_local_agent() {
    let dir = tempfile::tempdir().unwrap();
    let (path, seen) = agent(&dir);
    let serving = serving(path, Consent::Always);
    for kind in (0..=u8::MAX).filter(|kind| !matches!(kind, 11 | 13)) {
        assert_eq!(answer(&serving, &[kind, 0, 0, 0, 0]), FAILURE, "{kind}");
    }
    assert_eq!(answer(&serving, &[]), FAILURE);
    // A sign request too short to name a key, and one for a key the agent
    // does not hold, are refused without asking it to sign.
    assert_eq!(answer(&serving, &[13, 0, 0]), FAILURE);
    assert_eq!(sent(&seen), [0_u8; 0]);
    assert_eq!(answer(&serving, &sign(b"not-held")), FAILURE);
    assert_eq!(sent(&seen), [11]);
}

/// A request the server has stopped waiting for is not asked about, a yes
/// that comes after the deadline signs nothing, and a question nobody
/// answers ends at it.
#[test]
fn an_expired_request_is_not_signed() {
    let dir = tempfile::tempdir().unwrap();
    let (path, seen) = agent(&dir);
    let stale = Instant::now().checked_sub(DEADLINE).unwrap();

    let (consent, questions) = asked(true);
    let serving_stale = serving(path.clone(), consent);
    assert_eq!(answer_at(&serving_stale, &sign(KEY), stale), FAILURE);
    assert!(questions.lock().unwrap().is_empty());
    sent(&seen);

    let almost = || {
        Instant::now()
            .checked_sub(DEADLINE.checked_sub(Duration::from_millis(300)).unwrap())
            .unwrap()
    };
    // Answered a moment too late: the question was asked in time.
    let slow = Consent::Ask(Box::new(|question| {
        std::thread::sleep(Duration::from_millis(400));
        question.answer(true);
    }));
    let serving_slow = serving(path.clone(), slow);
    assert_eq!(answer_at(&serving_slow, &sign(KEY), almost()), FAILURE);
    // Never answered: the question is held open past the deadline.
    let held = Arc::new(Mutex::new(None));
    let holder = Arc::clone(&held);
    let open = Consent::Ask(Box::new(move |question| {
        *holder.lock().unwrap() = Some(question);
    }));
    let serving_open = serving(path, open);
    assert_eq!(answer_at(&serving_open, &sign(KEY), almost()), FAILURE);
    held.lock().unwrap().take().unwrap().answer(true);
    assert_eq!(sent(&seen), [11, 11], "neither was signed");
}

/// Two signatures at once are asked one after the other.
#[test]
fn questions_are_asked_one_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _seen) = agent(&dir);
    let (open, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (open_in, most_in) = (Arc::clone(&open), Arc::clone(&most));
    let consent = Consent::Ask(Box::new(move |question| {
        let now = open_in.fetch_add(1, Ordering::SeqCst) + 1;
        most_in.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(50));
        open_in.fetch_sub(1, Ordering::SeqCst);
        question.answer(true);
    }));
    let serving = Arc::new(serving(path, consent));
    let (done, answers) = std::sync::mpsc::channel();
    for id in 0..3 {
        let done = done.clone();
        let request = AgentRequest {
            id,
            pane: None,
            message: sign(KEY),
        };
        serving.handle(request, move |answer| done.send(answer.message).unwrap());
    }
    for _ in 0..3 {
        assert_eq!(
            answers.recv_timeout(Duration::from_secs(5)).unwrap(),
            SIGNATURE
        );
    }
    assert_eq!(most.load(Ordering::SeqCst), 1);
}

/// With no local agent to reach, everything is a failure, not a hang.
#[test]
fn an_unreachable_local_agent_is_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("absent.sock");
    assert!(local_agent(Some(&absent)).is_err());
    let serving = serving(absent, Consent::Always);
    assert_eq!(answer(&serving, &[11]), FAILURE);
    assert_eq!(answer(&serving, &sign(KEY)), FAILURE);
}

#[test]
fn the_comment_is_found_by_key_and_shown_without_control_bytes() {
    assert_eq!(comment_of(&listing(), KEY).as_deref(), Some("me@here"));
    assert_eq!(comment_of(&listing(), b"missing"), None);
    // Truncated listings and other answers hold no key.
    let whole = listing();
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

/// The asker names the pane with the remote's words made safe, and says so
/// when there is no pane to name.
#[test]
fn the_asker_is_named_without_the_remotes_control_bytes() {
    let question = |pane| Question {
        host: "mini".to_owned(),
        pane,
        key: String::new(),
        expires: Instant::now(),
        answer: std::sync::mpsc::sync_channel(1).0,
    };
    let known = question(Some(ResourceId::local(3)));
    assert_eq!(
        known.asker(Some("vim\u{1b}]0;x\u{202e}\n")),
        "pane @3 (vim]0;x) on mini"
    );
    assert_eq!(known.asker(None), "pane @3 on mini");
    assert_eq!(
        question(None).asker(Some("ignored")),
        "a process on mini (pane unknown)"
    );
}
