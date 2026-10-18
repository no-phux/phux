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
//! deadline, and the holder renews it by claiming again, which keeps its
//! fence. Each new claim takes the next fence number, so a writer that held
//! a lease can prove it was not superseded.

use rusqlite::{OptionalExtension as _, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::id::{OpaqueId, OperationId};
use crate::store::{Store, digest, timestamp};

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
/// Most paths one claim names.
const MAX_CLAIM_PATHS: usize = 64;
/// Longest claimed path, in bytes.
const MAX_PATH_BYTES: usize = 4096;
/// Longest claim note, in bytes.
const MAX_NOTE_BYTES: usize = 512;
/// Longest claim, so a forgotten one cannot hold a path for days.
pub const MAX_CLAIM_SECS: u64 = 24 * 60 * 60;
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

/// An absolute, normalized path: no `.` or `..` segment, no trailing `/`.
fn parse_path(text: &str) -> Result<String, Error> {
    let invalid = |reason| Error::invalid_text("path", reason);
    if !text.starts_with('/') || text.len() > MAX_PATH_BYTES || text.bytes().any(|b| b == 0) {
        return Err(invalid("must be an absolute path under 4 KiB"));
    }
    if text == "/" {
        return Err(invalid("cannot claim the whole filesystem"));
    }
    let trimmed = text.trim_end_matches('/');
    if trimmed
        .split('/')
        .skip(1)
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(invalid(
            "must be normalized, without empty, `.` or `..` segments",
        ));
    }
    Ok(trimmed.to_owned())
}

/// Whether claims on `a` and `b` cover a common path.
fn overlaps(a: &str, b: &str) -> bool {
    let within = |inner: &str, outer: &str| {
        inner == outer
            || inner
                .strip_prefix(outer)
                .is_some_and(|rest| rest.starts_with('/'))
    };
    within(a, b) || within(b, a)
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

/// `claim` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeClaim {
    /// Who holds it.
    pub holder: String,
    /// Absolute paths. All are granted, or none.
    pub paths: Vec<String>,
    /// Exclude every other holder, rather than sharing with other shared claims.
    pub exclusive: bool,
    /// Seconds until it expires, at most [`MAX_CLAIM_SECS`].
    pub ttl_secs: u64,
    /// Why, shown to anyone it blocks.
    pub note: String,
}

/// One live claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    /// The claimed path and everything under it.
    pub path: String,
    /// Holder, as written.
    pub holder: String,
    /// Excludes other holders.
    pub exclusive: bool,
    /// Increases with every new claim; a renewal keeps it.
    pub fence: u64,
    /// Why.
    pub note: String,
    /// Unix seconds when it lapses.
    pub expires_at: u64,
}

/// The answer to a claim: all of it, or who is in the way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "claims", rename_all = "snake_case")]
pub enum ClaimOutcome {
    /// Every path is now held.
    Granted(Vec<Claim>),
    /// Nothing was taken; these claims conflict.
    Conflict(Vec<Claim>),
}

/// `release` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseClaim {
    /// Whose claims.
    pub holder: String,
    /// Exact paths it claimed. Empty releases all of the holder's claims.
    pub paths: Vec<String>,
}

/// Which claims to list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimFilter {
    /// Only claims overlapping this path.
    pub path: Option<String>,
    /// Only this holder's.
    pub holder: Option<String>,
}

fn now_secs() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp()).unwrap_or(0)
}

