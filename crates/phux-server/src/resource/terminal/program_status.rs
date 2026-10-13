//! Per-terminal OSC 7501 records.
//!
//! The engine validates and decodes each report (see [`super::effects`]);
//! it keeps no records, so the store, its priority order, `app`
//! inheritance, and eviction live here. Records are stored
//! oldest-to-newest, so recency and LRU eviction need no clock or counter.
//! Inherited apps are resolved only in consumer snapshots.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private terminal module shared by sibling runtime and state modules"
)]

use std::fmt::Write as _;

use base64::Engine as _;
use serde::Serialize;

/// The specification's record cap; at it, the least recently updated
/// record makes room.
const MAX_RECORDS: usize = 256;
const STATES: [&str; 5] = ["blocked", "error", "done", "working", "idle"];

/// Consumer-visible record. The root id is always the empty string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Record {
    pub(crate) id: String,
    pub(crate) state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) progress: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) msg: Option<String>,
}

/// One validated report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Update {
    /// Replace the record with this id completely.
    Put(Record),
    /// Remove this id and every record beneath it; the root id removes all.
    Clear(String),
}

/// Authoritative records for one terminal, independent of its active screen.
#[derive(Debug, Default)]
pub(crate) struct ProgramStatus {
    records: Vec<Record>,
}

impl ProgramStatus {
    /// Apply one report, atomically replacing or clearing what it addresses.
    ///
    /// False means no visible change. An identical report still refreshes
    /// recency, which can reorder equal-priority records and always protects
    /// the record from least-recently-updated eviction.
    pub(crate) fn apply(&mut self, update: Update) -> bool {
        let record = match update {
            Update::Clear(id) if id.is_empty() => return self.clear(),
            Update::Clear(id) => {
                let before = self.records.len();
                self.records.retain(|stored| {
                    stored.id != id
                        && !stored
                            .id
                            .strip_prefix(&id)
                            .is_some_and(|suffix| suffix.starts_with('/'))
                });
                return self.records.len() != before;
            }
            Update::Put(record) => record,
        };

        let changed = if let Some(index) = self.records.iter().position(|r| r.id == record.id) {
            let changed = self.records[index] != record
                || self.records[index + 1..]
                    .iter()
                    .any(|later| later.state == record.state);
            self.records.remove(index);
            changed
        } else {
            if self.records.len() == MAX_RECORDS {
                self.records.remove(0);
            }
            true
        };
        self.records.push(record);
        changed
    }

    /// Prompt start or attached-process exit expires working and blocked only.
    pub(crate) fn expire_active(&mut self) -> bool {
        let before = self.records.len();
        self.records
            .retain(|record| !matches!(record.state, "working" | "blocked"));
        self.records.len() != before
    }

    /// Whether OSC 7501 currently owns any status on this terminal.
    pub(crate) const fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Full reset clears every record; alternate-screen and soft resets do not.
    pub(crate) fn clear(&mut self) -> bool {
        let changed = !self.records.is_empty();
        self.records.clear();
        changed
    }

    /// Priority descending, then most recently updated; apps resolve at read.
    pub(crate) fn snapshot(&self) -> Vec<Record> {
        let mut records = Vec::with_capacity(self.records.len());
        for state in STATES {
            for stored in self.records.iter().rev().filter(|r| r.state == state) {
                let mut record = stored.clone();
                if record.app.is_none() {
                    record.app = self.inherited_app(&record.id).map(str::to_owned);
                }
                records.push(record);
            }
        }
        records
    }

    /// Upgrade seed: reports oldest-to-newest, with stored (not inherited) app.
    ///
    /// Writing these bytes to a fresh engine reports them back in order, so
    /// both inheritance and the exact recency order are preserved.
    pub(crate) fn replay(&self) -> Vec<u8> {
        let mut bytes = String::new();
        for record in &self.records {
            bytes.push_str("\x1b]7501;state=");
            bytes.push_str(record.state);
            if !record.id.is_empty() {
                append_pair(&mut bytes, "id", &record.id);
            }
            if let Some(kind) = record.kind {
                append_pair(&mut bytes, "kind", kind);
            }
            if let Some(progress) = record.progress {
                // String's formatter is infallible.
                let _ = write!(bytes, ":progress={progress}");
            }
            if let Some(app) = &record.app {
                append_pair(&mut bytes, "app", app);
            }
            for (key, value) in [("title", &record.title), ("msg", &record.msg)] {
                if let Some(text) = value {
                    bytes.push(':');
                    bytes.push_str(key);
                    bytes.push('=');
                    base64::engine::general_purpose::STANDARD.encode_string(text, &mut bytes);
                }
            }
            bytes.push_str("\x1b\\");
        }
        bytes.into_bytes()
    }

