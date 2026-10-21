//! Agent coordination: mail between agents and claims on paths.
//!
//! Both live in the work store beside the objectives (ADR-0187), so one
//! keeper is their only writer and one file holds who asked whom for what.
//!
//! **Addresses.** An agent is addressed by the pane it runs in, `@749`,
//! optionally led by a display label, `pi@749`; the label is kept for display
//! and the pane is the key, so a renamed agent keeps its mail. Something that
//! runs in no pane (CI, a person at a shell) uses a bare name such as
//! `phall`, which is its own key.
//!
//! **Mail** is grouped in threads named by a short slug. A message has one
//! sender and one or more recipients; each recipient reads and acknowledges
//! it separately. Sending is idempotent by operation id.
//!
//! **Claims** reserve absolute paths. A claim covers its path and everything
//! under it, so `/repo` and `/repo/src` overlap. Shared claims coexist;
//! an exclusive claim excludes every other holder. A claim expires at its
//! deadline. Each atomic acquisition receives one fence; renewing or
//! releasing requires that exact fence and the holder. Taking again never
//! adopts an existing lease, even from the same pane.

use rusqlite::{OptionalExtension as _, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::id::{OpaqueId, OperationId};
use crate::store::{Store, digest, timestamp};

mod claims;
pub use claims::{
    Claim, ClaimFilter, ClaimOutcome, MAX_CLAIM_SECS, ReleaseClaim, RenewClaim, TakeClaim,
};

/// Tables this module owns. Additive: they do not change `user_version`, so
/// a binary that predates them still opens the store and ignores them.
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS mail (
    id BLOB NOT NULL PRIMARY KEY CHECK (length(id) = 16),
    thread TEXT NOT NULL,
    sender TEXT NOT NULL,
    sender_key TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS mail_thread ON mail (thread);
CREATE TABLE IF NOT EXISTS mail_delivery (
    mail_id BLOB NOT NULL REFERENCES mail(id),
    recipient TEXT NOT NULL,
    recipient_key TEXT NOT NULL,
    read_at TEXT,
    acked_at TEXT,
    PRIMARY KEY (mail_id, recipient_key)
);
CREATE INDEX IF NOT EXISTS mail_delivery_recipient ON mail_delivery (recipient_key, read_at);
CREATE TABLE IF NOT EXISTS claim (
    path TEXT NOT NULL,
    holder TEXT NOT NULL,
    holder_key TEXT NOT NULL,
    exclusive INTEGER NOT NULL CHECK (exclusive IN (0, 1)),
    fence INTEGER NOT NULL CHECK (fence >= 1),
    note TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (path, holder_key)
);
CREATE TABLE IF NOT EXISTS claim_fence (
    id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
    next INTEGER NOT NULL CHECK (next >= 1)
);
";

/// Longest address, in bytes.
const MAX_ADDRESS_BYTES: usize = 128;
/// Longest thread slug, in bytes.
const MAX_THREAD_BYTES: usize = 64;
/// Longest message body, in bytes.
const MAX_MAIL_BODY_BYTES: usize = 64 * 1024;
/// Most recipients one message names.
const MAX_RECIPIENTS: usize = 32;
/// Most messages one read returns.
const MAX_READ: u32 = 500;

/// Who sent or receives mail, or holds a claim: `label@N`, `@N`, or a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    display: String,
    key: String,
}

impl Address {
    /// Parse `pi@749`, `@749`, or `phall`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidText`] when the text is empty, too long, has a
    /// character outside `[A-Za-z0-9._-]` in the label, or a pane part that
    /// is not a number.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let text = text.trim();
        let invalid = |reason| Error::invalid_text("address", reason);
        if text.is_empty() || text.len() > MAX_ADDRESS_BYTES {
            return Err(invalid("must be 1 to 128 bytes"));
        }
        let (label, pane) = match text.rsplit_once('@') {
            Some((label, pane)) => (label, Some(pane)),
            None => (text, None),
        };
        if !label.chars().all(name_char) {
            return Err(invalid("a name may use letters, digits, `.`, `_` and `-`"));
        }
        let key = match pane {
            Some(pane) if !pane.is_empty() && pane.bytes().all(|byte| byte.is_ascii_digit()) => {
                format!("@{}", pane.trim_start_matches('0'))
            }
            Some(_) => return Err(invalid("the part after `@` must be a pane number")),
            None if label.is_empty() => return Err(invalid("is empty")),
            None => label.to_owned(),
        };
        Ok(Self {
            display: text.to_owned(),
            key,
        })
    }

    /// As written, such as `pi@749`.
    #[must_use]
    pub fn display(&self) -> &str {
        &self.display
    }

    /// What it is matched by: the pane, `@749`, or the bare name.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }
}

