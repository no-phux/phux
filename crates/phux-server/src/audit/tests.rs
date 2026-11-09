#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use base64::Engine as _;
use ring::signature::{Ed25519KeyPair, KeyPair as _};

use super::*;

fn entry(n: u64) -> Entry {
    Entry {
        kind: Kind::Command,
        actor: AuditActor {
            credential: Some(format!("sha256:{n:064x}")),
            principal: Some("automation".to_owned()),
            subject: None,
            on_behalf_of: None,
            transport: Transport::Quic,
        },
        action: "KILL_RESOURCE".to_owned(),
        target: Some(format!("terminal:{n}")),
        outcome: Outcome::Allowed,
        detail: serde_json::json!({ "client": n }),
    }
}

fn write(dir: &Path, count: u64, rotate_at: u64) {
    let mut log = AuditLog::open(dir).unwrap();
    log.rotate_at = rotate_at;
    for n in 0..count {
        log.append(entry(n)).unwrap();
    }
}

#[test]
fn records_chain_and_verify() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), 3, ROTATE_BYTES);
    let report = verify_dir(dir.path()).unwrap();
    assert_eq!(report.failure, None);
    assert_eq!(
        (report.records, report.first_seq, report.last_seq),
        (3, Some(0), Some(2))
    );
    assert!(report.from_genesis);

    let records: Vec<AuditRecord> = read_records(dir.path())
        .unwrap()
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(records[0].prev, GENESIS);
    assert_eq!(records[1].prev, records[0].hash);
    assert!(records[0].ts.ends_with('Z'));

    // The documented wire shape: field order, nulls, no extra fields.
    let line = std::fs::read_to_string(dir.path().join(LOG_FILE)).unwrap();
    let first = line.lines().next().unwrap();
    assert!(first.starts_with(r#"{"v":1,"seq":0,"ts":""#), "{first}");
    assert!(first.contains(r#""kind":"command","actor":{"credential":"sha256:"#));
    assert!(first.contains(r#""subject":null,"on_behalf_of":null,"transport":"quic"}"#));
    assert!(first.contains(r#""outcome":"allowed","detail":{"client":0},"prev":"000"#));

    // Owner-only material.
    let mode = |name: &str| {
        std::fs::metadata(dir.path().join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(LOG_FILE), 0o600);
    assert_eq!(mode(KEY_FILE), 0o600);
    assert_eq!(
        std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[test]
fn reopening_continues_the_chain() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), 2, ROTATE_BYTES);
    write(dir.path(), 2, ROTATE_BYTES);
    let report = verify_dir(dir.path()).unwrap();
    assert_eq!(report.failure, None);
    assert_eq!(report.records, 4);
}

#[test]
fn an_edited_line_fails_at_its_seq() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), 4, ROTATE_BYTES);
    let path = dir.path().join(LOG_FILE);
    let text = std::fs::read_to_string(&path).unwrap();
    let edited = text.replacen("terminal:2", "terminal:9", 1);
    assert_ne!(text, edited);
    std::fs::write(&path, edited).unwrap();

    let failure = verify_dir(dir.path()).unwrap().failure.unwrap();
    assert_eq!(failure.seq, Some(2));
    assert_eq!(failure.line, 3);
    assert_eq!(failure.reason, "hash does not match the record");
}

#[test]
fn a_rehashed_line_still_fails_its_signature() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), 2, ROTATE_BYTES);
    let path = dir.path().join(LOG_FILE);
    let text = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut forged: AuditRecord = serde_json::from_str(&lines[1]).unwrap();
    forged.outcome = Outcome::Denied;
    forged.hash = hex::encode(forged.digest());
    lines[1] = serde_json::to_string(&forged).unwrap();
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();

    let failure = verify_dir(dir.path()).unwrap().failure.unwrap();
    assert_eq!(failure.seq, Some(1));
    assert_eq!(failure.reason, "signature does not verify");
}

#[test]
fn a_deleted_line_breaks_the_chain() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), 3, ROTATE_BYTES);
    let path = dir.path().join(LOG_FILE);
    let text = std::fs::read_to_string(&path).unwrap();
    let kept: Vec<&str> = text
        .lines()
        .enumerate()
        .filter(|(i, _)| *i != 1)
        .map(|(_, l)| l)
        .collect();
    std::fs::write(&path, kept.join("\n") + "\n").unwrap();

    let failure = verify_dir(dir.path()).unwrap().failure.unwrap();
    assert_eq!(failure.seq, Some(2));
    assert_eq!(failure.reason, "seq is not contiguous");
}

#[test]
fn rotation_keeps_the_chain_and_drops_the_oldest() {
    let dir = tempfile::tempdir().unwrap();
    // Every record after the first in a file rotates: one record per file.
    write(dir.path(), 7, 1);
    let files = log_files(dir.path());
    assert_eq!(files.len(), 5, "{files:?}");

    let report = verify_dir(dir.path()).unwrap();
    assert_eq!(report.failure, None);
    assert_eq!((report.first_seq, report.last_seq), (Some(2), Some(6)));
    assert!(!report.from_genesis, "rotation dropped seq 0 and 1");

    // A restart resumes from the live file's head.
    write(dir.path(), 1, 1);
    let report = verify_dir(dir.path()).unwrap();
    assert_eq!(report.failure, None);
    assert_eq!(report.last_seq, Some(7));
}

