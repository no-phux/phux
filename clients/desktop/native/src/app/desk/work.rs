//! Projects, windows, hosts and the work a window shows.

use super::*;

impl Desk {
    pub(super) fn move_to_window(&mut self, cx: &mut Context<Self>) {
        let (Some(id), Some(terminal)) = (self.focused_id(), self.focused_terminal()) else {
            return;
        };
        let session = self.session.clone();
        self.workspace_call(|link| link.workspace.close_pane(&link.connection, Some(&id)));
        let remote = self.remote.clone();
        self.desktop.update(cx, |desktop, cx| {
            desktop.open_moved(session, terminal, remote, cx);
        });
    }

    /// Move the focused placement into the next open window on the same
    /// server. Only this window's view is released; the terminal (same
    /// `ResourceId`, same process) is shown by the target's own client.
    pub(super) fn move_to_next_window(&mut self, cx: &mut Context<Self>) {
        let Some(terminal) = self
            .focused_placement()
            .and_then(|placement| placement.terminal_id.clone())
        else {
            return;
        };
        let target = self
            .desktop
            .read(cx)
            .next_window(cx.entity_id(), self.remote.as_deref(), cx);
        let Some((handle, desk)) = target else {
            // Refuse before releasing anything: a target that is not attached
            // could never reveal the terminal, which would then have no view
            // in either window.
            self.post(
                Kind::Refusal,
                "No other connected window",
                Some(
                    "Open or reconnect another window on this server, or use Move Pane to New Window."
                        .into(),
                ),
            );
            cx.notify();
            return;
        };
        let placement = self
            .focused_placement()
            .map(|placement| placement.id.clone());
        self.workspace_call(|link| {
            link.workspace
                .close_pane(&link.connection, placement.as_deref())
        });
        cx.notify();
        // Deferred: the target is another window, updated outside this one.
        cx.defer(move |cx| {
            cx.activate(true);
            let _ = handle.update(cx, |_, window, _| window.activate_window());
            desk.update(cx, |desk, cx| desk.show_project_terminal(terminal, cx));
        });
    }