    fn inherited_app(&self, id: &str) -> Option<&str> {
        let mut ancestor = id;
        while !ancestor.is_empty() {
            ancestor = ancestor.rsplit_once('/').map_or("", |(parent, _)| parent);
            if let Some(app) = self
                .records
                .iter()
                .find(|record| record.id == ancestor)
                .and_then(|record| record.app.as_deref())
            {
                return Some(app);
            }
        }
        None
    }
}

fn append_pair(output: &mut String, key: &str, value: &str) {
    output.push(':');
    output.push_str(key);
    output.push('=');
    output.push_str(value);
}

#[cfg(test)]
mod tests {
    use super::super::effects::{self, Effect};
    use super::*;

    /// Apply `body` as the engine reports it from `OSC 7501 ; body BEL`.
    fn report(status: &mut ProgramStatus, body: &str) -> bool {
        restore(status, format!("\x1b]7501;{body}\x07").as_bytes())
    }

    /// Apply every report the engine sees in `bytes`.
    fn restore(status: &mut ProgramStatus, bytes: &[u8]) -> bool {
        let mut changed = false;
        for effect in effects::observe(&[bytes]).0 {
            let Effect::ProgramStatus(update) = effect else {
                panic!("reports only: {effect:?}");
            };
            changed |= status.apply(update);
        }
        changed
    }