const fn name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')
}

fn parse_thread(text: &str) -> Result<String, Error> {
    let text = text.trim();
    if text.is_empty() || text.len() > MAX_THREAD_BYTES || !text.chars().all(name_char) {
        return Err(Error::invalid_text(
            "thread",
            "must be 1 to 64 letters, digits, `.`, `_` or `-`",
        ));
    }
    Ok(text.to_owned())
}

fn parse_mail_body(text: &str) -> Result<String, Error> {
    if text.trim().is_empty() {
        return Err(Error::invalid_text("message", "is empty"));
    }
    if text.len() > MAX_MAIL_BODY_BYTES || text.bytes().any(|byte| byte == 0) {
        return Err(Error::invalid_text(
            "message",
            "is over 64 KiB or contains a NUL byte",
        ));
    }
    Ok(text.to_owned())
}

/// `send` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendMail {
    /// Thread slug.
    pub thread: String,
    /// Sender address.
    pub from: String,
    /// Recipient addresses.
    pub to: Vec<String>,
    /// Message text.
    pub body: String,
    /// Repeating this id with the same body returns the first result.
    pub operation_id: OperationId,
}

/// One message as one recipient sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mail {
    /// Opaque id.
    pub id: OpaqueId,
    /// Thread slug.
    pub thread: String,
    /// Sender, as written.
    pub from: String,
    /// Every recipient, as written.
    pub to: Vec<String>,
    /// Message text.
    pub body: String,
    /// RFC 3339 send time.
    pub created_at: String,
    /// When this reader first read it; `None` while unread. Always `None`
    /// on a message the reader sent.
    pub read_at: Option<String>,
    /// When this reader acknowledged it.
    pub acked_at: Option<String>,
}

/// The result of a send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sent {
    /// The stored message.
    pub mail: Mail,
    /// True when the operation id was already on disk and nothing changed.
    pub idempotent_replay: bool,
}

/// What a read returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadMail {
    /// Whose mail.
    pub reader: String,
    /// Only this thread, including what the reader sent in it.
    pub thread: Option<String>,
    /// Include messages already read.
    pub all: bool,
    /// Mark what is returned as read.
    pub mark_read: bool,
    /// At most this many, newest kept. Zero means the maximum.
    pub limit: u32,
}

/// Acknowledge one message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckMail {
    /// Whose acknowledgement.
    pub reader: String,
    /// Which message.
    pub id: OpaqueId,
}

