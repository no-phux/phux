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
//! **Hosts.** A hub's store also holds the mail and claims of its satellites
//! ([`federation`]). There an address may lead with the satellite it lives
//! on, `min-api/pi@3`, keyed `min-api/@3`; `hub/` names the hub itself and
//! is dropped from the key, so `hub/@749` is `@749`.
//!
//! **Mail** is grouped in threads named by a short slug. A message has one
//! sender and one or more recipients; each recipient reads and acknowledges
//! it separately. Sending is idempotent by operation id. A harness takes
//! each unread message once to put it in its agent's conversation.
//!
//! **Claims** reserve absolute paths. A claim covers its path and everything
//! under it, so `/repo` and `/repo/src` overlap. Shared claims coexist;
//! an exclusive claim excludes every other holder. A claim expires at its
//! deadline. Each atomic acquisition receives one fence; renewing or
//! releasing requires that exact fence and the holder. Taking again never
//! adopts an existing lease, even from the same pane.

use rusqlite::{Transaction, params};
use serde::Serialize;

use crate::error::Error;
use crate::id::OperationId;
#[cfg(test)]
use crate::store::Store;
use crate::store::timestamp;

mod claims;
pub mod federation;
mod mail;
pub use claims::{
    Claim, ClaimFilter, ClaimOutcome, MAX_CLAIM_SECS, ReleaseClaim, RenewClaim, TakeClaim,
};
pub use mail::{AckMail, MAX_WAIT_MS, Mail, ReadMail, ReleaseMail, SendMail, Sent, WaitMail};

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
CREATE TABLE IF NOT EXISTS mail_injected (
    mail_id BLOB NOT NULL,
    recipient_key TEXT NOT NULL,
    injected_at TEXT NOT NULL,
    PRIMARY KEY (mail_id, recipient_key),
    FOREIGN KEY (mail_id, recipient_key) REFERENCES mail_delivery (mail_id, recipient_key)
);
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

/// Who sent or receives mail, or holds a claim: `label@N`, `@N`, or a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    display: String,
    key: String,
}

impl Address {
    /// Parse `pi@749`, `@749`, or `phall`, optionally led by the host it
    /// lives on: `min-api/pi@3`, or `hub/@749` for the hub's own `@749`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidText`] when the text is empty, too long, has a
    /// character outside `[A-Za-z0-9._-]` in the label or the host, a pane
    /// part that is not a number, or more than one host.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let text = text.trim();
        let invalid = |reason| Error::invalid_text("address", reason);
        if text.is_empty() || text.len() > MAX_ADDRESS_BYTES {
            return Err(invalid("must be 1 to 128 bytes"));
        }
        let (host, local) = match text.split_once('/') {
            Some((host, local)) => (Some(host), local),
            None => (None, text),
        };
        if host.is_some_and(|host| host.is_empty() || !host.chars().all(name_char)) {
            return Err(invalid(
                "a host is a satellite name of letters, digits, `.`, `_` and `-`, as in `min-api/@3`",
            ));
        }
        if local.contains('/') {
            return Err(invalid(
                "an address names at most one host, as in `min-api/@3`",
            ));
        }
        let local = local_key(local).map_err(invalid)?;
        let key = match host {
            Some(host) if host != federation::HUB => format!("{host}/{local}"),
            _ => local,
        };
        Ok(Self {
            display: text.to_owned(),
            key,
        })
    }

    /// The satellite this address lives on, `None` for the hub's own.
    #[must_use]
    pub fn host(&self) -> Option<&str> {
        self.key.split_once('/').map(|(host, _)| host)
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

/// How long a claim lives unless its taker says otherwise, in seconds.
pub const DEFAULT_CLAIM_SECS: u64 = 60 * 60;

/// Overrides who a process speaks as.
pub const ADDRESS_ENV: &str = "PHUX_AGENT_ADDRESS";

/// Who this process speaks as.
///
/// `explicit`, else `$PHUX_AGENT_ADDRESS`, else its pane (`@N` from
/// `$PHUX_TERMINAL_ID`), else `$USER`, else `cli`. The one identity rule
/// every surface uses; the store validates the result.
#[must_use]
pub fn caller_address(explicit: Option<&str>) -> String {
    let env = |name| std::env::var(name).ok().filter(|value| !value.is_empty());
    explicit
        .map(str::to_owned)
        .or_else(|| env(ADDRESS_ENV))
        .or_else(|| env("PHUX_TERMINAL_ID").map(|id| pane_address(&id)))
        .or_else(|| env("USER").filter(|user| user.len() <= MAX_ADDRESS_BYTES))
        .unwrap_or_else(|| "cli".to_owned())
}

/// The address of a pane from its `PHUX_TERMINAL_ID`, `749` or `@749`.
#[must_use]
pub fn pane_address(terminal_id: &str) -> String {
    format!("@{}", terminal_id.strip_prefix('@').unwrap_or(terminal_id))
}

/// `path`, which must be absolute, with `.` and `..` resolved lexically and
/// no symlink followed: the form a claim is stored under. `None` when it is
/// relative or not UTF-8.
#[must_use]
pub fn claim_path(path: &std::path::Path) -> Option<String> {
    use std::path::Component;
    if !path.is_absolute() {
        return None;
    }
    let mut normal = std::path::PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normal.pop();
            }
            Component::Normal(part) => normal.push(part),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    normal.to_str().map(str::to_owned)
}