#[test]
fn a_torn_tail_is_reported_and_writing_resumes_after_it() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), 2, ROTATE_BYTES);
    let path = dir.path().join(LOG_FILE);
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(br#"{"v":1,"seq":2,"ts"#).unwrap();
    drop(file);

    write(dir.path(), 1, ROTATE_BYTES);
    let records = read_records(dir.path()).unwrap();
    assert_eq!(records.len(), 4);
    assert!(records[2].is_err());
    assert_eq!(records[3].as_ref().unwrap().seq, 2);
    let failure = verify_dir(dir.path()).unwrap().failure.unwrap();
    assert_eq!(failure.seq, Some(2));
}

#[test]
fn tail_keeps_the_last_n_since_a_seq() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), 6, ROTATE_BYTES);
    let seqs = |n, since| -> Vec<u64> {
        tail(dir.path(), n, since)
            .unwrap()
            .iter()
            .map(|record| record.seq)
            .collect()
    };
    assert_eq!(seqs(2, None), [4, 5]);
    assert_eq!(seqs(10, Some(3)), [3, 4, 5]);
    assert!(seqs(0, None).is_empty());
}

#[test]
fn the_actor_comes_from_the_connection_identity() {
    use phux_protocol::policy::{PeerIdentity, TransportType};
    let peer = |transport| PeerIdentity {
        uid: 501,
        pid: Some(7),
        exe_path: None,
        mcp_host_key: None,
        transport,
        source_addr: None,
    };
    let local = audit_actor(&ConnectionIdentity::from(peer(TransportType::UnixSocket)));
    assert_eq!(local, AuditActor::server());
    let ws = audit_actor(&ConnectionIdentity::from(peer(TransportType::WebSocket)));
    assert_eq!(ws.transport, Transport::Wss);

    let mut quic = ConnectionIdentity::from(peer(TransportType::Quic));
    quic.credential = Some(crate::auth::AuthenticatedCredential {
        id: "sha256:ab".to_owned(),
        principal: "human".to_owned(),
        scopes: vec!["observe@global".to_owned()],
        issued_at: chrono::Utc::now(),
        expires_at: None,
        generation: 1,
        registry_instance: None,
        subject: None,
        managed: None,
    });
    let actor = audit_actor(&quic);
    assert_eq!(actor.credential.as_deref(), Some("sha256:ab"));
    assert_eq!(actor.principal.as_deref(), Some("human"));
    assert_eq!(actor.transport, Transport::Quic);
}

/// A pinned vector the control plane checks its verifier against: a fixed
/// record signed with the key whose seed is 32 bytes of 0x07.
#[test]
fn canonical_hash_and_signature_match_the_pinned_vector() {
    let key = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let mut record = AuditRecord {
        v: 1,
        seq: 0,
        ts: "2026-11-09T12:00:00.000Z".to_owned(),
        host: "mini".to_owned(),
        kind: Kind::CommandRefused,
        actor: AuditActor {
            credential: Some("sha256:ab".to_owned()),
            principal: Some("automation".to_owned()),
            subject: None,
            on_behalf_of: None,
            transport: Transport::Quic,
        },
        action: "KILL_RESOURCE".to_owned(),
        target: Some("terminal:3".to_owned()),
        outcome: Outcome::Denied,
        detail: serde_json::json!({ "client": 12, "b": "é\"\n", "a": [true, null] }),
        prev: GENESIS.to_owned(),
        hash: String::new(),
        sig: String::new(),
    };
    assert_eq!(
        String::from_utf8(record.canonical()).unwrap(),
        concat!(
            r#"{"action":"KILL_RESOURCE","actor":{"credential":"sha256:ab","#,
            r#""on_behalf_of":null,"principal":"automation","subject":null,"#,
            r#""transport":"quic"},"detail":{"a":[true,null],"b":"é\"\n","client":12},"#,
            r#""host":"mini","kind":"command.refused","outcome":"denied","#,
            r#""prev":"0000000000000000000000000000000000000000000000000000000000000000","#,
            r#""seq":0,"target":"terminal:3","ts":"2026-11-09T12:00:00.000Z","v":1}"#,
        )
    );
    let digest = record.digest();
    record.hash = hex::encode(digest);
    record.sig = base64::engine::general_purpose::STANDARD.encode(key.sign(&digest).as_ref());
    assert_eq!(
        hex::encode(key.public_key().as_ref()),
        "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c"
    );
    assert_eq!(
        record.hash,
        "3e1e607a864291621bf521ee21c59661e36965e8aabb0d60c13276a007e1514d"
    );
    assert_eq!(
        record.sig,
        "JUDfywXAqBgW4Tz0V+4Umn1xfyNjWSWGYyrQuoPsIDpdwOgN1ieGSpaKxsKEvI3E1qWhGvQl6VFWBnztMUrvCg=="
    );
    // hash and sig never feed the hash.
    assert_eq!(record.digest(), digest);
}