impl Store {
    /// Store a message for every recipient.
    ///
    /// # Errors
    ///
    /// Validation and store errors, and [`Error::OperationConflict`] when
    /// the operation id was used for a different message.
    pub fn send_mail(&mut self, request: &SendMail) -> Result<Sent, Error> {
        let thread = parse_thread(&request.thread)?;
        let from = Address::parse(&request.from)?;
        let body = parse_mail_body(&request.body)?;
        if request.to.is_empty() || request.to.len() > MAX_RECIPIENTS {
            return Err(Error::invalid_text("recipients", "name 1 to 32 recipients"));
        }
        let mut to: Vec<Address> = Vec::with_capacity(request.to.len());
        for text in &request.to {
            let address = Address::parse(text)?;
            if !to.iter().any(|seen| seen.key == address.key) {
                to.push(address);
            }
        }
        let digest = digest(
            "mail_send",
            &serde_json::json!({
                "body": &body,
                "from": from.display(),
                "thread": &thread,
                "to": to.iter().map(Address::display).collect::<Vec<_>>(),
            }),
        )?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(mail) = replayed::<Mail>(&tx, &request.operation_id, &digest)? {
            return Ok(Sent {
                mail,
                idempotent_replay: true,
            });
        }
        let now = timestamp();
        let id = OpaqueId::generate()?;
        tx.execute(
            "INSERT INTO mail (id, thread, sender, sender_key, body, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id.as_slice(),
                &thread,
                from.display(),
                from.key(),
                &body,
                &now
            ],
        )?;
        for address in &to {
            tx.execute(
                "INSERT INTO mail_delivery (mail_id, recipient, recipient_key) VALUES (?1, ?2, ?3)",
                params![id.as_slice(), address.display(), address.key()],
            )?;
        }
        let mail = Mail {
            id,
            thread,
            from: from.display().to_owned(),
            to: to
                .iter()
                .map(|address| address.display().to_owned())
                .collect(),
            body,
            created_at: now.clone(),
            read_at: None,
            acked_at: None,
        };
        record(
            &tx,
            &request.operation_id,
            "mail_send",
            &digest,
            from.display(),
            "mail_sent",
            &mail,
            &now,
        )?;
        tx.commit()?;
        Ok(Sent {
            mail,
            idempotent_replay: false,
        })
    }

    /// A reader's mail, oldest first: what was sent to it, and in a thread
    /// also what it sent there.
    ///
    /// # Errors
    ///
    /// Validation and store errors.
    pub fn read_mail(&mut self, request: &ReadMail) -> Result<Vec<Mail>, Error> {
        let reader = Address::parse(&request.reader)?;
        let thread = request.thread.as_deref().map(parse_thread).transpose()?;
        let limit = if request.limit == 0 {
            MAX_READ
        } else {
            request.limit.min(MAX_READ)
        };
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = tx.prepare(
            "SELECT m.id, m.thread, m.sender, m.body, m.created_at, d.read_at, d.acked_at
             FROM mail m
             LEFT JOIN mail_delivery d ON d.mail_id = m.id AND d.recipient_key = ?1
             WHERE (d.recipient_key IS NOT NULL OR (?2 IS NOT NULL AND m.sender_key = ?1))
               AND (?2 IS NULL OR m.thread = ?2)
               AND (?3 OR d.recipient_key IS NULL OR d.read_at IS NULL)
             ORDER BY m.rowid DESC
             LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![reader.key(), thread, request.all, i64::from(limit)],
            |row| {
                Ok(Mail {
                    id: OpaqueId::from_slice(&row.get::<_, Vec<u8>>(0)?).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Blob,
                            Box::new(err),
                        )
                    })?,
                    thread: row.get(1)?,
                    from: row.get(2)?,
                    to: Vec::new(),
                    body: row.get(3)?,
                    created_at: row.get(4)?,
                    read_at: row.get(5)?,
                    acked_at: row.get(6)?,
                })
            },
        )?;
        let mut mail = rows.collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        mail.reverse();
        let now = timestamp();
        for message in &mut mail {
            message.to = recipients(&tx, message.id)?;
            if request.mark_read && message.read_at.is_none() {
                let marked = tx.execute(
                    "UPDATE mail_delivery SET read_at = ?3
                     WHERE mail_id = ?1 AND recipient_key = ?2 AND read_at IS NULL",
                    params![message.id.as_slice(), reader.key(), &now],
                )?;
                if marked > 0 {
                    message.read_at = Some(now.clone());
                }
            }
        }
        tx.commit()?;
        Ok(mail)
    }

    /// Acknowledge one message, reading it too. Repeating is a no-op.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when the message was not sent to this reader.
    pub fn ack_mail(&mut self, request: &AckMail) -> Result<Mail, Error> {
        let reader = Address::parse(&request.reader)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = timestamp();
        let changed = tx.execute(
            "UPDATE mail_delivery
             SET read_at = COALESCE(read_at, ?3), acked_at = COALESCE(acked_at, ?3)
             WHERE mail_id = ?1 AND recipient_key = ?2",
            params![request.id.as_slice(), reader.key(), &now],
        )?;
        if changed == 0 {
            return Err(Error::NoMail {
                id: request.id.to_hex(),
                reader: reader.display().to_owned(),
            });
        }
        let mail = tx.query_row(
            "SELECT m.thread, m.sender, m.body, m.created_at, d.read_at, d.acked_at
             FROM mail m JOIN mail_delivery d ON d.mail_id = m.id
             WHERE m.id = ?1 AND d.recipient_key = ?2",
            params![request.id.as_slice(), reader.key()],
            |row| {
                Ok(Mail {
                    id: request.id,
                    thread: row.get(0)?,
                    from: row.get(1)?,
                    to: Vec::new(),
                    body: row.get(2)?,
                    created_at: row.get(3)?,
                    read_at: row.get(4)?,
                    acked_at: row.get(5)?,
                })
            },
        )?;
        let mail = Mail {
            to: recipients(&tx, request.id)?,
            ..mail
        };
        tx.commit()?;
        Ok(mail)
    }
}

