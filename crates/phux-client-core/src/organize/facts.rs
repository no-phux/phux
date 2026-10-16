//! What the planner reads: sessions and terminals as metadata, and which
//! terminals a run may move.
//!
//! Evidence is metadata only, and [`scrub`] strips URL credentials from
//! anything that reaches a judge.

use phux_protocol::ids::{ResourceKey, SessionKey};
use serde_json::{Value, json};

/// One session as the planner sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFacts {
    /// Its record key.
    pub key: SessionKey,
    /// Its name.
    pub name: String,
    /// The designated Intake session: never a destination.
    pub intake: bool,
    /// Its project identity (`project.id`), when it carries one.
    pub project_id: Option<String>,
    /// Its project display name.
    pub project_name: Option<String>,
    /// Its project checkout root.
    pub checkout: Option<String>,
}

/// One terminal as the planner sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneFacts {
    /// Its record key.
    pub key: ResourceKey,
    /// How a person addresses it (`@3`, else `^ab23cd`).
    pub label: String,
    /// The session it belongs to now.
    pub session: SessionKey,
    /// Its name or title.
    pub title: Option<String>,
    /// Its working directory.
    pub cwd: Option<String>,
    /// The agent running there and its state (`claude blocked`).
    pub agent: Option<String>,
    /// Its state in words (`running`, `failed (exit 1)`).
    pub status: String,
    /// The project its working directory is in.
    pub project_id: Option<String>,
    /// That project's display name.
    pub project_name: Option<String>,
    /// That project's checkout root.
    pub checkout: Option<String>,
}

impl PaneFacts {
    /// The evidence one Jev question carries about this terminal.
    pub(super) fn evidence(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert("terminal".into(), json!(scrub(&self.label)));
        insert_some(&mut map, "title", self.title.as_deref());
        insert_some(&mut map, "working_directory", self.cwd.as_deref());
        insert_some(&mut map, "agent", self.agent.as_deref());
        insert_some(&mut map, "project", self.project_name.as_deref());
        insert_some(&mut map, "repository", self.project_id.as_deref());
        map.insert("status".into(), json!(self.status));
        Value::Object(map)
    }

    /// A one-line summary for a session's option description.
    pub(super) fn summary(&self) -> String {
        let what = self
            .agent
            .as_deref()
            .or(self.title.as_deref())
            .unwrap_or("shell");
        let summary = self
            .cwd
            .as_deref()
            .map_or_else(|| what.to_owned(), |cwd| format!("{what} in {cwd}"));
        scrub(&summary)
    }
}

pub(super) fn insert_some(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<&str>,
) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        map.insert(key.to_owned(), json!(scrub(value)));
    }
}

/// `text` with the user-info of every URL in it (`scheme://user:pass@`)
/// replaced, so a title holding a command line does not carry its
/// credentials to the judge.
#[must_use]
pub fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let authority_end = tail.find(['/', ' ', '\t', '"', '\'']).unwrap_or(tail.len());
        match tail[..authority_end].rfind('@') {
            Some(user_end) => {
                out.push_str("***");
                rest = &tail[user_end..];
            }
            None => rest = tail,
        }
    }
    out.push_str(rest);
    out
}

/// Which terminals the planner may move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every terminal on the server.
    All,
    /// Only this session's members; destinations are still every session.
    Session(SessionKey),
}

impl Scope {
    pub(super) fn covers(self, pane: &PaneFacts) -> bool {
        match self {
            Self::All => true,
            Self::Session(key) => pane.session == key,
        }
    }
}

/// The terminals `scope` covers, in input order.
#[must_use]
pub fn in_scope(panes: &[PaneFacts], scope: Scope) -> Vec<&PaneFacts> {
    panes.iter().filter(|pane| scope.covers(pane)).collect()
}

/// What one organize run read: every session and every terminal with a
/// record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// Every session.
    pub sessions: Vec<SessionFacts>,
    /// Every terminal with a record.
    pub panes: Vec<PaneFacts>,
}

impl Facts {
    /// The session named `name`.
    #[must_use]
    pub fn session_named(&self, name: &str) -> Option<&SessionFacts> {
        self.sessions.iter().find(|session| session.name == name)
    }

    /// The scope `session` names: every terminal for `None`, else that
    /// session's; `None` when no session has that name.
    #[must_use]
    pub fn scope(&self, session: Option<&str>) -> Option<Scope> {
        session.map_or(Some(Scope::All), |name| {
            self.session_named(name).map(|s| Scope::Session(s.key))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_in_urls_are_scrubbed() {
        assert_eq!(
            scrub("psql postgres://me:hunter2@db/app && curl https://x.io/a@b"),
            "psql postgres://***@db/app && curl https://x.io/a@b"
        );
        assert_eq!(scrub("no urls here"), "no urls here");
        assert_eq!(scrub("git@github.com:a/b"), "git@github.com:a/b");
    }
}
