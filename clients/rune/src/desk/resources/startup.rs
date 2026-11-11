//! Starting a mirror is an explicit new process on the selected host, never
//! text typed into a shell. Its dedicated terminal shows permission/errors
//! and remains the producer's supervisor (Ctrl-C stops it).

use phux_client_runtime::control::SpawnRequest;

use crate::desk::{Desk, NoticeKind, Pending};
use crate::palette::{Hit, Item, Target};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Page,
    Tree,
    Surface,
}

impl Source {
    pub const fn verb(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Tree => "tree",
            Self::Surface => "surface",
        }
    }

    pub const fn prompt(self) -> &'static str {
        match self {
            Self::Page => "Page title/URL fragment or first (Chrome debugging port 9222)…",
            Self::Tree => "Running macOS application name (e.g. Safari)…",
            Self::Surface => "macOS App, App:title, or display…",
        }
    }

    pub const fn prerequisites(self) -> &'static str {
        match self {
            Self::Page => {
                "Start Chrome with remote debugging on localhost:9222 in a separate profile; open the page first."
            }
            Self::Tree => {
                "Open the app first; grant the producer Accessibility access in System Settings → Privacy & Security."
            }
            Self::Surface => {
                "Open the window first; grant Screen Recording access in System Settings → Privacy & Security."
            }
        }
    }

    fn supported(self, remote: bool, macos: bool) -> Result<(), &'static str> {
        if remote {
            return Err(
                "Mirror startup needs a local selected host. Switch to This Computer, or run `phux agent page/tree/surface mirror` on the source host; reopen its resource here. This does not migrate the application.",
            );
        }
        if self != Self::Page && !macos {
            return Err(
                "App/tree and surface capture require macOS. Run the producer on the Mac displaying the app, then open its resource here.",
            );
        }
        Ok(())
    }
}

fn argv(
    phux: &std::path::Path,
    socket: &std::path::Path,
    source: Source,
    target: &str,
) -> Result<Vec<String>, String> {
    let path = |p: &std::path::Path| {
        p.to_str()
            .map(str::to_owned)
            .ok_or_else(|| "CLI/socket path is not UTF-8".to_owned())
    };
    Ok(vec![
        path(phux)?,
        "--socket".to_owned(),
        path(socket)?,
        "agent".to_owned(),
        source.verb().to_owned(),
        "mirror".to_owned(),
        "--".to_owned(),
        target.to_owned(),
    ])
}

impl Desk {
    pub(in crate::desk) fn resource_items(&self) -> Vec<Item> {
        let mut items = self.document_items();
        items.push(Self::screens_item());
        items.extend(
            [Source::Page, Source::Tree, Source::Surface].map(|source| Item {
                title: format!("Start {} mirror…", source.verb()),
                detail: "new supervised producer terminal · read-only".to_owned(),
                kind: "action",
                target: Target::Mirror(source),
            }),
        );
        items
    }