/// The key of an address with no host: `@749` for `pi@749`, or the name.
fn local_key(text: &str) -> Result<String, &'static str> {
    let (label, pane) = match text.rsplit_once('@') {
        Some((label, pane)) => (label, Some(pane)),
        None => (text, None),
    };
    if !label.chars().all(name_char) {
        return Err("a name may use letters, digits, `.`, `_` and `-`");
    }
    match pane {
        Some(pane) if !pane.is_empty() && pane.bytes().all(|byte| byte.is_ascii_digit()) => {
            Ok(format!("@{}", pane.trim_start_matches('0')))
        }
        Some(_) => Err("the part after `@` must be a pane number"),
        None if label.is_empty() => Err("is empty"),
        None => Ok(label.to_owned()),
    }
}

const fn name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')
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
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "tests"
    )]

    use super::*;

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
    fn claim_paths_are_resolved_lexically_and_must_be_absolute() {
        let path = |text: &str| claim_path(std::path::Path::new(text));
        assert_eq!(path("/a/./b/../c/").as_deref(), Some("/a/c"));
        assert_eq!(path("/..").as_deref(), Some("/"));
        assert_eq!(path("rel"), None);
    }

    #[test]
    fn a_pane_address_comes_from_its_terminal_id() {
        assert_eq!(pane_address("749"), "@749");
        assert_eq!(pane_address("@749"), "@749");
        assert_eq!(caller_address(Some("pi@749")), "pi@749");
    }

    #[test]
    fn a_satellite_pane_is_keyed_under_its_host_and_hub_is_this_host() {
        let boxed = Address::parse("min-api/pi@03").unwrap();
        assert_eq!(
            (boxed.display(), boxed.key()),
            ("min-api/pi@03", "min-api/@3")
        );
        assert_eq!(boxed.host(), Some("min-api"));
        let hub = Address::parse("hub/pi@749").unwrap();
        assert_eq!((hub.key(), hub.host()), ("@749", None));
        assert_eq!(Address::parse("ci/phall").unwrap().key(), "ci/phall");
        for bad in ["/@3", "a/b/@3", "a b/@3", "min-api/", "min-api/@x"] {
            assert!(Address::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn an_older_binary_still_opens_a_store_with_coordination_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("work").join("work-v1.sqlite");
        let store = Store::open(&path).unwrap();
        assert_eq!(store.user_version().unwrap(), 2);
        drop(store);
        let conn = rusqlite::Connection::open(path).unwrap();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND (name LIKE 'mail%' OR name LIKE 'claim%')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(tables.len(), 5, "{tables:?}");
    }
}