fn recipients(tx: &Transaction<'_>, id: OpaqueId) -> Result<Vec<String>, Error> {
    let mut statement =
        tx.prepare("SELECT recipient FROM mail_delivery WHERE mail_id = ?1 ORDER BY rowid")?;
    let rows = statement.query_map(params![id.as_slice()], |row| row.get(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
}

fn replayed<T: serde::de::DeserializeOwned>(
    tx: &Transaction<'_>,
    operation_id: &OperationId,
    digest: &str,
) -> Result<Option<T>, Error> {
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT digest, result_json FROM operation WHERE id = ?1",
            params![operation_id.as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some((stored, result)) if stored == digest => Ok(Some(serde_json::from_str(&result)?)),
        Some(_) => Err(Error::OperationConflict {
            operation_id: operation_id.to_hex(),
        }),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one operation row and one event row, written together"
)]
fn record(
    tx: &Transaction<'_>,
    operation_id: &OperationId,
    kind: &str,
    digest: &str,
    actor: &str,
    event_kind: &str,
    result: &impl Serialize,
    now: &str,
) -> Result<(), Error> {
    let json = serde_json::to_string(result)?;
    tx.execute(
        "INSERT INTO operation (id, kind, digest, objective_id, result_json, created_at)
         VALUES (?1, ?2, ?3, NULL, ?4, ?5)",
        params![operation_id.as_slice(), kind, digest, &json, now],
    )?;
    tx.execute(
        "INSERT INTO event (ts, actor, objective_id, operation_id, kind, json)
         VALUES (?1, ?2, NULL, ?3, ?4, ?5)",
        params![now, actor, operation_id.as_slice(), event_kind, &json],
    )?;
    Ok(())
}

/// Claim lifecycle changes are conditioned on their acquisition fence, not
/// replayed by a client operation id. Each committed audit event gets its own id.
fn append_event(
    tx: &Transaction<'_>,
    actor: &str,
    kind: &str,
    body: &impl Serialize,
) -> Result<(), Error> {
    let operation = OperationId::generate()?;
    tx.execute(
        "INSERT INTO event (ts, actor, objective_id, operation_id, kind, json)
         VALUES (?1, ?2, NULL, ?3, ?4, ?5)",
        params![
            timestamp(),
            actor,
            operation.as_slice(),
            kind,
            serde_json::to_string(body)?
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn a_pane_is_the_address_and_its_label_is_display() {
        let pi = Address::parse("pi@749").unwrap();
        assert_eq!((pi.display(), pi.key()), ("pi@749", "@749"));
        assert_eq!(Address::parse("@0749").unwrap().key(), "@749");
        assert_eq!(Address::parse("phall").unwrap().key(), "phall");
        for bad in ["", "@", "pi@x", "a b", "pi@749@1", "x\u{0}"] {
            assert!(Address::parse(bad).is_err(), "{bad:?}");
        }
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
    fn an_older_binary_still_opens_a_store_with_coordination_tables() {
        let (dir, store) = store();
        assert_eq!(store.user_version().unwrap(), 2);
        drop(store);
        let conn =
            rusqlite::Connection::open(dir.path().join("work").join("work-v1.sqlite")).unwrap();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND (name LIKE 'mail%' OR name LIKE 'claim%')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(tables.len(), 4, "{tables:?}");
    }
}
