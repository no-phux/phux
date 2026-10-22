#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use super::*;

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("work").join("work-v1.sqlite")).unwrap();
    (dir, store)
}

fn send(store: &mut Store, from: &str, to: &[&str], thread: &str, body: &str) -> Sent {
    store
        .send_mail(&SendMail {
            thread: thread.into(),
            from: from.into(),
            to: to.iter().map(|to| (*to).to_owned()).collect(),
            body: body.into(),
            operation_id: OperationId::generate().unwrap(),
        })
        .unwrap()
}

fn read(store: &mut Store, reader: &str, thread: Option<&str>, all: bool) -> Vec<Mail> {
    store
        .read_mail(&ReadMail {
            reader: reader.into(),
            thread: thread.map(str::to_owned),
            all,
            mark_read: true,
            limit: 0,
        })
        .unwrap()
}

fn take(store: &mut Store, reader: &str, limit: u32) -> Vec<Mail> {
    store
        .take_mail(&WaitMail {
            reader: reader.into(),
            timeout_ms: 0,
            limit,
        })
        .unwrap()
}

#[test]
fn mail_reaches_the_pane_whatever_label_the_sender_used() {
    let (_dir, mut store) = store();
    let sent = send(
        &mut store,
        "opencode@730",
        &["pi@749", "phall"],
        "desktop-ux",
        "landed",
    );
    assert_eq!(sent.mail.to, ["pi@749", "phall"]);
    let inbox = read(&mut store, "@749", None, false);
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].from, "opencode@730");
    assert!(inbox[0].read_at.is_some(), "reading marks it read");
    assert!(
        read(&mut store, "pi@749", None, false).is_empty(),
        "read once"
    );
    assert_eq!(read(&mut store, "pi@749", None, true).len(), 1);
    assert_eq!(
        read(&mut store, "phall", None, false).len(),
        1,
        "each reader separately"
    );
    assert!(
        read(&mut store, "@730", None, false).is_empty(),
        "a sender's inbox is empty"
    );
    assert_eq!(
        read(&mut store, "@730", Some("desktop-ux"), true).len(),
        1,
        "a thread shows the sender its own message"
    );
}

#[test]
fn acknowledging_reads_and_is_scoped_to_the_recipient() {
    let (_dir, mut store) = store();
    let id = send(&mut store, "a", &["b"], "t", "hi").mail.id;
    let acked = store
        .ack_mail(&AckMail {
            reader: "b".into(),
            id,
        })
        .unwrap();
    assert!(acked.acked_at.is_some() && acked.read_at.is_some());
    assert!(matches!(
        store.ack_mail(&AckMail {
            reader: "c".into(),
            id
        }),
        Err(Error::NoMail { .. })
    ));
}

#[test]
fn a_repeated_send_is_one_message_and_a_changed_one_conflicts() {
    let (_dir, mut store) = store();
    let request = SendMail {
        thread: "t".into(),
        from: "a".into(),
        to: vec!["b".into()],
        body: "once".into(),
        operation_id: OperationId::generate().unwrap(),
    };
    let first = store.send_mail(&request).unwrap();
    let again = store.send_mail(&request).unwrap();
    assert!(again.idempotent_replay);
    assert_eq!(again.mail, first.mail);
    assert_eq!(read(&mut store, "b", None, true).len(), 1);
    let changed = SendMail {
        body: "twice".into(),
        ..request
    };
    assert!(matches!(
        store.send_mail(&changed),
        Err(Error::OperationConflict { .. })
    ));
}

#[test]
fn a_message_is_taken_once_per_recipient_and_stays_unread() {
    let (_dir, mut store) = store();
    let sent = send(&mut store, "pi@749", &["@730", "phall"], "t", "hello");
    let taken = take(&mut store, "opencode@730", 0);
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].id, sent.mail.id);
    assert_eq!(taken[0].to, ["@730", "phall"]);
    assert!(take(&mut store, "@730", 0).is_empty(), "at most once");
    assert_eq!(take(&mut store, "phall", 0).len(), 1, "each recipient");
    let inbox = read(&mut store, "@730", None, false);
    assert_eq!(inbox.len(), 1, "taking leaves read state alone");
}

#[test]
fn mail_already_read_is_never_taken() {
    let (_dir, mut store) = store();
    send(&mut store, "a", &["b"], "t", "seen");
    read(&mut store, "b", None, false);
    assert!(take(&mut store, "b", 0).is_empty());
}

#[test]
fn a_take_is_oldest_first_and_bounded_by_count_and_bytes() {
    let (_dir, mut store) = store();
    for index in 0..20 {
        send(&mut store, "a", &["b"], "t", &format!("m{index}"));
    }
    let first = take(&mut store, "b", 3);
    let bodies: Vec<_> = first.iter().map(|mail| mail.body.as_str()).collect();
    assert_eq!(bodies, ["m0", "m1", "m2"]);
    assert_eq!(take(&mut store, "b", 0).len(), 16, "the maximum");
    assert_eq!(take(&mut store, "b", 0).len(), 1, "the rest");

    let big = "x".repeat(60 * 1024);
    for _ in 0..4 {
        send(&mut store, "a", &["c"], "t", &big);
    }
    assert_eq!(take(&mut store, "c", 0).len(), 2, "128 KiB of bodies");
    assert_eq!(take(&mut store, "c", 0).len(), 2);
}

#[test]
fn a_wait_longer_than_an_hour_is_refused() {
    let (_dir, mut store) = store();
    let request = WaitMail {
        reader: "b".into(),
        timeout_ms: MAX_WAIT_MS + 1,
        limit: 0,
    };
    assert!(matches!(
        store.take_mail(&request),
        Err(Error::InvalidText { .. })
    ));
}

#[test]
fn a_released_take_is_taken_again_and_only_the_takers_rows_are_touched() {
    let (_dir, mut store) = store();
    let id = send(&mut store, "a", &["@730", "@731"], "t", "hello")
        .mail
        .id;
    assert_eq!(take(&mut store, "@730", 0).len(), 1);
    assert_eq!(take(&mut store, "@731", 0).len(), 1);
    let release = |reader: &str| ReleaseMail {
        reader: reader.into(),
        ids: vec![id],
    };
    assert_eq!(store.release_mail(&release("pi@730")).unwrap(), 1);
    assert_eq!(store.release_mail(&release("@730")).unwrap(), 0, "once");
    assert_eq!(take(&mut store, "@730", 0).len(), 1, "taken again");
    assert!(
        take(&mut store, "@731", 0).is_empty(),
        "another reader's take stands"
    );
}
