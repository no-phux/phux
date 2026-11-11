//! "Mirror a screen or window…": ask every host (this one and a hub's
//! satellites) which displays and windows it can mirror (`MIRROR`), list
//! them, then start the picked one and open its surface. A Mac answers only
//! while Rune runs there (`docs/resource-kinds.md#standing-mirrors`).

use std::sync::{Arc, Mutex};

use phux_client_runtime::control::{MirrorCall, MirrorError};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::MirrorOp;

use crate::desk::{Desk, NoticeKind};
use crate::palette::{Hit, Item, Mode, Target};

/// What arrived from a worker waiting on a `MIRROR`.
enum Answer {
    Targets(Option<String>, Result<serde_json::Value, MirrorError>),
    Started(Option<String>, Result<serde_json::Value, MirrorError>),
}

/// The picker's rows and the answers still to come.
#[derive(Default)]
pub struct Screens {
    /// `(host, target, label, kind)`, hosts in the order asked.
    rows: Vec<(Option<String>, String, String, String)>,
    /// A host that could not answer, and why.
    failed: Vec<(Option<String>, String)>,
    waiting: usize,
    inbox: Arc<Mutex<Vec<Answer>>>,
}

impl Screens {
    /// What the palette says while it has no rows.
    pub fn empty_text(&self) -> &'static str {
        if self.waiting > 0 {
            "Asking each host for its screens…"
        } else {
            "No host offered a screen or window"
        }
    }
}

fn host_name(host: Option<&str>) -> &str {
    host.unwrap_or(crate::machines::LOCAL)
}

impl Desk {
    pub(in crate::desk) fn screens_item() -> Item {
        Item {
            title: "Mirror a screen or window…".to_owned(),
            detail: "each host's displays and windows".to_owned(),
            kind: "action",
            target: Target::Screens,
        }
    }

    /// Open the picker and ask every host.
    pub(in crate::desk) fn open_screens(&mut self) {
        self.open_palette(Mode::Screens);
        let Some(client) = self.client.clone() else {
            return;
        };
        let mut hosts: Vec<Option<String>> = vec![None];
        for session in self.topology.iter().flat_map(|t| &t.satellite_sessions) {
            if !hosts.contains(&Some(session.host.clone())) {
                hosts.push(Some(session.host.clone()));
            }
        }
        let screens = &mut self.resources.screens;
        screens.rows.clear();
        screens.failed.clear();
        for host in hosts {
            let call = client.with_control(|c| c.mirror(host.clone(), MirrorOp::Targets));
            self.wait_mirror(host, call, Answer::Targets);
        }
    }

