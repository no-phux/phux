//! Draining connection activity into layout, agents, commands and notices.

use super::*;

impl Desk {
    pub(super) fn on_activity(&mut self, cx: &mut Context<Self>) {
        let _drain = crate::diag::DRAIN.timer();
        crate::diag::DRAIN_WAKES.incr();
        let (events, answers, uploads, epoch, agents_noted) = {
            let Some(link) = self.link.as_mut() else {
                return;
            };
            (
                link.connection.drain(),
                link.connection.take_path_answers(),
                link.connection.take_file_upload_receipts(),
                link.connection.connection_epoch(),
                link.connection.agents_noted(),
            )
        };
        crate::diag::DRAIN_EVENTS.add(events.len() as u64);
        let had_uploads = !uploads.is_empty();
        if had_uploads {
            self.finish_uploads(uploads);
        }
        let conversation_changed = self.conversation_receipts(&events);
        let chrome = self.read_runtime_chrome();
        let chrome_changed = chrome != self.runtime_chrome;
        self.runtime_chrome = chrome;
        // Notify cached terminals even for mixed agent/PTY batches, but keep
        // the last frame while disconnected instead of reacquiring a view.
        if self.session_is_attached() {
            let terminals = events
                .iter()
                .filter_map(|event| match event {
                    Event::TerminalChanged { terminal_id } => Some(terminal_id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            self.panes.notify_terminals(&terminals, cx);
        }
        // A spawn's bootstrap can arrive as a pure paint batch. Promote only
        // output that can actually settle an awaiting placement.
        let awaiting_output = self
            .link
            .as_ref()
            .is_some_and(|link| link.workspace.awaiting_output(&events));
        if answers.is_empty()
            && paint_batch(&events, agents_noted)
            && !conversation_changed
            && !awaiting_output
        {
            if had_uploads || chrome_changed {
                cx.notify();
            }
            return;
        }
        self.apply_activity(&events, answers, epoch, cx);
    }

    pub(super) fn apply_activity(
        &mut self,
        events: &[Event],
        answers: Vec<PathAnswer>,
        epoch: u64,
        cx: &mut Context<Self>,
    ) {
        self.adopt_session_layout(cx);
        self.sync_home_session(cx);
        self.note_activity(events, epoch);
        // A reexec resets layout placements, not the ACP owner or its editor.
        // Reopen its guardian before waiting for the exact fresh activation.
        self.remember_native_focus();
        if let Some(link) = self.link.as_mut() {
            link.workspace.handle(events, &link.connection);
            link.workspace.settle(&link.connection);
        }
        self.capture_receipts(events, cx);
        self.settle_project_terminal();
        self.settle_fleet_terminal();
        self.reveal_pending_native(cx);
        self.take_notices();
        self.note_agents();
        self.note_approvals(cx);
        self.note_approval_outcomes();
        self.flush_system(cx);
        for answer in answers {
            self.picker = paths::accept_path_answer(self.picker.take(), &answer);
        }
        self.retain_panes();
        if self
            .find
            .as_ref()
            .is_some_and(|find| self.placement(&find.placement_id).is_none())
        {
            self.close_find(cx);
        }
        if self.modal == Modal::Path
            && self.picker.as_ref().is_some_and(|active| {
                !self.session_is_attached()
                    || !paths::same_target(&active.target, self.path_target().as_ref())
            })
        {
            self.close_path_picker();
        }
        self.persist_if_changed();
        self.ensure_clock(cx);
        self.follow_changes(cx);
        if events.iter().any(crate::connection::session_attached) {
            self.panes.notify_all(cx);
        }
        cx.notify();
    }

    pub(super) fn note_activity(&mut self, events: &[Event], epoch: u64) {
        if self.agent_epoch != Some(epoch)
            || events.iter().any(crate::connection::starts_connection)
        {
            // Command starts survive a reconnect: a build that spans one
            // still alerts when it finishes, and a newer start replaces them.
            self.agent_seen.clear();
            self.feed.retain(|entry| entry.kind != Kind::Agent);
            self.agent_epoch = Some(epoch);
        }
        for event in events {
            if crate::connection::starts_connection(event) {
                self.organization.connection_changed();
            }
            self.note_event(event);
        }
    }

    pub(super) fn settle_project_terminal(&mut self) {
        if self
            .pending_project_terminal
            .as_ref()
            .is_some_and(|terminal| self.terminal_reveal_ready(terminal))
            && let Some(terminal) = self.pending_project_terminal.take()
        {
            self.reveal_terminal(&terminal);
        }
    }

    pub(super) fn note_event(&mut self, event: &Event) {
        self.note_terminal_event(event);
        self.note_command_event(event);
        if let Event::OrganizationOperation { operation, outcome } = event {
            self.note_operation(*operation, outcome);
        }
        self.note_input_event(event);
        if let Event::Detached { reason, message } = event {
            self.post(
                Kind::Error,
                "Detached",
                Some(detach_detail(*reason, message)),
            );
        }
    }

    pub(super) fn sync_home_session(&mut self, cx: &mut Context<Self>) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        let name = link.connection.session_name().to_owned();
        if let Some(topology) = &link.connection.topology {
            let desk = cx.entity_id();
            let local = self.remote.is_none();
            self.desktop.update(cx, |desktop, _| {
                if local {
                    desktop.observe_topology(topology);
                }
                desktop.sync_window_session(desk, &name, self.remote.as_deref(), self.role);
            });
        }
        if self.session == name {
            return;
        }
        self.session = name.clone();
        link.workspace.set_home_session(name.clone());
    }

    pub(super) fn adopt_session_layout(&mut self, cx: &mut Context<Self>) {
        if !self.stable_layout || self.role == WindowRole::Fresh {
            return;
        }
        let Some(key) = self.link_home_key() else {
            return;
        };
        if self.layout_key == Some(key) {
            return;
        }
        self.layout_key = Some(key);
        let path = self.work_layout_path(self.remote.as_deref(), &self.session, Some(key));
        // A preferences edit can still be writing the legacy source. Adoption
        // must not restore older bytes over the snapshot we just queued.
        self.flush_saves();
        let missing = matches!(
            FileLayoutStore::new(&path, None).read(),
            persist::StoreValue::Missing
        );
        let store = FileLayoutStore::new(path, Some(self.store.path.clone()));
        let loaded = persist::load_layout(&store.read());
        self.layout_blocked |= loaded.blocked;
        self.persist_writes &= !loaded.blocked;
        self.layout_migration_pending = missing && !self.layout_blocked;
        self.store = store;
        if loaded.blocked {
            self.post(
                Kind::Error,
                "Session layout is unreadable",
                Some("The saved file is preserved; layout writes are disabled.".to_owned()),
            );
            return;
        }
        if let Some(saved) = loaded.initial {
            // The initial session seeds this window's display preferences.
            // Once navigation has saved a previous location, destination
            // layouts restore tabs/splits only: old per-session chrome must
            // never overwrite an explicit host filter, grouping or placement.
            if self.locations.is_empty() {
                self.prefs = if self.role == WindowRole::Quick {
                    persist::quick_display(Some(&self.prefs), Some(&saved))
                } else {
                    saved.display.clone()
                };
            }
            self.last_saved = Some(saved.clone());
            self.workspace_call(|link| link.workspace.restore(Some(saved)));
            self.sync_bindings(cx);
            self.sync_skin(false, cx);
            self.apply_kit_theme(cx);
        }
    }

    pub(super) fn note_operation(
        &mut self,
        operation: OperationId,
        outcome: &phux_client_core::org_ops::OrgOutcome,
    ) {
        self.note_organization(operation, outcome);
        let sentence = || crate::connection::org::outcome_sentence(outcome);
        if let Some(name) = self.kills.remove(&operation)
            && !matches!(outcome, phux_client_core::org_ops::OrgOutcome::Committed(_))
        {
            self.post(
                Kind::Error,
                &format!("Could not kill {name}"),
                Some(sentence()),
            );
        }
        if let Some(name) = self.creates.remove(&operation)
            && !outcome.succeeded()
        {
            let body = format!("{name}: {}", sentence());
            self.post(Kind::Error, session_failure(true), Some(body));
        }
        if let Some(name) = self.renames.remove(&operation) {
            if outcome.succeeded() {
                self.post(Kind::Info, "Session renamed", Some(name));
            } else {
                self.post(Kind::Error, session_failure(false), Some(sentence()));
            }
        }
    }

    pub(super) fn note_terminal_event(&mut self, event: &Event) {
        if let Event::TerminalClosed {
            terminal_id,
            exit_status,
            signal,
            ..
        } = event
        {
            let views = self
                .link
                .as_ref()
                .map(|link| link.workspace.views_of(terminal_id))
                .unwrap_or(0);
            if views > 0 {
                let status = exit_status
                    .map(|code| format!("exit {code}"))
                    .or_else(|| signal.map(|code| format!("signal {code}")))
                    .unwrap_or_else(|| "closed".into());
                let title = self.pane_title(Some(terminal_id));
                self.post(Kind::Info, &format!("{title} ended"), Some(status));
            }
            self.agent_seen.remove(terminal_id);
            self.commands.forget(terminal_id);
            self.feed.retain(|entry| {
                !matches!(entry.kind, Kind::Agent | Kind::Command)
                    || entry.terminal.as_ref() != Some(terminal_id)
            });
        }
    }

    /// A long shell command that finishes in a pane the person is not
    /// looking at asks for attention, as a finished agent does.
    pub(super) fn note_command_event(&mut self, event: &Event) {
        match event {
            Event::CommandStarted { terminal_id } => {
                self.commands.started(terminal_id.clone(), Instant::now());
            }
            Event::CommandFinished {
                terminal_id,
                exit_code,
            } => {
                let focused = self.focused_terminal().as_ref() == Some(terminal_id);
                let Some(ran) = self.commands.finished(terminal_id, Instant::now(), focused) else {
                    return;
                };
                if !self.prefs.notifications {
                    return;
                }
                let title = self.pane_title(Some(terminal_id));
                let (title, outcome) = match exit_code {
                    Some(0) | None => (format!("{title} finished"), "done".to_owned()),
                    Some(code) => (format!("{title} failed"), format!("exit {code}")),
                };
                let body = format!(
                    "{outcome} after {} · {}",
                    command_span(ran),
                    short_path(self.pane_cwd(Some(terminal_id)))
                );
                self.feed.post_command(title, body, terminal_id.clone());
            }
            // An exited shell finishes nothing it started.
            Event::Exited { terminal_id, .. } => self.commands.forget(terminal_id),
            _ => {}
        }
    }

    pub(super) fn note_input_event(&mut self, event: &Event) {
        let message = crate::connection::refused_input(event).or_else(|| match event {
            Event::InputDelivery {
                outcome: DeliveryOutcome::Refused,
                message,
                ..
            } if !message.is_empty() => Some(message.clone()),
            _ => None,
        });
        if let Some(message) = message {
            self.post(Kind::Error, "Input not delivered", Some(message));
        }
    }

    pub(super) fn note_agents(&mut self) {
        let Some(link) = self.link.as_ref() else {
            return;
        };
        let focused = link
            .workspace
            .focused()
            .and_then(|placement| placement.terminal_id.clone());
        let current = link.connection.agents.clone();
        // An agent entry stands while its agent still reports the state it announced.
        self.feed.retain(|entry| {
            entry.kind != Kind::Agent
                || entry
                    .terminal
                    .as_ref()
                    .and_then(|id| current.get(id))
                    .is_some_and(|info| {
                        entry.agent_state == Some(info.state) && agent_attention(info).actionable()
                    })
        });
        let notify = self.prefs.notifications;
        for (id, info) in &current {
            let next = agent_attention(info);
            if !notify || !next.should_alert(self.agent_seen.get(id), focused.as_ref() == Some(id))
            {
                continue;
            }
            let title = match info.word() {
                "" => format!("{} needs attention", info.name),
                word => format!("{} {word}", info.name),
            };
            let body = format!(
                "{} · {}",
                self.pane_title(Some(id)),
                short_path(self.pane_cwd(Some(id)))
            );
            self.feed.post_agent(title, body, id.clone(), info.state);
        }
        self.agent_seen = current
            .into_iter()
            .map(|(id, info)| (id, agent_attention(&info)))
            .collect();
    }

    pub(super) fn take_notices(&mut self) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        for notice in link.workspace.take_notices() {
            let kind = if notice.kind == "error" {
                Kind::Error
            } else {
                Kind::Info
            };
            self.post(kind, &notice.title, notice.body.clone());
        }
    }

    /// A terminal reports a click: focus its placement.
    pub(crate) fn on_terminal_event(
        &mut self,
        placement_id: &str,
        event: &crate::terminal::TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            crate::terminal::TerminalEvent::Activated => {
                if let Some(tab) = self.active_tab_id() {
                    self.focus_placement(&tab, placement_id, window, cx);
                }
            }
            crate::terminal::TerminalEvent::Notice { title, body } => {
                self.post(Kind::Refusal, title, Some(body.clone()));
            }
            crate::terminal::TerminalEvent::Upload(batch) => match self
                .placement(placement_id)
                .and_then(|placement| placement.terminal_id.clone())
            {
                Some(terminal) => self.start_upload(&terminal, batch.clone(), cx),
                None => self.refuse_paste("That terminal is gone. Nothing was pasted."),
            },
            crate::terminal::TerminalEvent::ConfirmPaste(paste) => {
                let Some(terminal_id) = self
                    .placement(placement_id)
                    .and_then(|placement| placement.terminal_id.clone())
                else {
                    self.refuse_paste("That terminal is gone. Nothing was pasted.");
                    cx.notify();
                    return;
                };
                self.modal = Modal::Paste {
                    placement_id: placement_id.to_owned(),
                    title: self.pane_title(Some(&terminal_id)),
                    terminal_id,
                    paste: paste.clone(),
                };
                self.after_command(window, cx);
            }
        }
        cx.notify();
    }
}