    fn record<'a>(records: &'a [Record], id: &str) -> &'a Record {
        records.iter().find(|r| r.id == id).expect("record exists")
    }

    #[test]
    fn replacement_and_json_preserve_only_the_last_report() {
        let mut status = ProgramStatus::default();
        assert!(status.is_empty());
        assert!(report(
            &mut status,
            "state=blocked:kind=permission:progress=42:app=cargo:title=QnVpbGQ=:msg=QXBwcm92ZT8="
        ));
        let root = status.snapshot().remove(0);
        assert_eq!(root.id, "");
        assert_eq!(root.kind, Some("permission"));
        assert_eq!(root.progress, Some(42));
        assert_eq!(root.title.as_deref(), Some("Build"));
        assert_eq!(root.msg.as_deref(), Some("Approve?"));
        assert!(report(&mut status, "state=done"));
        assert_eq!(
            serde_json::to_value(status.snapshot().remove(0)).unwrap(),
            serde_json::json!({"id": "", "state": "done"})
        );
        assert!(!report(&mut status, "state=done"));
        assert!(
            !report(&mut status, "state=error:msg=YR"),
            "a rejected report leaves the set alone"
        );
        assert_eq!(status.snapshot()[0].state, "done");
    }

    #[test]
    fn nearest_ancestor_app_is_live_in_snapshots_and_never_written_to_children() {
        let mut status = ProgramStatus::default();
        report(&mut status, "state=idle:app=root");
        report(&mut status, "state=working:id=build:app=cargo");
        report(&mut status, "state=blocked:id=build/test/unit");
        report(&mut status, "state=done:id=other/leaf");
        report(&mut status, "state=done:id=isolated:app=own");
        let records = status.snapshot();
        assert_eq!(
            record(&records, "build/test/unit").app.as_deref(),
            Some("cargo")
        );
        assert_eq!(record(&records, "other/leaf").app.as_deref(), Some("root"));
        assert_eq!(record(&records, "isolated").app.as_deref(), Some("own"));
        assert_eq!(record(&status.records, "build/test/unit").app, None);
        report(&mut status, "state=working:id=build");
        assert_eq!(
            record(&status.snapshot(), "build/test/unit").app.as_deref(),
            Some("root")
        );
        report(&mut status, "state=idle:app=new-root");
        assert_eq!(
            record(&status.snapshot(), "build/test/unit").app.as_deref(),
            Some("new-root")
        );
        report(&mut status, "state=idle");
        assert_eq!(record(&status.snapshot(), "build/test/unit").app, None);
    }

    #[test]
    fn subtree_clear_obeys_path_boundaries_and_root_clear_removes_everything() {
        let mut status = ProgramStatus::default();
        for id in ["a", "a/b", "a/b/c", "a/bc", "ab", "x/a/b"] {
            report(&mut status, &format!("state=done:id={id}"));
        }
        assert!(report(&mut status, "state=clear:id=a/b"));
        let ids: Vec<_> = status.snapshot().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, ["x/a/b", "ab", "a/bc", "a"]);
        assert!(!report(&mut status, "state=clear:id=a/b"));
        assert!(report(&mut status, "state=done:id=missing/child"));
        assert!(
            report(&mut status, "state=clear:id=missing"),
            "parent need not exist"
        );
        assert!(report(&mut status, "state=clear"));
        assert!(status.snapshot().is_empty());
        assert!(!status.clear());
    }

    #[test]
    fn expiry_keeps_idle_done_error_and_can_change_inherited_apps() {
        let mut status = ProgramStatus::default();
        report(&mut status, "state=working:app=parent");
        for state in STATES {
            report(&mut status, &format!("state={state}:id={state}"));
        }
        assert!(status.expire_active());
        let records = status.snapshot();
        assert_eq!(
            records.iter().map(|r| r.state).collect::<Vec<_>>(),
            ["error", "done", "idle"]
        );
        assert!(records.iter().all(|r| r.app.is_none()));
        assert!(!status.expire_active());
        assert!(status.clear());
    }

    #[test]
    fn priority_and_identical_refresh_are_visible_only_when_order_changes() {
        let mut status = ProgramStatus::default();
        for state in STATES {
            report(&mut status, &format!("state={state}:id={state}"));
        }
        assert_eq!(
            status
                .snapshot()
                .iter()
                .map(|r| r.state)
                .collect::<Vec<_>>(),
            STATES
        );
        assert!(
            !report(&mut status, "state=blocked:id=blocked"),
            "only later lower-priority records"
        );
        assert!(report(&mut status, "state=blocked:id=new"));
        assert!(report(&mut status, "state=blocked:id=blocked"));
        assert_eq!(status.snapshot()[0].id, "blocked");
        assert_eq!(status.snapshot()[1].id, "new");
        assert!(!report(&mut status, "state=blocked:id=blocked"));
    }

    #[test]
    fn lru_eviction_is_by_update_not_priority_and_identical_reports_refresh_it() {
        let mut status = ProgramStatus::default();
        for index in 0..MAX_RECORDS {
            report(&mut status, &format!("state=blocked:id=r{index}"));
        }
        assert_eq!(status.records.len(), MAX_RECORDS);
        assert!(report(&mut status, "state=blocked:id=r0"));
        assert!(report(&mut status, "state=idle:id=new"));
        assert_eq!(status.records.len(), MAX_RECORDS);
        assert!(status.records.iter().any(|r| r.id == "r0"));
        assert!(!status.records.iter().any(|r| r.id == "r1"));
        // An invalid report cannot refresh the next eviction candidate.
        assert!(!report(&mut status, "state=blocked:id=r2:msg=YR"));
        assert!(report(&mut status, "state=done:id=newer"));
        assert!(!status.records.iter().any(|r| r.id == "r2"));
    }

    #[test]
    fn replay_restores_own_apps_unicode_and_recency() {
        let mut status = ProgramStatus::default();
        report(&mut status, "state=idle:app=root");
        report(&mut status, "state=working:id=parent:app=near:progress=100");
        // "café 🦀"
        assert!(report(
            &mut status,
            "state=blocked:id=parent/child:kind=question:msg=Y2Fmw6kg8J+mgA=="
        ));
        report(&mut status, "state=done:id=finished:title=:msg=");
        report(&mut status, "state=working:id=parent:app=near:progress=100");
        let mut restored = ProgramStatus::default();
        restore(&mut restored, &status.replay());
        assert_eq!(restored.records, status.records);
        assert_eq!(restored.snapshot(), status.snapshot());
        report(&mut restored, "state=working:id=parent");
        assert_eq!(
            record(&restored.snapshot(), "parent/child").app.as_deref(),
            Some("root")
        );
    }
}