fn sql_secs(secs: u64) -> i64 {
    i64::try_from(secs).unwrap_or(i64::MAX)
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

    /// Take or renew claims on every path, or none if any conflicts.
    ///
    /// # Errors
    ///
    /// Validation and store errors. A conflict is an outcome, not an error.
    pub fn take_claim(&mut self, request: &TakeClaim) -> Result<ClaimOutcome, Error> {
        let holder = Address::parse(&request.holder)?;
        if request.paths.is_empty() || request.paths.len() > MAX_CLAIM_PATHS {
            return Err(Error::invalid_text("paths", "name 1 to 64 paths"));
        }
        if request.ttl_secs == 0 || request.ttl_secs > MAX_CLAIM_SECS {
            return Err(Error::invalid_text("ttl", "must be 1 second to 24 hours"));
        }
        if request.note.len() > MAX_NOTE_BYTES || request.note.contains('\0') {
            return Err(Error::invalid_text(
                "note",
                "is over 512 bytes or contains NUL",
            ));
        }
        let mut paths: Vec<String> = Vec::with_capacity(request.paths.len());
        for path in &request.paths {
            let path = parse_path(path)?;
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        let now = now_secs();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        sweep(&tx, now)?;
        let live = claims(&tx, &ClaimFilter::default())?;
        let conflicts: Vec<Claim> = live
            .into_iter()
            .filter(|claim| {
                Address::parse(&claim.holder).is_ok_and(|other| other.key != holder.key)
                    && (claim.exclusive || request.exclusive)
                    && paths.iter().any(|path| overlaps(path, &claim.path))
            })
            .collect();
        if !conflicts.is_empty() {
            return Ok(ClaimOutcome::Conflict(conflicts));
        }
        let expires = sql_secs(now.saturating_add(request.ttl_secs));
        let created = timestamp();
        let mut granted = Vec::with_capacity(paths.len());
        for path in paths {
            let existing: Option<i64> = tx
                .query_row(
                    "SELECT fence FROM claim WHERE path = ?1 AND holder_key = ?2",
                    params![&path, holder.key()],
                    |row| row.get(0),
                )
                .optional()?;
            let fence = match existing {
                Some(fence) => fence,
                None => next_fence(&tx)?,
            };
            tx.execute(
                "INSERT INTO claim (path, holder, holder_key, exclusive, fence, note, expires_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(path, holder_key) DO UPDATE SET
                    holder = excluded.holder, exclusive = excluded.exclusive,
                    note = excluded.note, expires_at = excluded.expires_at",
                params![
                    &path,
                    holder.display(),
                    holder.key(),
                    request.exclusive,
                    fence,
                    &request.note,
                    expires,
                    &created
                ],
            )?;
            granted.push(Claim {
                path,
                holder: holder.display().to_owned(),
                exclusive: request.exclusive,
                fence: u64::try_from(fence).unwrap_or(0),
                note: request.note.clone(),
                expires_at: u64::try_from(expires).unwrap_or(0),
            });
        }
        append_event(&tx, holder.display(), "claim_taken", &granted)?;
        tx.commit()?;
        Ok(ClaimOutcome::Granted(granted))
    }

    /// Release a holder's claims on exactly these paths, or all of them.
    /// Returns how many were released.
    ///
    /// # Errors
    ///
    /// Validation and store errors.
    pub fn release_claim(&mut self, request: &ReleaseClaim) -> Result<u64, Error> {
        let holder = Address::parse(&request.holder)?;
        let paths = request
            .paths
            .iter()
            .map(|path| parse_path(path))
            .collect::<Result<Vec<_>, _>>()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut released = 0;
        if paths.is_empty() {
            released += tx.execute(
                "DELETE FROM claim WHERE holder_key = ?1",
                params![holder.key()],
            )?;
        }
        for path in &paths {
            released += tx.execute(
                "DELETE FROM claim WHERE holder_key = ?1 AND path = ?2",
                params![holder.key(), path],
            )?;
        }
        if released > 0 {
            append_event(&tx, holder.display(), "claim_released", &paths)?;
        }
        tx.commit()?;
        Ok(u64::try_from(released).unwrap_or(u64::MAX))
    }

    /// Live claims, by path.
    ///
    /// # Errors
    ///
    /// Validation and store errors.
    pub fn claims(&mut self, filter: &ClaimFilter) -> Result<Vec<Claim>, Error> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        sweep(&tx, now_secs())?;
        let live = claims(&tx, filter)?;
        tx.commit()?;
        Ok(live)
    }
}

