//! Session create, rename and kill, terminate, and focus between tabs and panes.

use super::*;

impl Desk {
    /// The focused pane's named session, or a refusal to `verb` it.
    pub(super) fn focused_session(&mut self, verb: &str) -> Option<(String, u32)> {
        let session = self
            .focused_pane()
            .map(|pane| (pane.session_name.clone(), pane.session_id))
            .filter(|(name, _)| !name.trim().is_empty());
        if session.is_none() {
            self.post(
                Kind::Refusal,
                "No session",
                Some(format!("Focus a pane in the session you want to {verb}.")),
            );
        }
        session
    }

    pub(super) fn open_rename_session(&mut self) {
        let Some((current, id)) = self.focused_session("rename") else {
            return;
        };
        let target = self.rename_session_target(id);
        self.modal = Modal::Session {
            create: false,
            current,
            target,
        };
    }

    /// The durable key of session `id` in the current topology.
    pub(crate) fn session_key(&self, id: u32) -> Option<SessionKey> {
        let topology = self.link.as_ref()?.connection.topology.as_ref()?;
        topology
            .sessions
            .iter()
            .find(|session| session.id == id)?
            .key
    }

    pub(crate) fn rename_session_target(&self, id: u32) -> Option<SessionRenameTarget> {
        let connection = &self.link.as_ref()?.connection;
        let session = connection
            .topology
            .as_ref()?
            .sessions
            .iter()
            .find(|session| session.id == id)?;
        Some(SessionRenameTarget {
            key: session.key,
            id,
            identity: connection.identity()?,
        })
    }

    pub(super) fn rename_active(&mut self) {
        if let Some(tab) = self.active_tab() {
            self.modal = Modal::Rename {
                tab_id: tab.id.clone(),
            };
        }
    }

    pub(super) fn ask_terminate(&mut self) {
        let Some(terminal) = self.focused_terminal() else {
            return;
        };
        let title = self.pane_title(Some(&terminal));
        self.modal = Modal::Terminate { terminal, title };
    }