    pub(super) fn open_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let desk = cx.entity().downgrade();
        let prompt = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |_, cx| {
            let paths = prompt
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .unwrap_or_default();
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = desk.update_in(cx, |desk, window, cx| {
                desk.open_project_path(path, window, cx);
            });
        })
        .detach();
    }

    /// A selected folder and a sidebar project use the same CLI/trust path.
    pub fn open_project_path(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(failure) = self.local_only() {
            self.post(Kind::Refusal, "Could not open folder", Some(failure));
            cx.notify();
            return;
        }
        let desk = cx.entity().downgrade();
        let phux = self.phux.clone();
        let socket = self.socket.clone();
        let navigation = self.upload_epoch;
        cx.spawn_in(window, async move |_, cx| {
            // Validation, trust checks and recipe launches can block.
            let result = smol::unblock(move || host::open_project(&phux, &socket, &path)).await;
            let _ = desk.update(cx, |desk, cx| match result {
                Ok(opened) => {
                    if let Err(error) = desk.desktop.update(cx, |desktop, cx| {
                        desktop.record_recent_project(opened.path, cx)
                    }) {
                        desk.post(
                            Kind::Error,
                            "Could not save recent folder",
                            Some(error.to_string()),
                        );
                    }
                    if desk.upload_epoch == navigation {
                        desk.open_project_window(opened.session, opened.terminal, cx);
                    } else {
                        desk.post(Kind::Info, "Project ready on Local", Some(opened.session));
                        cx.notify();
                    }
                }
                Err(error) => {
                    desk.post(Kind::Error, "Could not open folder", Some(error));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn forget_recent_project(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if let Err(error) = self
            .desktop
            .update(cx, |desktop, cx| desktop.forget_recent_project(path, cx))
        {
            self.post(
                Kind::Error,
                "Could not forget folder",
                Some(error.to_string()),
            );
        }
        cx.notify();
    }

    /// Whether a terminal moved here from another window would be revealed:
    /// attached, and not already waiting to reveal another one (which a new
    /// move would silently replace).
    pub(crate) fn accepts_moved_terminal(&self) -> bool {
        self.pending_project_terminal.is_none()
            && self
                .link
                .as_ref()
                .is_some_and(|link| link.connection.status == Status::Attached)
    }

    pub(crate) fn show_project_terminal(&mut self, terminal: ResourceId, cx: &mut Context<Self>) {
        if self
            .link
            .as_ref()
            .is_some_and(|link| link.connection.pane(&terminal).is_some())
        {
            self.pending_project_terminal = None;
            self.reveal_terminal(&terminal);
        } else {
            self.pending_project_terminal = Some(terminal);
            if let Some(link) = self.link.as_ref() {
                let _ = link.connection.refresh_topology();
            }
        }
        cx.notify();
    }

    pub fn open_project_window(
        &mut self,
        session: String,
        terminal: ResourceId,
        cx: &mut Context<Self>,
    ) {
        if self.remote.is_some() || self.session != session {
            let key = self
                .work_catalog()
                .into_iter()
                .find(|work| work.destination.host.is_none() && work.destination.name == session)
                .and_then(|work| work.destination.key);
            self.adopt_work(None, session, key, None, cx);
        }
        self.show_project_terminal(terminal, cx);
    }

    /// A window on the focused pane's session; a choice only when there is one.
    pub(super) fn new_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sessions = self
            .link
            .as_ref()
            .and_then(|link| link.connection.topology.as_ref())
            .map_or(0, |topology| topology.sessions.len());
        if sessions > 1 {
            self.modal = Modal::WindowSession;
            return;
        }
        let name = self
            .focused_pane()
            .map(|pane| pane.session_name.clone())
            .unwrap_or_default();
        self.open_session_window(name, window, cx);
    }

    pub fn open_session_window(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_modal(window, cx);
        let session = if name.trim().is_empty() {
            self.session.clone()
        } else {
            name
        };
        let remote = self.remote.clone();
        self.desktop
            .update(cx, |desktop, cx| desktop.open_session(session, remote, cx));
        cx.notify();
    }

    /// Re-read the remote registry on the background executor, which a test
    /// scheduler drives like any other window-open work.
    pub(crate) fn refresh_hosts(&mut self, cx: &mut Context<Self>) {
        let phux = self.phux.clone();
        let listed = cx
            .background_executor()
            .spawn(async move { host::list_hosts(&phux) });
        cx.spawn(async move |desk, cx| {
            let result = listed.await;
            let _ = desk.update(cx, |desk, cx| {
                match result {
                    Ok(hosts) => {
                        desk.hosts = hosts;
                        desk.sync_fleet(cx);
                    }
                    // No CLI is already explained at launch; the section just stays empty.
                    Err(error) => log::warn!("host list: {error}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn work_layout_path(
        &self,
        host: Option<&str>,
        session: &str,
        key: Option<SessionKey>,
    ) -> PathBuf {
        let authority = host.map_or_else(
            || self.socket.display().to_string(),
            |host| format!("host:{host}"),
        );
        let paths = match key {
            Some(key) => persist::session_layout_paths(&authority, key),
            None => crate::workspace::layout_paths(&authority, session),
        };
        if self.role == WindowRole::Quick {
            paths.quick
        } else {
            paths.layout
        }
    }

    pub(in crate::app) fn adopt_work(
        &mut self,
        name: Option<String>,
        session: String,
        key: Option<SessionKey>,
        authority: Option<phux_protocol::ids::AuthorityId>,
        cx: &mut Context<Self>,
    ) {
        let previous = self.work_layout_path(
            self.remote.as_deref(),
            &self.session,
            self.link_home_key().or(self.home_key),
        );
        self.locations.insert(previous, self.store.clone());
        self.persist();
        self.abandon_organize();
        self.release_link(true);
        self.link = None;
        self.pending_project_terminal = None;
        self.connect_error = None;
        self.goto_search = Default::default();
        self.remote = name.clone();
        self.session = session.clone();
        if self.role == WindowRole::Fresh {
            self.role = WindowRole::Main;
        }
        let id = cx.entity_id();
        self.desktop.update(cx, |desktop, _| {
            desktop.sync_window_session(id, &session, name.as_deref(), self.role)
        });
        self.home_key = key;
        self.expected_authority = authority;
        self.modal = Modal::None;
        self.prepared = Modal::None;
        self.modal_inputs.clear();
        self.picker = None;
        self.organization = Default::default();
        self.creates.clear();
        self.renames.clear();
        self.kills.clear();
        self.layout_key = None;
        self.last_saved = None;
        self.open_fresh = false;
        let path = self.work_layout_path(name.as_deref(), &session, key);
        // In particular A -> B -> A must see A's latest queued layout. Disk
        // waits are confined to this read-after-write boundary and close/quit.
        self.flush_saves();
        self.store = self
            .locations
            .get(&path)
            .cloned()
            .unwrap_or_else(|| FileLayoutStore::new(path, None));
        let loaded = persist::load_layout(&self.store.read());
        self.layout_blocked = loaded.blocked;
        self.persist_writes = self.role != WindowRole::Fresh && !loaded.blocked;
        self.layout_migration_pending = false;
        self.last_saved = loaded.initial;
        self.attach(None, cx);
        self.sync_fleet(cx);
        self.sync_bindings(cx);
        self.apply_kit_theme(cx);
        cx.notify();
    }

    /// Enroll `target` over ssh. The dialog closes at once; enrollment runs in
    /// the background and reports through a notification.
    pub(crate) fn apply_add_host(
        &mut self,
        target: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = target.trim().to_owned();
        if target.is_empty() {
            self.post(
                Kind::Refusal,
                "Could not add host",
                Some("Enter an ssh destination such as me@mini.".into()),
            );
            cx.notify();
            return;
        }
        if self.host_adding.is_some() {
            return;
        }
        self.close_modal(window, cx);
        self.host_adding = Some(target.clone());
        let phux = self.phux.clone();
        cx.spawn(async move |desk, cx| {
            let added = target.clone();
            let result = smol::unblock(move || host::add_host(&phux, &target, &name)).await;
            let _ = desk.update(cx, |desk, cx| {
                desk.host_adding = None;
                match result {
                    Ok(()) => desk.post(Kind::Info, &format!("Added {added}"), None),
                    Err(error) => {
                        desk.post(Kind::Error, &format!("Could not add {added}"), Some(error))
                    }
                }
                desk.refresh_hosts(cx);
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn confirm_remove_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Modal::RemoveHost { name } = self.modal.clone() else {
            return;
        };
        self.close_modal(window, cx);
        match host::remove_host(&self.phux, &name) {
            Ok(()) => self.hosts.retain(|host| host.name != name),
            Err(error) => self.post(
                Kind::Error,
                &format!("Could not remove {name}"),
                Some(error),
            ),
        }
        self.refresh_hosts(cx);
    }

    /// Why a CLI-backed action cannot run here: the CLI drives the local
    /// server's socket, not a registered host.
    pub(super) fn local_only(&self) -> Option<String> {
        self.remote.as_deref().map(|host| {
            format!("This window is on {host}. Run it there with `phux attach {host}`.")
        })
    }
}
