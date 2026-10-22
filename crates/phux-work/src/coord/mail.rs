//! Mail between agents: send, read, acknowledge, and take for delivery.
//!
//! Reading and acknowledging are the recipient's own bookkeeping. Taking
//! is the harness's: [`Store::take_mail`] hands each unread message to one
//! caller once, marking it injected so it reaches an agent's conversation at
//! most once. A message injected but never read stays unread in the inbox.

use rusqlite::{OptionalExtension as _, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use super::{Address, name_char};
use crate::error::Error;
use crate::id::{OpaqueId, OperationId};
use crate::store::{Store, digest, timestamp};

/// Longest thread slug, in bytes.
const MAX_THREAD_BYTES: usize = 64;
/// Longest message body, in bytes.
const MAX_MAIL_BODY_BYTES: usize = 64 * 1024;
/// Most recipients one message names.
const MAX_RECIPIENTS: usize = 32;
/// Most messages one read returns.
const MAX_READ: u32 = 500;
/// Most messages one take returns.
const MAX_TAKE: u32 = 16;
/// Body bytes one take returns, past its first message. Keeps the reply,
/// even fully JSON-escaped, under the keeper's line limit.
const TAKE_BODY_BUDGET: usize = 128 * 1024;
/// Longest a wait may hold the keeper connection, in milliseconds.
pub const MAX_WAIT_MS: u64 = 60 * 60 * 1000;

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

/// Take a reader's undelivered mail, waiting up to `timeout_ms` for some.
///
/// Undelivered means neither read nor taken before. What is returned is
/// marked taken in the same transaction, so concurrent takers for one
/// reader never receive the same message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitMail {
    /// Whose mail.
    pub reader: String,
    /// How long the keeper holds the request open when nothing is waiting.
    /// Zero answers at once. At most [`MAX_WAIT_MS`].
    pub timeout_ms: u64,
    /// At most this many, oldest first. Zero means the maximum, 16.
    pub limit: u32,
}

/// Return taken messages to the reader's undelivered mail, because they
/// never reached the agent's conversation: the reply to the taker was lost,
/// or the harness could not inject them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseMail {
    /// Whose mail.
    pub reader: String,
    /// The taken messages. Ids not taken by this reader are ignored.
    pub ids: Vec<OpaqueId>,
}

impl WaitMail {
    /// The reader, checked, and the bounded limit.
    fn checked(&self) -> Result<(Address, u32), Error> {
        if self.timeout_ms > MAX_WAIT_MS {
            return Err(Error::invalid_text(
                "timeout",
                "must be at most 3600 seconds",
            ));
        }
        let limit = if self.limit == 0 {
            MAX_TAKE
        } else {
            self.limit.min(MAX_TAKE)
        };
        Ok((Address::parse(&self.reader)?, limit))
    }
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
        let to = recipient_addresses(&request.to)?;
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
        let operation = Recorded {
            operation_id: &request.operation_id,
            digest: &digest,
            actor: from.display(),
            now: &now,
        };
        record(&tx, &operation, &mail)?;
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
            mail_row,
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
    /// [`Error::NoMail`] when the message was not sent to this reader.
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
            "SELECT m.id, m.thread, m.sender, m.body, m.created_at, d.read_at, d.acked_at
             FROM mail m JOIN mail_delivery d ON d.mail_id = m.id
             WHERE m.id = ?1 AND d.recipient_key = ?2",
            params![request.id.as_slice(), reader.key()],
            mail_row,
        )?;
        let mail = Mail {
            to: recipients(&tx, request.id)?,
            ..mail
        };
        tx.commit()?;
        Ok(mail)
    }

    /// Take a reader's undelivered mail, oldest first, and mark it taken.
    /// Returns at once; the keeper does the waiting. Read and acknowledged
    /// state are left as they are.
    ///
    /// # Errors
    ///
    /// Validation and store errors.
    pub fn take_mail(&mut self, request: &WaitMail) -> Result<Vec<Mail>, Error> {
        let (reader, limit) = request.checked()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut mail = undelivered(&tx, &reader, limit)?;
        within_budget(&mut mail);
        let now = timestamp();
        for message in &mut mail {
            message.to = recipients(&tx, message.id)?;
            tx.execute(
                "INSERT INTO mail_injected (mail_id, recipient_key, injected_at) VALUES (?1, ?2, ?3)",
                params![message.id.as_slice(), reader.key(), &now],
            )?;
        }
        tx.commit()?;
        Ok(mail)
    }
}