    /// Send the held paste to the placement it was meant for.
    pub fn confirm_paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Modal::Paste {
            placement_id,
            terminal_id,
            paste,
            ..
        } = self.modal.clone()
        else {
            return;
        };
        self.close_modal(window, cx);
        let current = self
            .placement(&placement_id)
            .and_then(|placement| placement.terminal_id.as_ref());
        if !held_paste_target_holds(current, &terminal_id)
            || self.panes.get(&placement_id).is_none()
        {
            self.post(
                Kind::Refusal,
                crate::attach::PASTE_REFUSED,
                Some("That terminal is gone. Nothing was pasted.".into()),
            );
        } else if let Some(notice) =
            self.panes
                .request(&placement_id, HostAction::PasteConfirmed(paste), cx)
        {
            self.post(Kind::Refusal, notice.title, Some(notice.body));
        }
        self.after_command(window, cx);
    }

    pub fn confirm_terminate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Modal::Terminate { terminal, .. } = self.modal.clone() else {
            return;
        };
        self.close_modal(window, cx);
        self.workspace_call(|link| {
            link.workspace.terminate(&link.connection, &terminal);
        });
        self.after_command(window, cx);
    }

    pub fn apply_rename(&mut self, title: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Modal::Rename { tab_id } = self.modal.clone() {
            self.workspace_call(|link| link.workspace.rename_tab(&tab_id, &title));
        }
        self.close_modal(window, cx);
        self.after_command(window, cx);
    }

    /// Create or rename a session. A refusal before anything is sent keeps
    /// the dialog open with a notification. A create on an `ORGANIZATION` server is
    /// persistent and reports its outcome when the server answers.
    pub fn apply_session(
        &mut self,
        name: String,
        directory: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Modal::Session {
            create,
            current,
            target,
        } = self.modal.clone()
        else {
            return;
        };
        let directory = directory.unwrap_or_default();
        let failure = if let Some(submitted) =
            self.submit_organization_session(create, target.as_ref(), &name, &directory)
        {
            submitted
                .map(|operation| {
                    let pending = if create {
                        &mut self.creates
                    } else {
                        &mut self.renames
                    };
                    pending.insert(operation, name.trim().to_owned());
                })
                .err()
        } else if !create && !self.legacy_rename_matches(target.as_ref(), &current) {
            self.post(
                Kind::Error,
                "Session changed",
                Some("Reopen the rename dialog to select its current identity.".to_owned()),
            );
            cx.notify();
            return;
        } else {
            let failure = self.local_only().or_else(|| {
                if create {
                    host::create_session(&self.phux, &self.socket, &name, &directory)
                } else {
                    host::rename_session(&self.phux, &self.socket, &current, &name)
                }
            });
            if failure.is_none()
                && let Some(link) = self.link.as_ref()
            {
                let _ = link.connection.refresh_topology();
            }
            failure
        };
        if let Some(failure) = failure {
            self.post(Kind::Error, session_failure(create), Some(failure));
            cx.notify();
            return;
        }
        self.close_modal(window, cx);
        self.after_command(window, cx);
    }

    /// Submit through `ORGANIZATION`, whose outcome is reported when the
    /// server answers. `None` when the server does not have it.
    pub(super) fn submit_organization_session(
        &self,
        create: bool,
        target: Option<&SessionRenameTarget>,
        name: &str,
        directory: &str,
    ) -> Option<Result<OperationId, String>> {
        let link = self
            .link
            .as_ref()
            .filter(|link| link.connection.organization())?;
        Some(if create {
            match crate::workspace::create_session_error(name, directory) {
                Some(refused) => Err(refused.to_owned()),
                None => link
                    .connection
                    .create_session(name.trim(), directory.trim()),
            }
        } else {
            target
                .and_then(|target| target.key)
                .ok_or_else(|| {
                    "Session identity is unavailable. Reopen the rename dialog after refreshing."
                        .to_owned()
                })
                .and_then(|key| {
                    link.connection
                        .client
                        .rename_session(key, name.trim())
                        .map_err(|error| error.to_string())
                })
        })
    }

    pub(super) fn legacy_rename_matches(
        &self,
        target: Option<&SessionRenameTarget>,
        name: &str,
    ) -> bool {
        let (Some(target), Some(link)) = (target, self.link.as_ref()) else {
            return false;
        };
        link.connection.status == Status::Attached
            && link.connection.identity().as_ref() == Some(&target.identity)
            && link.connection.topology.as_ref().is_some_and(|topology| {
                topology.sessions.iter().any(|session| {
                    session.id == target.id && session.name == name && session.key == target.key
                })
            })
    }

    /// Link the project dialog's session into `project`, or unlink it with
    /// `None` or a blank name. A refusal keeps the dialog open.
    pub fn link_project(
        &mut self,
        project: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Modal::Project { host, session, .. } = self.modal.clone() else {
            return;
        };
        if host != self.remote {
            self.post(
                Kind::Refusal,
                "Session moved",
                Some("This window now shows another machine. Reopen Link to Project from the session.".into()),
            );
            self.close_modal(window, cx);
            return;
        }
        let project = project
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty());
        let submitted = match project.as_deref().and_then(project_refusal) {
            Some(refused) => Err(refused.to_owned()),
            None => self
                .link
                .as_ref()
                .filter(|link| link.connection.organization())
                .ok_or_else(|| "This server cannot link sessions to projects.".to_owned())
                .and_then(|link| {
                    link.connection
                        .client
                        .set_session_project(session, project.as_deref())
                        .map_err(|error| error.to_string())
                }),
        };
        if let Err(failure) = submitted {
            self.post(Kind::Error, "Could not link the project", Some(failure));
            cx.notify();
            return;
        }
        self.close_modal(window, cx);
        self.after_command(window, cx);
    }

    /// Ask to kill the focused pane's session.
    pub(super) fn ask_kill_session(&mut self) {
        let Some((name, session)) = self.focused_session("kill") else {
            return;
        };
        if self.intake_kill_refused(session) {
            return;
        }
        let target = self.rename_session_target(session);
        self.modal = Modal::KillSession {
            name,
            session,
            target,
        };
    }

    pub fn confirm_kill_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Modal::KillSession {
            name,
            session,
            target,
        } = self.modal.clone()
        else {
            return;
        };
        self.close_modal(window, cx);
        if !self.legacy_rename_matches(target.as_ref(), &name) {
            self.post(
                Kind::Refusal,
                "Session changed",
                Some("Reopen the session menu before killing it. Nothing was killed.".into()),
            );
            self.after_command(window, cx);
            return;
        }
        self.kill_session(&name, session, cx);
        self.after_command(window, cx);
    }

    /// Kill `session` with one `SESSION_KILL`; a server without it gets
    /// `phux kill`.
    pub(super) fn kill_session(&mut self, name: &str, session: u32, cx: &mut Context<Self>) {
        if self.intake_kill_refused(session) {
            return;
        }
        let Some(link) = self.link.as_ref() else {
            self.post(
                Kind::Error,
                "Could not kill the session",
                Some("Not connected.".into()),
            );
            return;
        };
        let supported = link.connection.server.as_ref().is_some_and(|server| {
            server.has_ext(phux_protocol::caps::ServerFeatureExt::SessionKill)
        });
        if !supported {
            self.kill_legacy_session(name, cx);
            return;
        }
        let Some(key) = self.session_key(session) else {
            self.post(
                Kind::Error,
                "Could not kill the session",
                Some("That session is gone. Refresh and try again.".into()),
            );
            return;
        };
        match link.connection.try_kill_session(key) {
            Ok(operation) => {
                self.kills.insert(operation, name.to_owned());
            }
            Err(failure) => self.post(Kind::Error, "Could not kill the session", Some(failure)),
        }
    }

    /// Older servers need the CLI, whose network wait must never hold the UI.
    fn kill_legacy_session(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(failure) = self.local_only() {
            self.post(Kind::Error, "Could not kill the session", Some(failure));
            return;
        }
        let name = name.to_owned();
        let phux = self.phux.clone();
        let socket = self.socket.clone();
        let killed = cx
            .background_executor()
            .spawn(async move { host::kill_session(&phux, &socket, &name) });
        cx.spawn(async move |desk, cx| {
            let failure = killed.await;
            let _ = desk.update(cx, |desk, cx| {
                if let Some(failure) = failure {
                    desk.post(Kind::Error, "Could not kill the session", Some(failure));
                } else if let Some(link) = &desk.link {
                    let _ = link.connection.refresh_topology();
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn intake_kill_refused(&mut self, session: u32) -> bool {
        if !self
            .link
            .as_ref()
            .is_some_and(|link| link.connection.is_intake_session(session))
        {
            return false;
        }
        self.post(Kind::Info, "Intake cannot be killed", Some("Move its work to a named session, or terminate individual processes. Intake is the designated home for unorganized work.".to_owned()));
        true
    }

    pub(super) fn move_active_tab(&mut self, step: i8) {
        let Some(link) = self.link.as_ref() else {
            return;
        };
        let tabs = &link.workspace.tabs;
        let len = tabs.len();
        let Some(from) = tabs
            .iter()
            .position(|tab| tab.id == link.workspace.active_id)
            .filter(|_| len > 1)
        else {
            return;
        };
        let to = (from as i32 + step as i32).rem_euclid(len as i32) as usize;
        self.workspace_call(|link| link.workspace.move_tab(from, to));
    }

    /// Reveal the agent that most needs the user: the most urgent state, then
    /// the latest change.
    pub(super) fn next_attention(&mut self) {
        let Some(link) = self.link.as_ref() else {
            return;
        };
        let working = crate::connection::state_rank(AgentState::Working);
        let next = link
            .connection
            .panes()
            .iter()
            .filter_map(|pane| {
                let agent = link.connection.agents.get(&pane.terminal_id)?;
                let rank = crate::connection::state_rank(agent.state);
                (rank <= working).then(|| {
                    let order = (rank, std::cmp::Reverse(agent.changed_at_ms));
                    (order, pane.terminal_id.clone())
                })
            })
            .min_by_key(|(order, _)| *order)
            .map(|(_, terminal)| terminal);
        match next {
            Some(terminal) => self.reveal_terminal(&terminal),
            None => self.post(Kind::Refusal, "No agent needs you", None),
        }
    }

    pub fn reveal_terminal(&mut self, terminal: &ResourceId) {
        let label = self
            .placement_of(terminal)
            .map(|placement| placement.terminal_label.clone());
        self.workspace_call(|link| {
            if let Some(pane) = link.connection.pane(terminal).cloned() {
                link.workspace.open(&link.connection, &pane);
            } else if let Some(label) = label {
                let _ = link.workspace.reveal(&label);
            }
        });
    }

    pub fn select_tab(&mut self, id: &str) {
        self.workspace_call(|link| link.workspace.select_tab(id));
    }

    pub fn close_tab(&mut self, id: &str) {
        self.workspace_call(|link| link.workspace.terminate_tab(&link.connection, id));
    }

    pub fn focus_placement(
        &mut self,
        tab_id: &str,
        placement_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspace_call(|link| link.workspace.focus_placement(tab_id, placement_id));
        self.after_command(window, cx);
    }

    /// Give the keyboard to whatever owns it: the open modal's control, the
    /// focused pane's find field, its terminal, or the window itself.
    pub(super) fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.modal, Modal::None) || self.changes.has_keys(window, cx) {
            return;
        }
        if self.text_field_has_keys()
            && let Some((input, _)) = self.find_input.as_ref()
        {
            let input = input.clone();
            input.update(cx, |input, cx| input.focus(window, cx));
            return;
        }
        if let Some(id) = self.focused_id()
            && self.focus_conversation(&id, window, cx)
        {
            return;
        }
        match self.focused_view().and(self.focused_id()) {
            Some(id) if self.panes.get(&id).is_some() => self.panes.focus(&id, window, cx),
            _ => window.focus(&self.focus, cx),
        }
    }
}