    fn document_items(&self) -> Vec<Item> {
        let Some(client) = &self.client else {
            return Vec::new();
        };
        let org = client.organization();
        let mut items: Vec<_> = self
            .resources
            .entries
            .iter()
            .filter_map(|(id, doc)| {
                // A satellite's document has no session here: its host names it.
                let session = match id.host() {
                    Some(host) => host.as_str().to_owned(),
                    None => org.session(&doc.record.session)?.name.clone(),
                };
                Some(Item {
                    title: doc
                        .record
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("{:?}", doc.record.kind)),
                    detail: format!("{session} · {id} · {}", self.mirror_label(id)),
                    kind: "document",
                    target: Target::Terminal {
                        id: id.clone(),
                        session,
                    },
                })
            })
            .collect();
        items.sort_by(|a, b| a.detail.cmp(&b.detail));
        items
    }

    pub(in crate::desk) fn mirror_items(&self, source: Source, query: &str) -> Vec<Hit> {
        let target = query.trim();
        if target.is_empty() {
            return Vec::new();
        }
        vec![Hit {
            item: Item {
                title: format!("Start {} mirror of {target:?}", source.verb()),
                detail: source.prerequisites().to_owned(),
                kind: "action",
                target: Target::StartMirror(source, target.to_owned()),
            },
            score: 0,
            matched: Vec::new(),
        }]
    }

    pub(in crate::desk) fn ask_mirror(&mut self, source: Source, target: String) {
        if let Err(why) = self.mirror_argv(source, &target) {
            return self.say(NoticeKind::Error, why);
        }
        self.pending = Some(Pending::Mirror(source, target));
    }

    fn mirror_argv(&self, source: Source, target: &str) -> Result<Vec<String>, String> {
        source
            .supported(self.machines.current.is_some(), cfg!(target_os = "macos"))
            .map_err(str::to_owned)?;
        if target.trim().is_empty() || target.contains('\0') {
            return Err("Name an existing page, app, or window.".to_owned());
        }
        let phux =
            self.launch.phux.as_deref().ok_or(
                "Select a phux CLI with --phux PATH; it must support agent mirror commands.",
            )?;
        let socket = self
            .launch
            .socket
            .as_ref()
            .map_err(|_| "Connect to the local host before starting a producer.")?;
        if self.status != phux_client_runtime::control::Status::Attached {
            return Err("Connect to the local host before starting a producer.".to_owned());
        }
        if self
            .client
            .as_ref()
            .and_then(|c| c.selected_session())
            .is_none()
        {
            return Err(
                "Create or select a session first; the producer needs its own terminal.".to_owned(),
            );
        }
        argv(phux, socket, source, target)
    }

    pub(in crate::desk) fn start_mirror(&mut self, source: Source, target: &str) {
        let command = match self.mirror_argv(source, target) {
            Ok(argv) => argv,
            Err(why) => return self.say(NoticeKind::Error, why),
        };
        let Some(client) = &self.client else { return };
        let session_id = client.selected_session();
        let request = client.with_control(|control| {
            control.spawn_terminal_once(SpawnRequest {
                command: Some(command),
                session_id,
                // Keep errors visible even in a document-only session.
                initial_size: Some((100, 28)),
                ..SpawnRequest::default()
            })
        });
        self.pending_splits.insert(request, None);
        self.resources.starting.insert(request, source);
        self.say(NoticeKind::Info, "Starting a dedicated producer terminal. Its output shows prerequisites and failures; Ctrl-C there stops the mirror. Open the published document from the palette.");
    }

    pub(in crate::desk) fn producer_ended(
        &mut self,
        id: &phux_protocol::ResourceId,
        exit: Option<i32>,
    ) -> bool {
        let Some(source) = self.resources.producers.remove(id) else {
            return false;
        };
        let status = exit.map_or_else(
            || "disconnected or stopped".to_owned(),
            |code| format!("exit {code}"),
        );
        self.say(NoticeKind::Error, format!("{} mirror producer ended ({status}). {} Start a new mirror from the palette when ready; it is never restarted automatically.", source.verb(), source.prerequisites()));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_preserves_untrusted_target_as_one_argument_and_pins_host_socket() {
        let args = argv(
            "/dev/phux".as_ref(),
            "/dev/socket".as_ref(),
            Source::Page,
            "page; touch /tmp/no",
        )
        .unwrap();
        assert_eq!(
            args,
            [
                "/dev/phux",
                "--socket",
                "/dev/socket",
                "agent",
                "page",
                "mirror",
                "--",
                "page; touch /tmp/no"
            ]
        );
    }

    #[test]
    fn unsupported_hosts_are_actionable_not_guessed() {
        assert!(Source::Page.supported(false, false).is_ok());
        assert!(
            Source::Tree
                .supported(false, false)
                .unwrap_err()
                .contains("macOS")
        );
        assert!(Source::Surface.supported(false, true).is_ok());
        for source in [Source::Page, Source::Tree, Source::Surface] {
            assert!(
                source
                    .supported(true, true)
                    .unwrap_err()
                    .contains("source host")
            );
        }
    }
}