fn recipients(tx: &Transaction<'_>, id: OpaqueId) -> Result<Vec<String>, Error> {
    let mut statement =
        tx.prepare("SELECT recipient FROM mail_delivery WHERE mail_id = ?1 ORDER BY rowid")?;
    let rows = statement.query_map(params![id.as_slice()], |row| row.get(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
}

fn sweep(tx: &Transaction<'_>, now: u64) -> Result<(), Error> {
    tx.execute(
        "DELETE FROM claim WHERE expires_at <= ?1",
        params![sql_secs(now)],
    )?;
    Ok(())
}

fn next_fence(tx: &Transaction<'_>) -> Result<i64, Error> {
    let fence = tx.query_row(
        "INSERT INTO claim_fence (id, next) VALUES (1, 1)
         ON CONFLICT(id) DO UPDATE SET next = next + 1
         RETURNING next",
        [],
        |row| row.get(0),
    )?;
    Ok(fence)
}

fn claims(tx: &Transaction<'_>, filter: &ClaimFilter) -> Result<Vec<Claim>, Error> {
    let path = filter.path.as_deref().map(parse_path).transpose()?;
    let holder = filter.holder.as_deref().map(Address::parse).transpose()?;
    let mut statement = tx.prepare(
        "SELECT path, holder, holder_key, exclusive, fence, note, expires_at
         FROM claim ORDER BY path, fence",
    )?;
    let rows = statement.query_map([], |row| {
        let fence: i64 = row.get(4)?;
        let expires: i64 = row.get(6)?;
        Ok((
            row.get::<_, String>(2)?,
            Claim {
                path: row.get(0)?,
                holder: row.get(1)?,
                exclusive: row.get(3)?,
                fence: u64::try_from(fence).unwrap_or(0),
                note: row.get(5)?,
                expires_at: u64::try_from(expires).unwrap_or(0),
            },
        ))
    })?;
    let mut live = Vec::new();
    for row in rows {
        let (key, claim) = row?;
        let by_path = path
            .as_deref()
            .is_none_or(|path| overlaps(path, &claim.path));
        let by_holder = holder.as_ref().is_none_or(|holder| holder.key == key);
        if by_path && by_holder {
            live.push(claim);
        }
    }
    Ok(live)
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

/// An event with no client operation id: claims are not replayed by id,
/// because taking a claim again renews it.
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

    fn take(store: &mut Store, holder: &str, paths: &[&str], exclusive: bool) -> ClaimOutcome {
        store
            .take_claim(&TakeClaim {
                holder: holder.into(),
                paths: paths.iter().map(|path| (*path).to_owned()).collect(),
                exclusive,
                ttl_secs: 600,
                note: "test".into(),
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
    fn claims_cover_their_subtree_and_exclusive_excludes_others() {
        let (_dir, mut store) = store();
        let ClaimOutcome::Granted(first) = take(&mut store, "pi@749", &["/w/phux-int"], true)
        else {
            panic!("free path")
        };
        let ClaimOutcome::Conflict(holders) = take(&mut store, "@730", &["/w/phux-int/src"], false)
        else {
            panic!("inside an exclusive claim")
        };
        assert_eq!(holders[0].holder, "pi@749");
        assert!(
            matches!(
                take(&mut store, "@730", &["/w/phux-int-other"], true),
                ClaimOutcome::Granted(_)
            ),
            "a sibling with a shared prefix is a different path"
        );
        let ClaimOutcome::Granted(renewed) = take(&mut store, "@749", &["/w/phux-int"], true)
        else {
            panic!("the holder renews")
        };
        assert_eq!(
            renewed[0].fence, first[0].fence,
            "a renewal keeps its fence"
        );
        assert_eq!(
            store
                .release_claim(&ReleaseClaim {
                    holder: "pi@749".into(),
                    paths: Vec::new()
                })
                .unwrap(),
            1
        );
        let ClaimOutcome::Granted(next) = take(&mut store, "@730", &["/w/phux-int/src"], true)
        else {
            panic!("released")
        };
        assert!(
            next[0].fence > first[0].fence,
            "a new claim takes a new fence"
        );
    }

    #[test]
    fn shared_claims_coexist_and_a_conflict_takes_nothing() {
        let (_dir, mut store) = store();
        assert!(matches!(
            take(&mut store, "a", &["/r"], false),
            ClaimOutcome::Granted(_)
        ));
        assert!(matches!(
            take(&mut store, "b", &["/r/x"], false),
            ClaimOutcome::Granted(_)
        ));
        assert!(matches!(
            take(&mut store, "c", &["/elsewhere", "/r/y"], true),
            ClaimOutcome::Conflict(_)
        ));
        let held = store
            .claims(&ClaimFilter {
                path: Some("/elsewhere".into()),
                holder: None,
            })
            .unwrap();
        assert!(held.is_empty(), "all or nothing");
    }

    #[test]
    fn expired_claims_free_their_path() {
        let (_dir, mut store) = store();
        assert!(matches!(
            take(&mut store, "a", &["/r"], true),
            ClaimOutcome::Granted(_)
        ));
        store
            .conn
            .execute("UPDATE claim SET expires_at = 1", [])
            .unwrap();
        assert!(matches!(
            take(&mut store, "b", &["/r"], true),
            ClaimOutcome::Granted(_)
        ));
        assert_eq!(store.claims(&ClaimFilter::default()).unwrap().len(), 1);
    }

    #[test]
    fn paths_must_be_absolute_and_normalized() {
        for bad in ["relative", "/", "/a/../b", "/a//b", "/a/./b"] {
            assert!(parse_path(bad).is_err(), "{bad}");
        }
        assert_eq!(parse_path("/a/b/").unwrap(), "/a/b");
        assert!(overlaps("/a", "/a/b") && overlaps("/a/b", "/a") && !overlaps("/a", "/ab"));
    }

    #[test]
    fn an_older_binary_still_opens_a_store_with_coordination_tables() {
        let (dir, store) = store();
        assert_eq!(store.user_version().unwrap(), 1);
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