impl Store {
    /// Undo [`Store::take_mail`] for `request.ids`, so a later take returns
    /// them again. Returns how many were taken and are now released.
    ///
    /// # Errors
    ///
    /// Validation and store errors.
    pub fn release_mail(&mut self, request: &ReleaseMail) -> Result<u64, Error> {
        let reader = Address::parse(&request.reader)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut released = 0;
        for id in &request.ids {
            released += tx.execute(
                "DELETE FROM mail_injected WHERE mail_id = ?1 AND recipient_key = ?2",
                params![id.as_slice(), reader.key()],
            )?;
        }
        tx.commit()?;
        Ok(u64::try_from(released).unwrap_or(u64::MAX))
    }
}

fn recipient_addresses(texts: &[String]) -> Result<Vec<Address>, Error> {
    if texts.is_empty() || texts.len() > MAX_RECIPIENTS {
        return Err(Error::invalid_text("recipients", "name 1 to 32 recipients"));
    }
    let mut to: Vec<Address> = Vec::with_capacity(texts.len());
    for text in texts {
        let address = Address::parse(text)?;
        if !to.iter().any(|seen| seen.key() == address.key()) {
            to.push(address);
        }
    }
    Ok(to)
}

fn undelivered(tx: &Transaction<'_>, reader: &Address, limit: u32) -> Result<Vec<Mail>, Error> {
    let mut statement = tx.prepare(
        "SELECT m.id, m.thread, m.sender, m.body, m.created_at, d.read_at, d.acked_at
         FROM mail_delivery d
         JOIN mail m ON m.id = d.mail_id
         LEFT JOIN mail_injected i ON i.mail_id = d.mail_id AND i.recipient_key = d.recipient_key
         WHERE d.recipient_key = ?1 AND d.read_at IS NULL AND i.mail_id IS NULL
         ORDER BY m.rowid
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![reader.key(), i64::from(limit)], mail_row)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
}

/// Keep the oldest messages whose bodies fit [`TAKE_BODY_BUDGET`], and
/// always the first, so one large message never blocks the queue.
fn within_budget(mail: &mut Vec<Mail>) {
    let mut spent = 0;
    let keep = mail
        .iter()
        .enumerate()
        .take_while(|(index, message)| {
            spent += message.body.len();
            *index == 0 || spent <= TAKE_BODY_BUDGET
        })
        .count();
    mail.truncate(keep);
}

fn mail_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Mail> {
    let id = OpaqueId::from_slice(&row.get::<_, Vec<u8>>(0)?).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(err))
    })?;
    Ok(Mail {
        id,
        thread: row.get(1)?,
        from: row.get(2)?,
        to: Vec::new(),
        body: row.get(3)?,
        created_at: row.get(4)?,
        read_at: row.get(5)?,
        acked_at: row.get(6)?,
    })
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

/// The operation a send records beside its event.
struct Recorded<'a> {
    operation_id: &'a OperationId,
    digest: &'a str,
    actor: &'a str,
    now: &'a str,
}

fn record(
    tx: &Transaction<'_>,
    operation: &Recorded<'_>,
    result: &impl Serialize,
) -> Result<(), Error> {
    let json = serde_json::to_string(result)?;
    tx.execute(
        "INSERT INTO operation (id, kind, digest, objective_id, result_json, created_at)
         VALUES (?1, 'mail_send', ?2, NULL, ?3, ?4)",
        params![
            operation.operation_id.as_slice(),
            operation.digest,
            &json,
            operation.now
        ],
    )?;
    tx.execute(
        "INSERT INTO event (ts, actor, objective_id, operation_id, kind, json)
         VALUES (?1, ?2, NULL, ?3, 'mail_sent', ?4)",
        params![
            operation.now,
            operation.actor,
            operation.operation_id.as_slice(),
            &json
        ],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "mail_tests.rs"]
mod tests;