    /// Start `target` on `host`; its surface opens once it answers.
    pub(in crate::desk) fn start_screen(&mut self, host: Option<String>, target: String) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let op = MirrorOp::Start {
            target: target.clone(),
            jpeg: false,
        };
        let call = client.with_control(|c| c.mirror(host.clone(), op));
        self.say(
            NoticeKind::Info,
            format!(
                "Starting the mirror of {target} on {}…",
                host_name(host.as_deref())
            ),
        );
        self.wait_mirror(host, call, Answer::Started);
    }

    /// Wait for `call` on a worker; its answer lands in the inbox.
    fn wait_mirror(
        &mut self,
        host: Option<String>,
        call: Result<MirrorCall, MirrorError>,
        wrap: fn(Option<String>, Result<serde_json::Value, MirrorError>) -> Answer,
    ) {
        let screens = &mut self.resources.screens;
        let inbox = screens.inbox.clone();
        screens.waiting += 1;
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let answer = call.and_then(MirrorCall::wait);
            if let Ok(mut inbox) = inbox.lock() {
                inbox.push(wrap(host, answer));
            }
            if let Some(waker) = waker {
                waker();
            }
        });
    }

    /// Fold in the workers' answers. True when anything changed.
    pub(in crate::desk) fn take_screens(&mut self) -> bool {
        let answers = match self.resources.screens.inbox.lock() {
            Ok(mut inbox) => std::mem::take(&mut *inbox),
            Err(_) => return false,
        };
        let changed = !answers.is_empty();
        for answer in answers {
            self.resources.screens.waiting = self.resources.screens.waiting.saturating_sub(1);
            match answer {
                Answer::Targets(host, Ok(doc)) => {
                    let rows = doc["targets"].as_array().cloned().unwrap_or_default();
                    for t in rows {
                        let text = |key: &str| t[key].as_str().unwrap_or_default().to_owned();
                        if !text("target").is_empty() {
                            let row = (host.clone(), text("target"), text("label"), text("kind"));
                            self.resources.screens.rows.push(row);
                        }
                    }
                }
                Answer::Targets(host, Err(why)) => {
                    self.resources.screens.failed.push((host, why.to_string()));
                }
                Answer::Started(host, result) => self.screen_started(host, result),
            }
        }
        changed
    }

    fn screen_started(
        &mut self,
        host: Option<String>,
        result: Result<serde_json::Value, MirrorError>,
    ) {
        let doc = match result {
            Ok(doc) => doc,
            Err(why) => return self.say(NoticeKind::Error, format!("No mirror: {why}")),
        };
        let session = doc["session"].as_str().unwrap_or_default().to_owned();
        let Some(id) = doc["surface"]
            .as_u64()
            .and_then(|id| u32::try_from(id).ok())
        else {
            return self.say(
                NoticeKind::Info,
                format!("The mirror runs in {session}; its first frame has not come yet. Open it from the palette once it shows."),
            );
        };
        let surface = match &host {
            Some(host) => ResourceId::satellite(host.clone(), id),
            None => ResourceId::local(id),
        };
        self.show(surface, &session);
    }

    /// The picker's rows for `query`, best first; a host that failed says
    /// why, and picking it asks again.
    pub(in crate::desk) fn screen_hits(&self, query: &str) -> Vec<Hit> {
        let screens = &self.resources.screens;
        let mut items: Vec<Item> = screens
            .rows
            .iter()
            .map(|(host, target, label, kind)| Item {
                title: if label.is_empty() {
                    target.clone()
                } else {
                    label.clone()
                },
                detail: format!("{} · {kind}", host_name(host.as_deref())),
                kind: "action",
                target: Target::Screen(host.clone(), target.clone()),
            })
            .collect();
        items.extend(screens.failed.iter().map(|(host, why)| Item {
            title: format!("{}: {why}", host_name(host.as_deref())),
            detail: "pick to ask again".to_owned(),
            kind: "action",
            target: Target::Screens,
        }));
        crate::palette::rank(items, query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_become_rows_and_failures_say_why() {
        let mut desk = Desk::new(&crate::launch::Resolved::offline("test"));
        let inbox = desk.resources.screens.inbox.clone();
        desk.resources.screens.waiting = 2;
        inbox.lock().unwrap().extend([
            Answer::Targets(
                None,
                Ok(serde_json::json!({ "targets": [
                    { "target": "display", "kind": "display", "label": "Main display" },
                    { "target": "Safari:Docs", "kind": "window", "label": "Safari: Docs" },
                ]})),
            ),
            Answer::Targets(
                Some("box".to_owned()),
                Err(MirrorError::Refused {
                    code: phux_protocol::wire::frame::ErrorCode::Unavailable,
                    message: "open Rune here".to_owned(),
                }),
            ),
        ]);
        assert!(desk.take_screens());
        assert_eq!(desk.resources.screens.waiting, 0);
        let hits = desk.screen_hits("safari");
        assert_eq!(
            hits[0].item.target,
            Target::Screen(None, "Safari:Docs".to_owned())
        );
        let all = desk.screen_hits("");
        assert_eq!(all.len(), 3);
        assert!(all.iter().any(
            |hit| hit.item.title == "box: open Rune here" && hit.item.target == Target::Screens
        ));
    }
}
